//! Relationship rules shared by every social endpoint (05-social §2).
//!
//! - Friendships are stored once per pair, `(user_low, user_high)` in UUID byte order.
//! - A block (either direction) hides both users from each other: endpoints answer
//!   `404 not_found`, the same as for a user that does not exist, so a block (or an
//!   account) is never confirmed to someone who should not know about it.
//! - A user is *visible* to another when they are friends (any state), share a current
//!   conversation, or are the same user, and neither blocked the other.

use sqlx::PgConnection;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::ApiResult;

/// Accepted friends per user (05-social §2).
pub const FRIEND_LIMIT: i64 = 500;
/// Pending outgoing requests per user.
pub const PENDING_LIMIT: i64 = 100;

/// `(low, high)` in the order the `friendships` table stores them.
pub fn pair(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a < b { (a, b) } else { (b, a) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Friendship {
    None,
    Pending {
        requested_by: Uuid,
        since: OffsetDateTime,
    },
    Accepted {
        since: OffsetDateTime,
    },
}

/// The friendship row between `a` and `b`, locked with `FOR UPDATE` when `lock`.
pub async fn friendship(
    conn: &mut PgConnection,
    a: Uuid,
    b: Uuid,
    lock: bool,
) -> ApiResult<Friendship> {
    let (low, high) = pair(a, b);
    let row = if lock {
        sqlx::query!(
            "SELECT state, requested_by, created_at, accepted_at FROM friendships
             WHERE user_low = $1 AND user_high = $2 FOR UPDATE",
            low,
            high
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| (r.state, r.requested_by, r.created_at, r.accepted_at))
    } else {
        sqlx::query!(
            "SELECT state, requested_by, created_at, accepted_at FROM friendships
             WHERE user_low = $1 AND user_high = $2",
            low,
            high
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| (r.state, r.requested_by, r.created_at, r.accepted_at))
    };
    Ok(match row {
        None => Friendship::None,
        Some((state, _, created_at, accepted_at)) if state == "accepted" => Friendship::Accepted {
            since: accepted_at.unwrap_or(created_at),
        },
        Some((_, requested_by, created_at, _)) => Friendship::Pending {
            requested_by,
            since: created_at,
        },
    })
}

/// `true` when either user blocked the other.
pub async fn blocked_either(conn: &mut PgConnection, a: Uuid, b: Uuid) -> ApiResult<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM user_blocks
             WHERE (blocker_id = $1 AND blocked_id = $2) OR (blocker_id = $2 AND blocked_id = $1)
           ) AS "e!""#,
        a,
        b
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// `true` when both users are current members of at least one conversation.
pub async fn share_conversation(conn: &mut PgConnection, a: Uuid, b: Uuid) -> ApiResult<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM conversation_members m1
             JOIN conversation_members m2 ON m2.conversation_id = m1.conversation_id
             WHERE m1.user_id = $1 AND m2.user_id = $2 AND m1.left_at IS NULL AND m2.left_at IS NULL
           ) AS "e!""#,
        a,
        b
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// `true` when `target` exists, is not disabled, and is visible to `viewer` (module docs).
pub async fn can_view(conn: &mut PgConnection, viewer: Uuid, target: Uuid) -> ApiResult<bool> {
    if !active_user(conn, target).await? {
        return Ok(false);
    }
    if viewer == target {
        return Ok(true);
    }
    if blocked_either(conn, viewer, target).await? {
        return Ok(false);
    }
    if friendship(conn, viewer, target, false).await? != Friendship::None {
        return Ok(true);
    }
    share_conversation(conn, viewer, target).await
}

/// `true` when the user exists and is not disabled.
pub async fn active_user(conn: &mut PgConnection, id: Uuid) -> ApiResult<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND disabled_at IS NULL) AS "e!""#,
        id
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// `true` when `a` and `b` are accepted friends and neither blocked the other.
pub async fn are_friends(conn: &mut PgConnection, a: Uuid, b: Uuid) -> ApiResult<bool> {
    Ok(matches!(
        friendship(conn, a, b, false).await?,
        Friendship::Accepted { .. }
    ) && !blocked_either(conn, a, b).await?)
}

/// Accepted friends of `user` (the audience of presence events).
pub async fn accepted_friend_ids(conn: &mut PgConnection, user: Uuid) -> ApiResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT CASE WHEN user_low = $1 THEN user_high ELSE user_low END AS "id!"
           FROM friendships WHERE (user_low = $1 OR user_high = $1) AND state = 'accepted'"#,
        user
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// Accepted friends of `user` right now.
pub async fn friend_count(conn: &mut PgConnection, user: Uuid) -> ApiResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM friendships WHERE (user_low = $1 OR user_high = $1) AND state = 'accepted'"#,
        user
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// Pending requests `user` sent.
pub async fn outgoing_pending_count(conn: &mut PgConnection, user: Uuid) -> ApiResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM friendships WHERE requested_by = $1 AND state = 'pending'"#,
        user
    )
    .fetch_one(&mut *conn)
    .await?)
}

/// `true` when `a` may exchange keys and messages with `b`: the same user, or accepted
/// friends or current conversation co-members, with no block either way.
pub async fn may_message(conn: &mut PgConnection, a: Uuid, b: Uuid) -> ApiResult<bool> {
    if a == b {
        return Ok(true);
    }
    if !active_user(conn, b).await? || blocked_either(conn, a, b).await? {
        return Ok(false);
    }
    Ok(matches!(
        friendship(conn, a, b, false).await?,
        Friendship::Accepted { .. }
    ) || share_conversation(conn, a, b).await?)
}

/// Everyone who follows `user`'s devices: the user (their other devices), accepted friends
/// and current conversation co-members, without anyone on either side of a block.
pub async fn contacts(conn: &mut PgConnection, user: Uuid) -> ApiResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT DISTINCT c.id AS "id!" FROM (
             SELECT $1::uuid AS id
             UNION SELECT CASE WHEN user_low = $1 THEN user_high ELSE user_low END
               FROM friendships WHERE (user_low = $1 OR user_high = $1) AND state = 'accepted'
             UNION SELECT m2.user_id FROM conversation_members m1
               JOIN conversation_members m2 ON m2.conversation_id = m1.conversation_id
               WHERE m1.user_id = $1 AND m1.left_at IS NULL AND m2.left_at IS NULL
           ) c
           WHERE c.id = $1 OR NOT EXISTS (
             SELECT 1 FROM user_blocks
             WHERE (blocker_id = $1 AND blocked_id = c.id) OR (blocker_id = c.id AND blocked_id = $1))"#,
        user
    )
    .fetch_all(&mut *conn)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_orders_like_postgres_uuid_comparison() {
        let a = Uuid::parse_str("00000000-0000-7000-8000-000000000002").unwrap();
        let b = Uuid::parse_str("ffffffff-0000-7000-8000-000000000001").unwrap();
        assert_eq!(pair(a, b), (a, b));
        assert_eq!(pair(b, a), (a, b));
    }
}
