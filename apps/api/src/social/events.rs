//! Publishing social realtime events (03-api §6, 05-social-notes §4).
//!
//! `realtime::bus` sends the recipient list inside the NOTIFY payload, which Postgres caps
//! at 8000 bytes. Presence goes to up to 500 friends, so recipients are sent in chunks.

use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    error::ApiResult,
    realtime::{bus, hub::Target},
};

/// Recipients per NOTIFY (100 UUIDs ≈ 4 KB of the 7.5 KB budget).
pub const RECIPIENTS_PER_NOTICE: usize = 100;

/// Publishes `kind`/`data` to every user in `users`. Inside a transaction, delivery
/// happens at commit (and never on rollback).
pub async fn publish(
    conn: &mut PgConnection,
    users: &[Uuid],
    kind: &str,
    data: impl Serialize,
) -> ApiResult<()> {
    if users.is_empty() {
        return Ok(());
    }
    let data = serde_json::to_value(data).map_err(crate::error::ApiError::internal_from)?;
    for chunk in users.chunks(RECIPIENTS_PER_NOTICE) {
        bus::publish_tx(conn, &Target::users(chunk), kind, &data).await?;
    }
    Ok(())
}
