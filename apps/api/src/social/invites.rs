//! Game invites (05-social §5, 04-database §3, A4-T06).
//!
//! ```text
//! pending ──accept──▶ accepted ──▶ installing ──▶ ready ──▶ joined
//!    │                   │  └───────────────────────▶ ready
//!    ├─decline─▶ declined   (any active) ──cancel (sender)──▶ cancelled
//!    └─(time)──▶ expired    (any active) ──(time)──▶ expired
//!                           accepted | installing ──failed (invitee, reason)──▶ failed
//! ```
//!
//! - Only accepted friends without a block can invite each other, to a published package
//!   with at least one release; one active invite per (sender, invitee, package), a second
//!   create returns it. 60 creates per hour per user.
//! - Every transition is one guarded `UPDATE … WHERE state = ANY(allowed)`, so racing
//!   requests (accept vs. cancel) have exactly one winner; the loser gets `409`.
//! - Pending invites live 10 minutes; accepting gives them 24 hours. Expired ones are marked
//!   by `social.sweep` (and lazily before any read or change).
//! - Installing progress is always stored but published at most every 2 s per invite;
//!   state changes always publish `invite.updated` to both parties.
//! - The server never sees join secrets: they travel over Olm.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
};
use sqlx::PgConnection;
use time::Duration;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    realtime::{InviteEvent, kinds},
    social::{
        Invite, InviteCreate, InviteFailure, InviteList, InviteReport, InviteState,
        InviteStatusUpdate,
    },
};

use super::{events, relations};
use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    http::{
        json::{Json, JsonResponse, Validate, invalid},
        ratelimit::Policy,
    },
    openapi_problems::{BadRequest, Conflict, Forbidden, NotFound, TooManyRequests, Unauthorized},
    state::AppState,
};

/// How long a pending invite waits for an answer.
pub const PENDING_TTL: Duration = Duration::minutes(10);
/// How long an accepted invite may take to get to `joined`.
pub const ACTIVE_TTL: Duration = Duration::hours(24);
/// Invites finished longer ago than this drop out of `GET /v1/invites`.
const LIST_RECENT: Duration = Duration::hours(24);

/// States in which an invite is still live.
pub const ACTIVE_STATES: [&str; 4] = ["pending", "accepted", "installing", "ready"];

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_invites, create_invite))
        .routes(routes!(accept_invite))
        .routes(routes!(decline_invite))
        .routes(routes!(cancel_invite))
        .routes(routes!(report_invite_status))
}

fn active() -> Vec<String> {
    ACTIVE_STATES.iter().map(|s| (*s).to_string()).collect()
}

fn states(list: &[InviteState]) -> Vec<String> {
    list.iter().map(|s| s.as_str().to_string()).collect()
}

fn invalid_transition(from: InviteState) -> ApiError {
    ApiError::conflict(
        "invalid_transition",
        "The invite cannot make this change in its current state",
    )
    .with_detail(format!("the invite is {}", from.as_str()))
}

// ---------------------------------------------------------------------------------------
// Loading and publishing
// ---------------------------------------------------------------------------------------

/// Loads invites through `conn` (so a transaction sees its own changes), in `ids` order.
async fn load(state: &AppState, conn: &mut PgConnection, ids: &[Uuid]) -> ApiResult<Vec<Invite>> {
    let rows = sqlx::query!(
        r#"SELECT id, from_user_id, to_user_id, package_id, state, progress, message, failure_reason,
                  created_at, updated_at, expires_at
           FROM game_invites WHERE id = ANY($1)"#,
        ids
    )
    .fetch_all(&mut *conn)
    .await?;
    let users: Vec<Uuid> = rows
        .iter()
        .flat_map(|r| [r.from_user_id, r.to_user_id])
        .collect();
    let packages: Vec<Uuid> = rows.iter().map(|r| r.package_id).collect();
    let users = crate::packages::users_public(state, &users).await?;
    let packages = crate::packages::summaries(state, &packages).await?;
    let mut by_id: HashMap<Uuid, Invite> = HashMap::new();
    for r in rows {
        let (Some(from), Some(to)) = (users.get(&r.from_user_id), users.get(&r.to_user_id)) else {
            continue;
        };
        let Some(package) = packages.get(&r.package_id).cloned() else {
            continue;
        };
        by_id.insert(
            r.id,
            Invite {
                id: r.id,
                from: from.clone(),
                to: to.clone(),
                package,
                state: InviteState::parse(&r.state).unwrap_or(InviteState::Expired),
                progress: r.progress.map(f64::from),
                message: r.message,
                failure_reason: r.failure_reason.as_deref().and_then(InviteFailure::parse),
                created_at: r.created_at,
                updated_at: r.updated_at,
                expires_at: r.expires_at,
            },
        );
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

async fn load_one(state: &AppState, conn: &mut PgConnection, id: Uuid) -> ApiResult<Invite> {
    load(state, conn, &[id])
        .await?
        .pop()
        .ok_or_else(ApiError::not_found)
}

/// Publishes `kind` for each invite to its sender and invitee.
async fn publish(conn: &mut PgConnection, kind: &str, invites: &[Invite]) -> ApiResult<()> {
    for invite in invites {
        events::publish(
            conn,
            &[invite.from.id, invite.to.id],
            kind,
            InviteEvent {
                invite: invite.clone(),
            },
        )
        .await?;
    }
    Ok(())
}

/// Marks overdue active invites expired (all of them, or those involving `user`) and tells
/// both parties. Returns how many expired.
pub async fn expire_due(
    state: &AppState,
    conn: &mut PgConnection,
    user: Option<Uuid>,
) -> ApiResult<usize> {
    let ids = sqlx::query_scalar!(
        "UPDATE game_invites SET state = 'expired'
         WHERE state = ANY($1) AND expires_at <= now()
           AND ($2::uuid IS NULL OR from_user_id = $2 OR to_user_id = $2)
         RETURNING id",
        &active(),
        user
    )
    .fetch_all(&mut *conn)
    .await?;
    if !ids.is_empty() {
        let invites = load(state, conn, &ids).await?;
        publish(conn, kinds::INVITE_UPDATED, &invites).await?;
    }
    Ok(ids.len())
}

/// [`expire_due`] for `user`'s invites in its own transaction, so a refused change that
/// follows cannot roll the expiry (and its events) back.
async fn expire_due_now(state: &AppState, user: Uuid) -> ApiResult<()> {
    let mut tx = state.db.begin().await?;
    expire_due(state, &mut tx, Some(user)).await?;
    tx.commit().await?;
    Ok(())
}

/// Cancels every live invite between `a` and `b` (a block) and tells both parties.
pub async fn cancel_between(
    state: &AppState,
    conn: &mut PgConnection,
    a: Uuid,
    b: Uuid,
) -> ApiResult<Vec<Uuid>> {
    let ids = sqlx::query_scalar!(
        "UPDATE game_invites SET state = 'cancelled'
         WHERE ((from_user_id = $1 AND to_user_id = $2) OR (from_user_id = $2 AND to_user_id = $1))
           AND state = ANY($3)
         RETURNING id",
        a,
        b,
        &active()
    )
    .fetch_all(&mut *conn)
    .await?;
    if !ids.is_empty() {
        let invites = load(state, conn, &ids).await?;
        publish(conn, kinds::INVITE_UPDATED, &invites).await?;
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------------------
// Create and list
// ---------------------------------------------------------------------------------------

impl Validate for InviteCreate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if let Some(m) = &self.message
            && (m.chars().count() > 200 || m.chars().any(|c| c.is_control() && c != '\n'))
        {
            invalid(
                errors,
                "message",
                "invalid",
                "at most 200 characters, no control characters",
            );
        }
    }
}

/// Invite a friend to play
#[utoipa::path(
    post,
    path = "/v1/invites",
    tag = "invites",
    operation_id = "createInvite",
    request_body = InviteCreate,
    responses(
        (status = 200, description = "The existing active invite", body = Invite),
        (status = 201, description = "Created", body = Invite),
        BadRequest, Unauthorized, Forbidden, NotFound, TooManyRequests
    )
)]
pub async fn create_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<InviteCreate>,
) -> ApiResult<JsonResponse<Invite>> {
    let me = user.user_id;
    if body.to_user_id == me {
        return Err(ApiError::field(
            "to_user_id",
            "self",
            "you cannot invite yourself",
        ));
    }
    state
        .limits
        .check(Policy::Invites, &format!("invites:{me}"))?;
    expire_due_now(&state, me).await?;
    let mut tx = state.db.begin().await?;
    if !relations::can_view(&mut tx, me, body.to_user_id).await? {
        return Err(ApiError::not_found());
    }
    if !relations::are_friends(&mut tx, me, body.to_user_id).await? {
        return Err(ApiError::forbidden_code(
            "not_allowed",
            "You can only invite friends",
        ));
    }
    let visible = sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM packages p
             WHERE p.id = $1 AND p.status = 'published' AND p.deleted_at IS NULL
               AND EXISTS (SELECT 1 FROM package_releases r WHERE r.package_id = p.id)
           ) AS "e!""#,
        body.package_id
    )
    .fetch_one(&mut *tx)
    .await?;
    if !visible {
        return Err(ApiError::not_found());
    }
    let message = body
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let inserted = sqlx::query_scalar!(
        "INSERT INTO game_invites (from_user_id, to_user_id, package_id, message, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))
         ON CONFLICT (from_user_id, to_user_id, package_id)
           WHERE state IN ('pending', 'accepted', 'installing', 'ready') DO NOTHING
         RETURNING id",
        me,
        body.to_user_id,
        body.package_id,
        message,
        PENDING_TTL.as_seconds_f64()
    )
    .fetch_optional(&mut *tx)
    .await?;
    let (id, created) = match inserted {
        Some(id) => (id, true),
        None => (
            sqlx::query_scalar!(
                "SELECT id FROM game_invites
                 WHERE from_user_id = $1 AND to_user_id = $2 AND package_id = $3 AND state = ANY($4)",
                me,
                body.to_user_id,
                body.package_id,
                &active()
            )
            .fetch_one(&mut *tx)
            .await?,
            false,
        ),
    };
    let invite = load_one(&state, &mut tx, id).await?;
    if created {
        publish(
            &mut tx,
            kinds::INVITE_CREATED,
            std::slice::from_ref(&invite),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(JsonResponse(
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        invite,
    ))
}

/// Your active invites and those that ended in the last day, newest first
#[utoipa::path(
    get,
    path = "/v1/invites",
    tag = "invites",
    operation_id = "listInvites",
    responses((status = 200, description = "Invites", body = InviteList), Unauthorized)
)]
pub async fn list_invites(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<axum::Json<InviteList>> {
    let mut tx = state.db.begin().await?;
    expire_due(&state, &mut tx, Some(user.user_id)).await?;
    let ids = sqlx::query_scalar!(
        "SELECT id FROM game_invites
         WHERE (from_user_id = $1 OR to_user_id = $1)
           AND (state = ANY($2) OR updated_at > now() - make_interval(secs => $3))
         ORDER BY created_at DESC, id DESC LIMIT 200",
        user.user_id,
        &active(),
        LIST_RECENT.as_seconds_f64()
    )
    .fetch_all(&mut *tx)
    .await?;
    let items = load(&state, &mut tx, &ids).await?;
    tx.commit().await?;
    Ok(axum::Json(InviteList { items }))
}

// ---------------------------------------------------------------------------------------
// Transitions
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Sender,
    Invitee,
}

/// One allowed move of the state machine.
struct Change<'a> {
    role: Role,
    from: &'a [InviteState],
    to: InviteState,
    /// New lifetime from now (accepting).
    extend_to: Option<Duration>,
    failure: Option<InviteFailure>,
}

impl<'a> Change<'a> {
    fn new(role: Role, from: &'a [InviteState], to: InviteState) -> Self {
        Self {
            role,
            from,
            to,
            extend_to: None,
            failure: None,
        }
    }
}

/// Applies one guarded transition for `user` and publishes it.
async fn transition(
    state: &AppState,
    user: Uuid,
    id: Uuid,
    change: Change<'_>,
) -> ApiResult<Invite> {
    let Change {
        role,
        from,
        to,
        extend_to,
        failure,
    } = change;
    expire_due_now(state, user).await?;
    let mut tx = state.db.begin().await?;
    let changed = sqlx::query_scalar!(
        "UPDATE game_invites SET
           state = $4,
           progress = CASE WHEN $4 IN ('ready', 'joined') THEN 1 WHEN $4 = 'accepted' THEN NULL ELSE progress END,
           failure_reason = $5,
           expires_at = CASE WHEN $6::float8 IS NULL THEN expires_at ELSE now() + make_interval(secs => $6) END
         WHERE id = $1 AND state = ANY($3)
           AND CASE WHEN $7 THEN from_user_id = $2 ELSE to_user_id = $2 END
         RETURNING id",
        id,
        user,
        &states(from),
        to.as_str(),
        failure.map(InviteFailure::as_str),
        extend_to.map(|d| d.as_seconds_f64()),
        role == Role::Sender
    )
    .fetch_optional(&mut *tx)
    .await?;
    if changed.is_none() {
        return Err(refusal(&mut tx, user, id, role).await?);
    }
    let invite = load_one(state, &mut tx, id).await?;
    publish(
        &mut tx,
        kinds::INVITE_UPDATED,
        std::slice::from_ref(&invite),
    )
    .await?;
    tx.commit().await?;
    Ok(invite)
}

/// Why a guarded update changed nothing: not this caller's invite (404) or the wrong state (409).
async fn refusal(conn: &mut PgConnection, user: Uuid, id: Uuid, role: Role) -> ApiResult<ApiError> {
    let row = sqlx::query!(
        "SELECT state, from_user_id, to_user_id FROM game_invites WHERE id = $1",
        id
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        Some(r)
            if (role == Role::Sender && r.from_user_id == user)
                || (role == Role::Invitee && r.to_user_id == user) =>
        {
            invalid_transition(InviteState::parse(&r.state).unwrap_or(InviteState::Expired))
        }
        _ => ApiError::not_found(),
    })
}

/// Accept an invite (invitee)
#[utoipa::path(
    post,
    path = "/v1/invites/{invite_id}/accept",
    tag = "invites",
    operation_id = "acceptInvite",
    params(("invite_id" = Uuid, Path)),
    responses((status = 200, description = "Updated invite", body = Invite), Unauthorized, NotFound, Conflict)
)]
pub async fn accept_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<Invite>> {
    Ok(axum::Json(
        transition(
            &state,
            user.user_id,
            id,
            Change {
                extend_to: Some(ACTIVE_TTL),
                ..Change::new(
                    Role::Invitee,
                    &[InviteState::Pending],
                    InviteState::Accepted,
                )
            },
        )
        .await?,
    ))
}

/// Decline an invite (invitee)
#[utoipa::path(
    post,
    path = "/v1/invites/{invite_id}/decline",
    tag = "invites",
    operation_id = "declineInvite",
    params(("invite_id" = Uuid, Path)),
    responses((status = 200, description = "Updated invite", body = Invite), Unauthorized, NotFound, Conflict)
)]
pub async fn decline_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<Invite>> {
    Ok(axum::Json(
        transition(
            &state,
            user.user_id,
            id,
            Change::new(
                Role::Invitee,
                &[InviteState::Pending],
                InviteState::Declined,
            ),
        )
        .await?,
    ))
}

/// Cancel an invite (sender)
#[utoipa::path(
    post,
    path = "/v1/invites/{invite_id}/cancel",
    tag = "invites",
    operation_id = "cancelInvite",
    params(("invite_id" = Uuid, Path)),
    responses((status = 200, description = "Updated invite", body = Invite), Unauthorized, NotFound, Conflict)
)]
pub async fn cancel_invite(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<Invite>> {
    let from = [
        InviteState::Pending,
        InviteState::Accepted,
        InviteState::Installing,
        InviteState::Ready,
    ];
    Ok(axum::Json(
        transition(
            &state,
            user.user_id,
            id,
            Change::new(Role::Sender, &from, InviteState::Cancelled),
        )
        .await?,
    ))
}

impl Validate for InviteStatusUpdate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        match self.progress {
            Some(_) if self.state != InviteReport::Installing => {
                invalid(errors, "progress", "not_allowed", "only with installing");
            }
            Some(p) if !(0.0..=1.0).contains(&p) || p.is_nan() => {
                invalid(errors, "progress", "out_of_range", "0 to 1");
            }
            _ => {}
        }
        match (self.state, self.failure_reason) {
            (InviteReport::Failed, None) => {
                invalid(errors, "failure_reason", "required", "required with failed");
            }
            (s, Some(_)) if s != InviteReport::Failed => {
                invalid(errors, "failure_reason", "not_allowed", "only with failed");
            }
            _ => {}
        }
    }
}

/// Report install or join progress (invitee)
#[utoipa::path(
    post,
    path = "/v1/invites/{invite_id}/status",
    tag = "invites",
    operation_id = "reportInviteStatus",
    params(("invite_id" = Uuid, Path)),
    request_body = InviteStatusUpdate,
    responses((status = 200, description = "Updated invite", body = Invite), BadRequest, Unauthorized, NotFound, Conflict)
)]
pub async fn report_invite_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<InviteStatusUpdate>,
) -> ApiResult<axum::Json<Invite>> {
    use InviteState::{Accepted, Installing, Ready};
    let invite = match body.state {
        InviteReport::Installing => {
            report_progress(&state, user.user_id, id, body.progress).await?
        }
        InviteReport::Ready => {
            let change = Change::new(Role::Invitee, &[Accepted, Installing], InviteState::Ready);
            transition(&state, user.user_id, id, change).await?
        }
        InviteReport::Joined => {
            let change = Change::new(Role::Invitee, &[Ready], InviteState::Joined);
            transition(&state, user.user_id, id, change).await?
        }
        InviteReport::Failed => {
            let change = Change {
                failure: body.failure_reason,
                ..Change::new(Role::Invitee, &[Accepted, Installing], InviteState::Failed)
            };
            transition(&state, user.user_id, id, change).await?
        }
    };
    Ok(axum::Json(invite))
}

/// `installing` from `accepted` (a state change, always published) or again with new
/// progress (published at most every 2 s per invite).
async fn report_progress(
    state: &AppState,
    user: Uuid,
    id: Uuid,
    progress: Option<f64>,
) -> ApiResult<Invite> {
    expire_due_now(state, user).await?;
    let mut tx = state.db.begin().await?;
    #[allow(clippy::cast_possible_truncation)]
    let progress = progress.map(|p| p as f32);
    let published = sqlx::query_scalar!(
        r#"UPDATE game_invites SET
             progress = COALESCE($3, progress),
             progress_published_at = CASE
               WHEN state <> 'installing' OR progress_published_at IS NULL
                    OR progress_published_at <= now() - interval '2 seconds' THEN now()
               ELSE progress_published_at END,
             state = 'installing'
           WHERE id = $1 AND to_user_id = $2 AND state IN ('accepted', 'installing')
           RETURNING progress_published_at = now() AS "published!""#,
        id,
        user,
        progress
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(published) = published else {
        return Err(refusal(&mut tx, user, id, Role::Invitee).await?);
    };
    let invite = load_one(state, &mut tx, id).await?;
    if published {
        publish(
            &mut tx,
            kinds::INVITE_UPDATED,
            std::slice::from_ref(&invite),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(invite)
}
