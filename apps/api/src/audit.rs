//! Audit trail. Every admin/owner mutation calls [`record`] inside the transaction that
//! performs it, so the change and its audit row commit (or roll back) together.

use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{auth::RequestMeta, error::ApiResult};

pub async fn record(
    conn: &mut PgConnection,
    actor: Uuid,
    meta: &RequestMeta,
    action: &str,
    target_type: &str,
    target_id: &str,
    details: Value,
) -> ApiResult<()> {
    sqlx::query!(
        r#"INSERT INTO audit_log (actor_user_id, action, target_type, target_id, ip, user_agent, details)
           VALUES ($1, $2, $3, $4, $5::text::inet, $6, $7)"#,
        actor,
        action,
        target_type,
        target_id,
        meta.ip.map(|ip| ip.to_string()),
        meta.user_agent,
        details
    )
    .execute(conn)
    .await?;
    Ok(())
}
