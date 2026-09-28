//! The realtime `typing` event (05-social-notes §3): a member's client says it is typing in
//! a conversation; the server tells the other members' sockets, at most once per user and
//! conversation every [`TYPING_EVERY`]. Nothing is stored. Members on either side of a block
//! with the typist are left out, like the relay does for envelopes.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use uuid::Uuid;
use vgames_proto::realtime::{Typing, TypingStart, kinds};

use super::events;
use crate::{error::ApiError, http::ratelimit::Policy, realtime::hub::InboundContext};

/// One `typing` fan-out per user and conversation in this interval.
pub const TYPING_EVERY: Duration = Duration::from_secs(3);
/// Entries kept for throttling before old ones are pruned.
const MAX_TRACKED: usize = 10_000;

fn last_sent() -> &'static Mutex<HashMap<(Uuid, Uuid), Instant>> {
    static LAST: OnceLock<Mutex<HashMap<(Uuid, Uuid), Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `true` when `(user, conversation)` may fan out now (and records it).
fn allow(user: Uuid, conversation: Uuid, now: Instant) -> bool {
    let mut map = last_sent().lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= MAX_TRACKED {
        map.retain(|_, at| now.duration_since(*at) < TYPING_EVERY);
    }
    match map.get(&(user, conversation)) {
        Some(at) if now.duration_since(*at) < TYPING_EVERY => false,
        _ => {
            map.insert((user, conversation), now);
            true
        }
    }
}

/// Realtime `typing` handler.
pub async fn on_typing(ctx: InboundContext, data: serde_json::Value) -> Result<(), ApiError> {
    ctx.state
        .limits
        .check(Policy::User, &format!("typing:{}", ctx.user_id))?;
    let start: TypingStart = crate::http::json::parse_json(data.to_string().as_bytes())?;
    let mut conn = ctx.state.db.acquire().await?;
    let recipients = sqlx::query_scalar!(
        r#"SELECT m.user_id FROM conversation_members m
           WHERE m.conversation_id = $1 AND m.left_at IS NULL AND m.user_id <> $2
             AND EXISTS (SELECT 1 FROM conversation_members me
                         WHERE me.conversation_id = $1 AND me.user_id = $2 AND me.left_at IS NULL)
             AND NOT EXISTS (SELECT 1 FROM user_blocks b
                             WHERE (b.blocker_id = $2 AND b.blocked_id = m.user_id)
                                OR (b.blocker_id = m.user_id AND b.blocked_id = $2))"#,
        start.conversation_id,
        ctx.user_id
    )
    .fetch_all(&mut *conn)
    .await?;
    if recipients.is_empty() || !allow(ctx.user_id, start.conversation_id, Instant::now()) {
        return Ok(());
    }
    events::publish(
        &mut conn,
        &recipients,
        kinds::TYPING,
        Typing {
            conversation_id: start.conversation_id,
            user_id: ctx.user_id,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_fan_out_per_user_and_conversation_every_three_seconds() {
        let (user, conv, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let t0 = Instant::now();
        assert!(allow(user, conv, t0));
        assert!(!allow(user, conv, t0 + Duration::from_millis(2_900)));
        assert!(allow(user, other, t0 + Duration::from_millis(100)));
        assert!(allow(other, conv, t0 + Duration::from_millis(100)));
        assert!(allow(user, conv, t0 + TYPING_EVERY));
    }
}
