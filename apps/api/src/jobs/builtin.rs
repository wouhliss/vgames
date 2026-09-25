//! Housekeeping jobs owned by the API core.

use super::{JobContext, JobError, JobHandler, handler};

pub fn handlers() -> Vec<(&'static str, JobHandler)> {
    vec![
        ("sweep.expired", handler(sweep_expired)),
        ("saves.gc", handler(crate::saves::gc)),
    ]
}

/// Deletes expired short-lived rows (04-database §5). Social expiry (invites, friend
/// codes, envelopes) is Agent 4's `social.*` sweep.
async fn sweep_expired(ctx: JobContext) -> Result<(), JobError> {
    let db = &ctx.state.db;
    sqlx::query!("DELETE FROM oauth_flows WHERE expires_at < now() - interval '1 day'")
        .execute(db)
        .await?;
    sqlx::query!("DELETE FROM login_codes WHERE expires_at < now() - interval '1 day'")
        .execute(db)
        .await?;
    sqlx::query!("DELETE FROM idempotency_keys WHERE expires_at < now()")
        .execute(db)
        .await?;
    sqlx::query!("DELETE FROM realtime_tickets WHERE expires_at < now()")
        .execute(db)
        .await?;
    sqlx::query!("DELETE FROM realtime_events WHERE created_at < now() - interval '5 minutes'")
        .execute(db)
        .await?;
    sqlx::query!(
        r#"DELETE FROM sessions
           WHERE (revoked_at IS NOT NULL AND revoked_at < now() - interval '30 days')
              OR (revoked_at IS NULL AND kind = 'web' AND access_expires_at < now() - interval '30 days')
              OR (revoked_at IS NULL AND kind = 'desktop' AND refresh_expires_at < now() - interval '30 days')"#
    )
    .execute(db)
    .await?;
    sqlx::query!(
        "DELETE FROM jobs WHERE state IN ('succeeded') AND finished_at < now() - interval '7 days'"
    )
    .execute(db)
    .await?;
    Ok(())
}
