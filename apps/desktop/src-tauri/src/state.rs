//! Process-wide launcher state, managed by Tauri and reachable from every
//! command. Every member is a cheap handle, so background tasks clone what they
//! need instead of holding the whole state.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::events::EventBus;
use crate::paths::AppPaths;
use crate::servers::Servers;

pub struct AppState {
    pub paths: AppPaths,
    /// Local SQLite database (see `db`).
    pub db: Db,
    /// Internal typed event bus (see `events`).
    pub bus: EventBus,
    /// Root of every task's cancellation token; cancelled on exit.
    pub shutdown: CancellationToken,
    /// Cancelled (used as a one-shot latch) once the UI has rendered and called
    /// `app_ready`. Any number of tasks may wait on `ui_ready.cancelled()`, before
    /// or after it fires.
    pub ui_ready: CancellationToken,
    /// Servers, trust and sign-in sessions (the only API client).
    pub servers: Arc<Servers>,
}

impl AppState {
    pub fn new(paths: AppPaths, db: Db, bus: EventBus, servers: Arc<Servers>) -> Self {
        Self {
            paths,
            db,
            bus,
            servers,
            shutdown: CancellationToken::new(),
            ui_ready: CancellationToken::new(),
        }
    }
}
