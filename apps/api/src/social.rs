//! Social features: friends, devices and keys, messaging, invites, presence.
//! Owner: Agent 4. This module is the mount point; Agent 4 fills `routes()` from
//! `apps/api/src/social/` (convert this file to `social/mod.rs`).

use utoipa_axum::router::OpenApiRouter;

use crate::state::AppState;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
}
