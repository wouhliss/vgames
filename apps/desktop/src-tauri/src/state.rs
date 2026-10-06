//! Process-wide launcher state, managed by Tauri and reachable from every
//! command. Every member is a cheap handle, so background tasks clone what they
//! need instead of holding the whole state.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::catalog::Catalog;
use crate::catalog::covers::Covers;
use crate::db::Db;
use crate::downloads::Downloads;
use crate::downloads::backend::ServerBackend;
use crate::events::EventBus;
use crate::images::{ImageCache, ImageCacheError};
use crate::launch::GameSessions;
use crate::launch::orchestrate::Launcher;
use crate::paths::AppPaths;
use crate::servers::Servers;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Images(#[from] ImageCacheError),
    #[error("cannot build the HTTP client")]
    Http(#[from] reqwest::Error),
}

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
    /// Root of every task's cancellation token; cancelled on exit.
    pub shutdown: CancellationToken,
    /// Cancelled (used as a one-shot latch) once the UI has rendered and called
    /// `app_ready`. Any number of tasks may wait on `ui_ready.cancelled()`, before
    /// or after it fires.
    pub ui_ready: CancellationToken,
    /// Servers, trust and sign-in sessions (the only API client).
    pub servers: Arc<Servers>,
    /// The active server's catalog, release selection and covers (INS-02).
    pub catalog: Arc<Catalog<Servers>>,
    /// The install queue and its worker (INS-03).
    pub downloads: Arc<Downloads<ServerBackend>>,
    /// Installed packages: updates found, launch targets, cloud-save state (INS-04).
    pub installs: Arc<crate::installs::Installs>,
}

impl AppState {
    pub fn new(
        paths: AppPaths,
        db: Db,
        bus: EventBus,
        servers: Arc<Servers>,
    ) -> Result<Self, StateError> {
        let images = Arc::new(ImageCache::open(paths.cache_dir.join("images"))?);
        let games = GameSessions::new(db.clone(), bus.clone());
        let launcher = Arc::new(Launcher::new(
            db.clone(),
            Arc::clone(&servers),
            games.clone(),
        ));
        let covers = Arc::new(Covers::new(Arc::clone(&images), crate::api::http_client()?));
        let catalog = Arc::new(Catalog::new(
            Arc::clone(&servers),
            db.clone(),
            covers,
            crate::catalog::release::host_platform(),
        ));
        let shutdown = CancellationToken::new();
        let downloads = Downloads::new(
            db.clone(),
            bus.clone(),
            Arc::new(ServerBackend::new(
                Arc::clone(&catalog),
                Arc::clone(&servers),
            )?),
            vgames_transfer::download::DownloadOptions::default(),
            shutdown.child_token(),
        );
        let installs: Arc<crate::installs::Installs> = Arc::default();
        {
            // After an update or repair: fresh pre-launch checks and launch
            // targets, and no stale "update available".
            let prelaunch = Arc::clone(launcher.prelaunch());
            let installs = Arc::clone(&installs);
            let bus = bus.clone();
            downloads.set_files_changed(Arc::new(move |package, root| {
                prelaunch.forget(root);
                installs.forget(package);
                installs.set_update(package, None);
                bus.publish(crate::events::AppEvent::InstallsChanged(
                    crate::events::InstallsChanged {},
                ));
            }));
        }
        Ok(Self {
            paths,
            catalog,
            downloads,
            installs,
            games,
            launcher,
            db,
            images,
            bus,
            servers,
            shutdown,
            ui_ready: CancellationToken::new(),
        })
    }
}
