//! Process-wide launcher state, managed by Tauri and reachable from every
//! command. Every member is a cheap handle, so background tasks clone what they
//! need instead of holding the whole state.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::events::EventBus;
use crate::images::ImageCache;
use crate::install_runner::{ServersRemote, TransferRunner};
use crate::launch::GameSessions;
use crate::launch::orchestrate::Launcher;
use crate::launch::orchestrate::host_platform;
use crate::paths::AppPaths;
use crate::queue::InstallQueue;
use crate::servers::Servers;

/// The production install queue.
pub type Installs = InstallQueue<TransferRunner<ServersRemote>>;

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
    /// Checks and starts games (see `launch::orchestrate`).
    pub launcher: Arc<Launcher<Servers>>,
    /// Persisted install queue and its worker (see `queue`).
    pub installs: Arc<Installs>,
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
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let images = Arc::new(ImageCache::open(paths.cache_dir.join("images"))?);
        let games = GameSessions::new(db.clone(), bus.clone());
        let launcher = Arc::new(Launcher::new(
            db.clone(),
            Arc::clone(&servers),
            games.clone(),
        ));
        let options = vgames_transfer::download::DownloadOptions::default();
        let runner = Arc::new(TransferRunner::new(
            db.clone(),
            Arc::new(ServersRemote(Arc::clone(&servers))),
            vgames_transfer::http::transfer_client(&options.client)?,
            options,
            host_platform(),
        ));
        let installs = InstallQueue::new(db.clone(), bus.clone(), runner);
        Ok(Self {
            paths,
            installs,
            games,
            launcher,
            db,
            images,
            bus,
            servers,
            shutdown: CancellationToken::new(),
            ui_ready: CancellationToken::new(),
        })
    }
}
