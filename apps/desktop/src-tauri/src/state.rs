//! Process-wide launcher state, managed by Tauri and reachable from every
//! command. Every member is a cheap handle, so background tasks clone what they
//! need instead of holding the whole state.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::events::EventBus;
use crate::images::{ImageCache, ImageCacheError};
use crate::launch::GameSessions;
use crate::paths::AppPaths;
use crate::servers::Servers;

pub struct AppState {
    pub paths: AppPaths,
    /// Local SQLite database (see `db`).
    pub db: Db,
    /// Native-only cover cache served by the `vgimg://` protocol.
    pub images: Arc<ImageCache>,
    /// Internal typed event bus (see `events`).
    pub bus: EventBus,
    /// Running games (see `launch::session`).
    pub games: GameSessions,
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
    pub fn new(
        paths: AppPaths,
        db: Db,
        bus: EventBus,
        servers: Arc<Servers>,
    ) -> Result<Self, ImageCacheError> {
        let images = Arc::new(ImageCache::open(paths.cache_dir.join("images"))?);
        Ok(Self {
            paths,
            games: GameSessions::new(db.clone(), bus.clone()),
            db,
            images,
            bus,
            servers,
            shutdown: CancellationToken::new(),
            ui_ready: CancellationToken::new(),
        })
    }
}
