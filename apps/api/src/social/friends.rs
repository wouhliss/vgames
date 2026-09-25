//! Friends, friend codes, blocks and profiles (05-social §2, A4-T03).
//!
//! Rules enforced here (see `relations` for visibility):
//! - A request goes to a user id the caller can already see (shared conversation), or
//!   through a friend code (8 chars of Crockford base32, 15 minutes, single use).
//! - Requests and code redemptions: 30/h per user; code creation: 30/h per user.
//! - 500 accepted friends and 100 pending outgoing requests per user.
//! - A block removes the friendship, hides presence both ways (presence only reaches
//!   accepted friends), cancels active invites, and makes every later request, invite or
//!   message between the two answer `404`, exactly like an unknown user.
//! - Every change is a guarded update or an insert that cannot race; a lost race is `409`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
};
use time::{Duration, OffsetDateTime};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
use vgames_proto::{
    FieldError,
    auth::UserPublic,
    realtime::{FriendEvent, kinds},
    social::{
        FRIEND_CODE_ALPHABET, FRIEND_CODE_LEN, Friend, FriendCode, FriendList, FriendRequestCreate,
        FriendState, is_friend_code,
    },
};

use super::{
    events, presence,
    relations::{self, FRIEND_LIMIT, Friendship, PENDING_LIMIT},
};
use crate::{
    auth::CurrentUser,
    error::{ApiError, ApiResult},
    http::{
        json::{Json, JsonResponse, Validate, invalid},
        ratelimit::Policy,
    },
    openapi_problems::{BadRequest, Conflict, NotFound, TooManyRequests, Unauthorized},
    state::AppState,
};

pub const FRIEND_CODE_TTL: Duration = Duration::minutes(15);

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_friends))
        .routes(routes!(send_request))
        .routes(routes!(accept_request))
        .routes(routes!(decline_request))
        .routes(routes!(remove_friend))
        .routes(routes!(create_friend_code))
        .routes(routes!(block_user, unblock_user))
        .routes(routes!(get_user_profile))
}

impl Validate for FriendRequestCreate {
    fn validate(&self, errors: &mut Vec<FieldError>) {
        if let FriendRequestCreate::Code(c) = self
            && !is_friend_code(&c.friend_code)
        {
            invalid(
                errors,
                "friend_code",
                "pattern",
                "must be 8 characters of Crockford base32",
            );
        }
    }
}

fn friend_code_invalid() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "friend_code_invalid",
        "This friend code is unknown, expired or already used",
    )
}

async fn user_public(state: &AppState, id: Uuid) -> ApiResult<UserPublic> {
    crate::packages::users_public(state, &[id])
        .await?
        .remove(&id)
        .ok_or_else(ApiError::not_found)
}

/// Publishes a `friend.*` event to both users, each seeing the *other* user's id.
async fn notify_pair(
    conn: &mut sqlx::PgConnection,
    kind_for_other: &str,
    kind_for_me: Option<&str>,
    me: Uuid,
    other: Uuid,
) -> ApiResult<()> {
    events::publish(conn, &[other], kind_for_other, FriendEvent { user_id: me }).await?;
    if let Some(kind) = kind_for_me {
        events::publish(conn, &[me], kind, FriendEvent { user_id: other }).await?;
    }
    Ok(())
}

async fn friend_view(
    state: &AppState,
    other: Uuid,
    friendship: Friendship,
    me: Uuid,
) -> ApiResult<Friend> {
    let user = user_public(state, other).await?;
    Ok(match friendship {
        Friendship::Accepted { since } => Friend {
            user,
            state: FriendState::Accepted,
            presence: presence::presence_of(state, &[other]).await?.remove(&other),
            since: Some(since),
        },
        Friendship::Pending {
            requested_by,
            since,
        } => Friend {
            user,
            state: if requested_by == me {
                FriendState::Outgoing
            } else {
                FriendState::Incoming
            },
            presence: None,
            since: Some(since),
        },
        Friendship::None => return Err(ApiError::not_found()),
    })
}

/// Friends and pending requests with presence
#[utoipa::path(
    get,
    path = "/v1/friends",
    tag = "social",
    operation_id = "listFriends",
    responses((status = 200, description = "Friend list", body = FriendList), Unauthorized)
)]
pub async fn list_friends(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<axum::Json<FriendList>> {
    let me = user.user_id;
    let rows = sqlx::query!(
        r#"SELECT u.id AS "other!", f.state, f.requested_by, f.created_at, f.accepted_at
           FROM friendships f
           JOIN users u ON u.id = CASE WHEN f.user_low = $1 THEN f.user_high ELSE f.user_low END
           WHERE (f.user_low = $1 OR f.user_high = $1) AND u.disabled_at IS NULL"#,
        me
    )
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.other).collect();
    let users = crate::packages::users_public(&state, &ids).await?;
    let accepted: Vec<Uuid> = rows
        .iter()
        .filter(|r| r.state == "accepted")
        .map(|r| r.other)
        .collect();
    let mut presence = presence::presence_of(&state, &accepted).await?;
    let mut list = FriendList::default();
    for r in rows {
        let Some(user) = users.get(&r.other).cloned() else {
            continue;
        };
        if r.state == "accepted" {
            list.friends.push(Friend {
                user,
                state: FriendState::Accepted,
                presence: presence.remove(&r.other),
                since: Some(r.accepted_at.unwrap_or(r.created_at)),
            });
        } else if r.requested_by == me {
            list.outgoing.push(Friend {
                user,
                state: FriendState::Outgoing,
                presence: None,
                since: Some(r.created_at),
            });
        } else {
            list.incoming.push(Friend {
                user,
                state: FriendState::Incoming,
                presence: None,
                since: Some(r.created_at),
            });
        }
    }
    let name = |f: &Friend| {
        f.user
            .display_name
            .clone()
            .unwrap_or_else(|| f.user.username.clone())
            .to_lowercase()
    };
    list.friends.sort_by_key(name);
    list.incoming.sort_by_key(|f| std::cmp::Reverse(f.since));
    list.outgoing.sort_by_key(|f| std::cmp::Reverse(f.since));
    Ok(axum::Json(list))
}

async fn check_can_add_friend(conn: &mut sqlx::PgConnection, user: Uuid) -> ApiResult<()> {
    if relations::friend_count(conn, user).await? >= FRIEND_LIMIT {
        return Err(ApiError::conflict(
            "friend_limit_reached",
            "The friend limit (500) has been reached",
        ));
    }
    Ok(())
}

/// Accepts a pending request `requester` sent to `me` (inside `tx`, row already locked).
async fn accept_locked(
    conn: &mut sqlx::PgConnection,
    me: Uuid,
    requester: Uuid,
) -> ApiResult<OffsetDateTime> {
    check_can_add_friend(conn, me).await?;
    check_can_add_friend(conn, requester).await?;
    let (low, high) = relations::pair(me, requester);
    let accepted_at = sqlx::query_scalar!(
        r#"UPDATE friendships SET state = 'accepted', accepted_at = now()
           WHERE user_low = $1 AND user_high = $2 AND state = 'pending' AND requested_by = $3
           RETURNING accepted_at AS "accepted_at!""#,
        low,
        high,
        requester
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ApiError::conflict("conflict", "The request changed; reload and try again"))?;
    notify_pair(
        conn,
        kinds::FRIEND_ACCEPTED,
        Some(kinds::FRIEND_ACCEPTED),
        me,
        requester,
    )
    .await?;
    Ok(accepted_at)
}

/// Send a friend request by user id or friend code
#[utoipa::path(
    post,
    path = "/v1/friends/requests",
    tag = "social",
    operation_id = "sendFriendRequest",
    request_body = FriendRequestCreate,
    responses(
        (status = 201, description = "Request created (or already friends)", body = Friend),
        BadRequest, Unauthorized, NotFound, Conflict, TooManyRequests
    )
)]
pub async fn send_request(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<FriendRequestCreate>,
) -> ApiResult<JsonResponse<Friend>> {
    let me = user.user_id;
    state
        .limits
        .check(Policy::FriendRequests, &format!("friend-request:{me}"))?;
    let mut tx = state.db.begin().await?;
    let target = match body {
        FriendRequestCreate::User(u) => {
            if u.user_id == me {
                return Err(ApiError::field(
                    "user_id",
                    "self",
                    "you cannot add yourself",
                ));
            }
            if !relations::can_view(&mut tx, me, u.user_id).await? {
                return Err(ApiError::not_found());
            }
            u.user_id
        }
        FriendRequestCreate::Code(c) => {
            let owner = sqlx::query_scalar!(
                "SELECT user_id FROM friend_codes WHERE code = $1 AND used_at IS NULL AND expires_at > now() FOR UPDATE",
                c.friend_code
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(friend_code_invalid)?;
            if owner == me {
                return Err(ApiError::field(
                    "friend_code",
                    "own_code",
                    "this is your own friend code",
                ));
            }
            // A blocked or disabled owner looks exactly like an unknown code; the code stays unused.
            if !relations::active_user(&mut tx, owner).await?
                || relations::blocked_either(&mut tx, me, owner).await?
            {
                return Err(friend_code_invalid());
            }
            sqlx::query!(
                "UPDATE friend_codes SET used_at = now() WHERE code = $1",
                c.friend_code
            )
            .execute(&mut *tx)
            .await?;
            owner
        }
    };
    if relations::blocked_either(&mut tx, me, target).await? {
        return Err(ApiError::not_found());
    }
    let friendship = match relations::friendship(&mut tx, me, target, true).await? {
        Friendship::Pending { requested_by, .. } if requested_by == target => {
            // They already asked: this is an acceptance.
            let since = accept_locked(&mut tx, me, target).await?;
            Friendship::Accepted { since }
        }
        // Already friends, or already asked: nothing changes.
        f @ (Friendship::Accepted { .. } | Friendship::Pending { .. }) => f,
        Friendship::None => {
            if relations::outgoing_pending_count(&mut tx, me).await? >= PENDING_LIMIT {
                return Err(ApiError::conflict(
                    "pending_limit_reached",
                    "You have too many pending friend requests (100)",
                ));
            }
            check_can_add_friend(&mut tx, me).await?;
            let (low, high) = relations::pair(me, target);
            let created_at = sqlx::query_scalar!(
                "INSERT INTO friendships (user_low, user_high, state, requested_by) VALUES ($1, $2, 'pending', $3)
                 ON CONFLICT (user_low, user_high) DO NOTHING RETURNING created_at",
                low,
                high,
                me
            )
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::conflict("conflict", "The friendship changed; reload and try again"))?;
            notify_pair(
                &mut tx,
                kinds::FRIEND_REQUEST,
                Some(kinds::FRIEND_REQUEST),
                me,
                target,
            )
            .await?;
            Friendship::Pending {
                requested_by: me,
                since: created_at,
            }
        }
    };
    tx.commit().await?;
    Ok(JsonResponse(
        StatusCode::CREATED,
        friend_view(&state, target, friendship, me).await?,
    ))
}

/// Accept an incoming request
#[utoipa::path(
    post,
    path = "/v1/friends/{user_id}/accept",
    tag = "social",
    operation_id = "acceptFriendRequest",
    params(("user_id" = Uuid, Path)),
    responses((status = 200, description = "Now friends", body = Friend), Unauthorized, NotFound, Conflict)
)]
pub async fn accept_request(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(other): Path<Uuid>,
) -> ApiResult<axum::Json<Friend>> {
    let me = user.user_id;
    let mut tx = state.db.begin().await?;
    if !relations::active_user(&mut tx, other).await? {
        return Err(ApiError::not_found());
    }
    let since = match relations::friendship(&mut tx, me, other, true).await? {
        Friendship::Pending { requested_by, .. } if requested_by == other => {
            accept_locked(&mut tx, me, other).await?
        }
        Friendship::Accepted { .. } => {
            return Err(ApiError::conflict(
                "already_friends",
                "You are already friends",
            ));
        }
        _ => return Err(ApiError::not_found()),
    };
    tx.commit().await?;
    Ok(axum::Json(
        friend_view(&state, other, Friendship::Accepted { since }, me).await?,
    ))
}

/// Decline an incoming request
#[utoipa::path(
    post,
    path = "/v1/friends/{user_id}/decline",
    tag = "social",
    operation_id = "declineFriendRequest",
    params(("user_id" = Uuid, Path)),
    responses((status = 204, description = "Declined"), Unauthorized, NotFound)
)]
pub async fn decline_request(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(other): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let me = user.user_id;
    let (low, high) = relations::pair(me, other);
    let mut tx = state.db.begin().await?;
    let deleted = sqlx::query!(
        "DELETE FROM friendships WHERE user_low = $1 AND user_high = $2 AND state = 'pending' AND requested_by = $3",
        low,
        high,
        other
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    notify_pair(
        &mut tx,
        kinds::FRIEND_REMOVED,
        Some(kinds::FRIEND_REMOVED),
        me,
        other,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Remove a friend or cancel an outgoing request
#[utoipa::path(
    delete,
    path = "/v1/friends/{user_id}",
    tag = "social",
    operation_id = "removeFriend",
    params(("user_id" = Uuid, Path)),
    responses((status = 204, description = "Removed"), Unauthorized, NotFound)
)]
pub async fn remove_friend(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(other): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let me = user.user_id;
    let (low, high) = relations::pair(me, other);
    let mut tx = state.db.begin().await?;
    let deleted = sqlx::query!(
        "DELETE FROM friendships WHERE user_low = $1 AND user_high = $2",
        low,
        high
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    notify_pair(
        &mut tx,
        kinds::FRIEND_REMOVED,
        Some(kinds::FRIEND_REMOVED),
        me,
        other,
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A random canonical friend code (40 bits from the OS CSPRNG).
pub fn random_friend_code() -> Result<String, ApiError> {
    let mut bytes = [0u8; 5];
    getrandom::fill(&mut bytes).map_err(ApiError::internal_from)?;
    let mut bits = bytes.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    let mut out = [0u8; FRIEND_CODE_LEN];
    for slot in out.iter_mut().rev() {
        let i = usize::try_from(bits & 31).unwrap_or(0);
        *slot = FRIEND_CODE_ALPHABET.get(i).copied().unwrap_or(b'0');
        bits >>= 5;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Create a single-use friend code (15 minutes)
#[utoipa::path(
    post,
    path = "/v1/friend-codes",
    tag = "social",
    operation_id = "createFriendCode",
    responses((status = 201, description = "Code", body = FriendCode), Unauthorized, TooManyRequests)
)]
pub async fn create_friend_code(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<JsonResponse<FriendCode>> {
    state.limits.check(
        Policy::FriendRequests,
        &format!("friend-code:{}", user.user_id),
    )?;
    let expires_at = OffsetDateTime::now_utc() + FRIEND_CODE_TTL;
    for _ in 0..4 {
        let code = random_friend_code()?;
        let inserted = sqlx::query!(
            "INSERT INTO friend_codes (code, user_id, expires_at) VALUES ($1, $2, $3) ON CONFLICT (code) DO NOTHING",
            code,
            user.user_id,
            expires_at
        )
        .execute(&state.db)
        .await?
        .rows_affected();
        if inserted == 1 {
            return Ok(JsonResponse(
                StatusCode::CREATED,
                FriendCode { code, expires_at },
            ));
        }
    }
    Err(ApiError::internal_from(
        "could not allocate a unique friend code",
    ))
}

/// Block a user
#[utoipa::path(
    post,
    path = "/v1/blocks/{user_id}",
    tag = "social",
    operation_id = "blockUser",
    params(("user_id" = Uuid, Path)),
    responses((status = 204, description = "Blocked"), Unauthorized, NotFound)
)]
pub async fn block_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(other): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let me = user.user_id;
    if other == me {
        return Err(ApiError::not_found());
    }
    let mut tx = state.db.begin().await?;
    let already = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM user_blocks WHERE blocker_id = $1 AND blocked_id = $2) AS "e!""#,
        me,
        other
    )
    .fetch_one(&mut *tx)
    .await?;
    if already {
        return Ok(StatusCode::NO_CONTENT);
    }
    if !relations::can_view(&mut tx, me, other).await? {
        return Err(ApiError::not_found());
    }
    sqlx::query!(
        "INSERT INTO user_blocks (blocker_id, blocked_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        me,
        other
    )
    .execute(&mut *tx)
    .await?;
    let (low, high) = relations::pair(me, other);
    let removed = sqlx::query!(
        "DELETE FROM friendships WHERE user_low = $1 AND user_high = $2",
        low,
        high
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    super::invites::cancel_between(&mut tx, me, other).await?;
    if removed > 0 {
        notify_pair(
            &mut tx,
            kinds::FRIEND_REMOVED,
            Some(kinds::FRIEND_REMOVED),
            me,
            other,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unblock a user
#[utoipa::path(
    delete,
    path = "/v1/blocks/{user_id}",
    tag = "social",
    operation_id = "unblockUser",
    params(("user_id" = Uuid, Path)),
    responses((status = 204, description = "Unblocked"), Unauthorized, NotFound)
)]
pub async fn unblock_user(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(other): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let deleted = sqlx::query!(
        "DELETE FROM user_blocks WHERE blocker_id = $1 AND blocked_id = $2",
        user.user_id,
        other
    )
    .execute(&state.db)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Public profile of a related user
#[utoipa::path(
    get,
    path = "/v1/users/{user_id}",
    tag = "social",
    operation_id = "getUserProfile",
    params(("user_id" = Uuid, Path)),
    responses((status = 200, description = "Profile", body = UserPublic), Unauthorized, NotFound)
)]
pub async fn get_user_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(other): Path<Uuid>,
) -> ApiResult<axum::Json<UserPublic>> {
    let mut conn = state.db.acquire().await?;
    if !relations::can_view(&mut conn, user.user_id, other).await? {
        return Err(ApiError::not_found());
    }
    drop(conn);
    Ok(axum::Json(user_public(&state, other).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_codes_are_canonical() {
        for _ in 0..200 {
            let c = random_friend_code().unwrap();
            assert!(is_friend_code(&c), "{c}");
        }
    }
}
