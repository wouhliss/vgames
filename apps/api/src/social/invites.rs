//! Game invites (05-social §5). A4-T06 adds the endpoints and the state machine; A4-T03
//! only needs blocking to end the invites between two users.

use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::ApiResult;

/// States in which an invite is still live.
pub const ACTIVE_STATES: [&str; 4] = ["pending", "accepted", "installing", "ready"];

/// Cancels every live invite between `a` and `b` (either direction). Returns their ids.
pub async fn cancel_between(conn: &mut PgConnection, a: Uuid, b: Uuid) -> ApiResult<Vec<Uuid>> {
    let active: Vec<String> = ACTIVE_STATES.iter().map(|s| (*s).to_string()).collect();
    Ok(sqlx::query_scalar!(
        "UPDATE game_invites SET state = 'cancelled', updated_at = now()
         WHERE ((from_user_id = $1 AND to_user_id = $2) OR (from_user_id = $2 AND to_user_id = $1))
           AND state = ANY($3)
         RETURNING id",
        a,
        b,
        &active
    )
    .fetch_all(&mut *conn)
    .await?)
}
