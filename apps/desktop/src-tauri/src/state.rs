//! Process-wide launcher state, managed by Tauri and reachable from every
//! command. Every member is a cheap handle, so background tasks clone what they
//! need instead of holding the whole state.

use std::sync::Arc;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;
use crate::paths::AppPaths;

pub struct AppState {
    pub paths: AppPaths,
    /// Internal typed event bus (see `events`).
    pub bus: EventBus,
    /// Root of every task's cancellation token; cancelled on exit.
    pub shutdown: CancellationToken,
    /// Signalled once the UI has rendered and called `app_ready`.
    pub ui_ready: Arc<Notify>,
}

impl AppState {
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            bus: EventBus::new(),
            shutdown: CancellationToken::new(),
            ui_ready: Arc::new(Notify::new()),
        }
    }
}
