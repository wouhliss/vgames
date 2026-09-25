//! Presence (05-social §3, 05-social-notes §2.4).
//!
//! - Set with `PUT /v1/presence` or the realtime `presence.set` event, on change only.
//! - Stored last-writer-wins per user in `user_presence` (UNLOGGED); a change is published as
//!   `presence.changed` to **accepted friends only** (blocking deletes the friendship, so
//!   blocked users never receive it).
//! - Liveness comes from the socket: every 10 s each API instance refreshes `heartbeat_at`
//!   for the users with a socket on it. Presence set without a socket (REST only) lapses.
//!   Any instance turns rows with a heartbeat older than 30 s `offline` with a guarded
//!   update, so exactly one instance publishes each change.
//! - `users.last_seen_at` moves at most once a minute, from the same heartbeat.

use std::{collections::HashMap, time::Duration};

use axum::{extract::State, http::StatusCode};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    realtime::{PresenceChanged, kinds},
    social::{Presence, PresenceSetStatus, PresenceStatus, PresenceUpdate},
};

use super::{events, relations};
use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    http::{
        json::{Json, Validate, invalid},
        ratelimit::Policy,
    },
    openapi_problems::{BadRequest, Unauthorized},
    realtime::hub::InboundContext,
    state::AppState,
};

/// How often each instance refreshes the heartbeat of the users connected to it.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(10);
/// A user whose heartbeat is older than this becomes offline.
pub const OFFLINE_AFTER: Duration = Duration::from_secs(30);

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(set_presence))
}

impl Validate for PresenceUpdate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if self.package_id.is_some() && self.status != PresenceSetStatus::InGame {
            invalid(
                errors,
                "package_id",
                "not_allowed",
                "only allowed with status in_game",
            );
        }
    }
}

/// Set own presence
#[utoipa::path(
    put,
    path = "/v1/presence",
    tag = "social",
    operation_id = "setPresence",
    request_body = PresenceUpdate,
    responses((status = 204, description = "Updated"), BadRequest, Unauthorized)
)]
pub async fn set_presence(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(update): Json<PresenceUpdate>,
) -> ApiResult<StatusCode> {
    apply(&state, user.user_id, &update).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Realtime `presence.set` handler.
pub async fn on_presence_set(ctx: InboundContext, data: serde_json::Value) -> Result<(), ApiError> {
    ctx.state
        .limits
        .check(Policy::User, &format!("presence:{}", ctx.user_id))?;
    let update: PresenceUpdate = crate::http::json::parse_json(data.to_string().as_bytes())?;
    let mut errors = Vec::new();
    update.validate(&mut errors);
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    apply(&ctx.state, ctx.user_id, &update).await
}

/// Stores `update` for `user` and publishes `presence.changed` to friends if it changed.
pub async fn apply(state: &AppState, user: Uuid, update: &PresenceUpdate) -> ApiResult<()> {
    let status = PresenceStatus::from(update.status);
    let mut tx = state.db.begin().await?;
    // Only a published, visible package is shared; anything else is "in game" without a title.
    let package = match update.package_id {
        Some(id) => sqlx::query!(
            "SELECT id, title FROM packages WHERE id = $1 AND status = 'published' AND deleted_at IS NULL",
            id
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| (r.id, r.title)),
        None => None,
    };
    let package_id = package.as_ref().map(|p| p.0);
    let before = sqlx::query!(
        "SELECT status, package_id FROM user_presence WHERE user_id = $1 FOR UPDATE",
        user
    )
    .fetch_optional(&mut *tx)
    .await?;
    let changed = before
        .as_ref()
        .is_none_or(|b| b.status != status.as_str() || b.package_id != package_id);
    sqlx::query!(
        r#"INSERT INTO user_presence (user_id, status, package_id, instance_id, updated_at, heartbeat_at)
           VALUES ($1, $2, $3, $4, now(), now())
           ON CONFLICT (user_id) DO UPDATE SET
             status = excluded.status, package_id = excluded.package_id,
             instance_id = excluded.instance_id, heartbeat_at = now(),
             updated_at = CASE WHEN $5 THEN now() ELSE user_presence.updated_at END"#,
        user,
        status.as_str(),
        package_id,
        state.instance_id,
        changed
    )
    .execute(&mut *tx)
    .await?;
    if changed {
        let friends = relations::accepted_friend_ids(&mut tx, user).await?;
        events::publish(
            &mut tx,
            &friends,
            kinds::PRESENCE_CHANGED,
            PresenceChanged {
                user_id: user,
                status,
                package_id,
                package_title: package.map(|p| p.1),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Marks users offline whose heartbeat is older than `older_than` and tells their friends.
/// Returns the users that went offline.
pub async fn sweep_stale(state: &AppState, older_than: Duration) -> ApiResult<Vec<Uuid>> {
    let mut tx = state.db.begin().await?;
    let gone = sqlx::query_scalar!(
        r#"UPDATE user_presence SET status = 'offline', package_id = NULL, updated_at = now()
           WHERE status <> 'offline' AND heartbeat_at < now() - make_interval(secs => $1)
           RETURNING user_id"#,
        older_than.as_secs_f64()
    )
    .fetch_all(&mut *tx)
    .await?;
    for user in &gone {
        let friends = relations::accepted_friend_ids(&mut tx, *user).await?;
        events::publish(
            &mut tx,
            &friends,
            kinds::PRESENCE_CHANGED,
            PresenceChanged {
                user_id: *user,
                status: PresenceStatus::Offline,
                package_id: None,
                package_title: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(gone)
}

/// Presence of `users` as friends see it (users without a row are offline).
pub async fn presence_of(state: &AppState, users: &[Uuid]) -> ApiResult<HashMap<Uuid, Presence>> {
    let rows = sqlx::query!(
        r#"SELECT u.user_id, u.status, u.updated_at, p.id AS "package_id?", p.title AS "package_title?"
           FROM user_presence u
           LEFT JOIN packages p ON p.id = u.package_id AND p.status = 'published' AND p.deleted_at IS NULL
           WHERE u.user_id = ANY($1)"#,
        users
    )
    .fetch_all(&state.db)
    .await?;
    let mut out: HashMap<Uuid, Presence> = rows
        .into_iter()
        .map(|r| {
            (
                r.user_id,
                Presence {
                    status: PresenceStatus::parse(&r.status).unwrap_or(PresenceStatus::Offline),
                    package_id: r.package_id,
                    package_title: r.package_title,
                    updated_at: Some(r.updated_at),
                },
            )
        })
        .collect();
    for u in users {
        out.entry(*u).or_insert(Presence {
            status: PresenceStatus::Offline,
            package_id: None,
            package_title: None,
            updated_at: None,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Liveness
// ---------------------------------------------------------------------------------------

/// Starts this instance's presence monitor (called with the realtime listener).
pub fn start(state: &AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(monitor(state.clone()))
}

async fn monitor(state: AppState) {
    let mut tick = tokio::time::interval(HEARTBEAT_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = state.shutdown.cancelled() => break,
            _ = tick.tick() => {}
        }
        if let Err(error) = heartbeat(&state).await {
            tracing::warn!(%error, "presence heartbeat failed");
        }
        if let Err(error) = sweep_stale(&state, OFFLINE_AFTER).await {
            tracing::warn!(%error, "presence sweep failed");
        }
    }
}

/// Refreshes the heartbeat of every user with a socket on this instance (and claims their
/// row for it), and moves `users.last_seen_at` at most once a minute. Returns how many
/// presence rows were refreshed.
pub async fn heartbeat(state: &AppState) -> ApiResult<u64> {
    let alive = state.realtime.connected_users();
    if alive.is_empty() {
        return Ok(0);
    }
    let refreshed = sqlx::query!(
        "UPDATE user_presence SET heartbeat_at = now(), instance_id = $2
         WHERE user_id = ANY($1) AND status <> 'offline'",
        &alive,
        state.instance_id
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    sqlx::query!(
        "UPDATE users SET last_seen_at = now()
         WHERE id = ANY($1) AND (last_seen_at IS NULL OR last_seen_at < now() - interval '1 minute')",
        &alive
    )
    .execute(&state.db)
    .await?;
    Ok(refreshed)
}
