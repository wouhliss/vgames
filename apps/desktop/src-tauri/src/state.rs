//! Process-wide launcher state, managed by Tauri and reachable from every
//! command. Every member is a cheap handle, so background tasks clone what they
//! need instead of holding the whole state.

use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::events::EventBus;
use crate::images::{ImageCache, ImageCacheError};
use crate::paths::AppPaths;

pub struct AppState {
    pub paths: AppPaths,
    /// Local SQLite database (see `db`).
    pub db: Db,
    /// Native-only cover cache served by the `vgimg://` protocol.
    pub images: Arc<ImageCache>,
    /// Internal typed event bus (see `events`).
    pub bus: EventBus,
    /// Root of every task's cancellation token; cancelled on exit.
    pub shutdown: CancellationToken,
    /// Cancelled (used as a one-shot latch) once the UI has rendered and called
    /// `app_ready`. Any number of tasks may wait on `ui_ready.cancelled()`, before
    /// or after it fires.
    pub ui_ready: CancellationToken,
}

impl AppState {
    pub fn new(paths: AppPaths, db: Db) -> Result<Self, ImageCacheError> {
        let images = Arc::new(ImageCache::open(paths.cache_dir.join("images"))?);
        Ok(Self {
            paths,
            db,
            images,
            bus: EventBus::new(),
            shutdown: CancellationToken::new(),
            ui_ready: CancellationToken::new(),
        })
    }
}
