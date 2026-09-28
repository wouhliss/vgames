//! Social features: friends, devices and keys, messaging, invites, presence
//! (docs/architecture/05-social.md, 05-social-notes.md). Owner: Agent 4.

pub mod devices;
pub mod events;
pub mod friends;
pub mod invites;
pub mod presence;
pub mod relations;
pub mod relay;
pub mod typing;

use utoipa_axum::router::OpenApiRouter;
use vgames_proto::realtime::kinds;

use crate::{realtime::hub, state::AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(friends::routes())
        .merge(devices::routes())
        .merge(relay::routes())
        .merge(invites::routes())
        .merge(presence::routes())
}

/// Client→server realtime events this module handles (e.g. `presence.set`, `typing`).
pub fn realtime_handlers() -> Vec<(&'static str, hub::InboundHandler)> {
    vec![
        (kinds::PRESENCE_SET, hub::handler(presence::on_presence_set)),
        (kinds::TYPING, hub::handler(typing::on_typing)),
    ]
}

/// Kind of the every-minute social sweep (05-social-notes §3).
pub const SWEEP_JOB: &str = "social.sweep";

/// Background job handlers this module owns (kinds prefixed `social.`).
pub fn job_handlers() -> Vec<(&'static str, crate::jobs::JobHandler)> {
    vec![(SWEEP_JOB, crate::jobs::handler(sweep))]
}

async fn sweep(ctx: crate::jobs::JobContext) -> Result<(), crate::jobs::JobError> {
    Ok(sweep_once(&ctx.state).await?)
}

/// One `social.sweep` pass: expires overdue invites, deletes friend codes a day after they
/// expire, claimed one-time keys after 30 days and undelivered envelopes when they expire
/// (30 days), and marks stale
/// presence offline (API instances do the latter every 10 s too; the guarded update makes
/// the two agree).
pub async fn sweep_once(state: &AppState) -> crate::error::ApiResult<()> {
    sqlx::query!("DELETE FROM friend_codes WHERE expires_at < now() - interval '1 day'")
        .execute(&state.db)
        .await?;
    sqlx::query!(
        "DELETE FROM device_one_time_keys
         WHERE NOT is_fallback AND claimed_at < now() - interval '30 days'"
    )
    .execute(&state.db)
    .await?;
    sqlx::query!("DELETE FROM message_envelopes WHERE expires_at < now()")
        .execute(&state.db)
        .await?;
    let mut tx = state.db.begin().await?;
    invites::expire_due(state, &mut tx, None).await?;
    tx.commit().await?;
    presence::sweep_stale(state, presence::OFFLINE_AFTER).await?;
    Ok(())
}
