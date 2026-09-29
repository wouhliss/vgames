//! Cross-instance event fan-out through Postgres `LISTEN/NOTIFY`.
//!
//! `publish` sends `{target, event}` on channel `vgames_events` (inside a transaction,
//! Postgres delivers it at commit). Each instance `LISTEN`s and dispatches to its local
//! sockets. Payloads above 7.5 KB are stored in `realtime_events` and sent by reference.

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, postgres::PgListener};
use time::OffsetDateTime;
use uuid::Uuid;
use vgames_proto::realtime::Envelope;

use super::hub::Target;
use crate::{error::ApiError, state::AppState};

pub const CHANNEL: &str = "vgames_events";
/// Postgres caps NOTIFY payloads at 8000 bytes.
const MAX_INLINE: usize = 7500;

#[derive(Serialize, Deserialize)]
struct Notice {
    target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<Envelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_ref: Option<Uuid>,
}

/// Builds an envelope for `kind` with `data`.
pub fn envelope(kind: &str, data: impl Serialize) -> Result<Envelope, ApiError> {
    Ok(Envelope {
        v: 1,
        id: Some(Uuid::now_v7()),
        kind: kind.to_string(),
        ts: Some(OffsetDateTime::now_utc()),
        data: serde_json::to_value(data).map_err(ApiError::internal_from)?,
    })
}

/// Publishes `kind`/`data` to `target` right away.
pub async fn publish(
    pool: &PgPool,
    target: &Target,
    kind: &str,
    data: impl Serialize,
) -> Result<(), ApiError> {
    let mut conn = pool.acquire().await?;
    publish_tx(&mut conn, target, kind, data).await
}

/// Publishes on `conn`. Inside a transaction, Postgres delivers the notice only at
/// commit, and never if the transaction rolls back.
pub async fn publish_tx(
    conn: &mut PgConnection,
    target: &Target,
    kind: &str,
    data: impl Serialize,
) -> Result<(), ApiError> {
    if target.users.is_empty() {
        return Ok(());
    }
    let env = envelope(kind, data)?;
    let inline = serde_json::to_string(&Notice {
        target: target.clone(),
        event: Some(env.clone()),
        event_ref: None,
    })
    .map_err(ApiError::internal_from)?;
    let payload = if inline.len() <= MAX_INLINE {
        inline
    } else {
        let stored = serde_json::to_value(&env).map_err(ApiError::internal_from)?;
        let id = sqlx::query_scalar!(
            "INSERT INTO realtime_events (payload) VALUES ($1) RETURNING id",
            stored
        )
        .fetch_one(&mut *conn)
        .await?;
        let notice = Notice {
            target: target.clone(),
            event: None,
            event_ref: Some(id),
        };
        serde_json::to_string(&notice).map_err(ApiError::internal_from)?
    };
    if payload.len() > MAX_INLINE {
        return Err(ApiError::internal_from(
            "realtime target list too large for NOTIFY",
        ));
    }
    sqlx::query!("SELECT pg_notify($1, $2)", CHANNEL, payload)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Ends sessions' realtime connections: sends `session.revoked` and closes the sockets.
pub async fn revoke_sessions(
    state: &AppState,
    user_id: Uuid,
    sessions: &[Uuid],
    reason: &str,
) -> Result<(), ApiError> {
    let target = Target {
        users: vec![user_id],
        sessions: Some(sessions.to_vec()),
        close: true,
    };
    publish(
        &state.db,
        &target,
        "session.revoked",
        vgames_proto::realtime::SessionRevoked {
            reason: reason.to_string(),
        },
    )
    .await
}

/// Same as [`revoke_sessions`] for every socket of `user_id`.
pub async fn revoke_user(state: &AppState, user_id: Uuid, reason: &str) -> Result<(), ApiError> {
    let target = Target {
        users: vec![user_id],
        sessions: None,
        close: true,
    };
    publish(
        &state.db,
        &target,
        "session.revoked",
        vgames_proto::realtime::SessionRevoked {
            reason: reason.to_string(),
        },
    )
    .await
}

/// Runs the `LISTEN` loop until shutdown, reconnecting with backoff.
///
/// Notifications sent while the `LISTEN` connection is down (database failover, connection
/// reset) are lost. So after listening again, this instance closes its sockets with 1012:
/// clients reconnect and resync over REST after `hello`, and nothing stays undelivered.
pub async fn listen(state: AppState) {
    let mut backoff = Duration::from_millis(250);
    let mut listened = false;
    while !state.shutdown.is_cancelled() {
        match listen_once(&state, &mut listened).await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(error = %e, ?backoff, "realtime listener failed; reconnecting");
                tokio::select! {
                    _ = state.shutdown.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
    }
}

async fn listen_once(state: &AppState, listened: &mut bool) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(&state.db).await?;
    listener.listen(CHANNEL).await?;
    if *listened {
        tracing::warn!("realtime listener back; asking clients to resync");
        state.realtime.close_all(1012, "realtime resync");
    }
    *listened = true;
    state.realtime_ready.send_replace(true);
    loop {
        let notification = tokio::select! {
            _ = state.shutdown.cancelled() => return Ok(()),
            // `recv` would reconnect silently and hide the gap; `try_recv` reports it.
            n = listener.try_recv() => match n? {
                Some(n) => n,
                None => {
                    return Err(sqlx::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "realtime LISTEN connection lost",
                    )));
                }
            },
        };
        let notice: Notice = match serde_json::from_str(notification.payload()) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "ignoring malformed realtime notice");
                continue;
            }
        };
        let event = match (notice.event, notice.event_ref) {
            (Some(e), _) => e,
            (None, Some(id)) => {
                let stored =
                    sqlx::query_scalar!("SELECT payload FROM realtime_events WHERE id = $1", id)
                        .fetch_optional(&state.db)
                        .await?;
                match stored.and_then(|v| serde_json::from_value::<Envelope>(v).ok()) {
                    Some(e) => e,
                    None => continue,
                }
            }
            (None, None) => continue,
        };
        match serde_json::to_string(&event) {
            Ok(text) => state.realtime.dispatch(&notice.target, Arc::from(text)),
            Err(e) => tracing::warn!(error = %e, "unserializable realtime event"),
        }
    }
}
