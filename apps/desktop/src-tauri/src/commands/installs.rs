//! Library commands for installed packages (INS-04).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tauri::State;

use crate::db::{collections, download_jobs, installs as store};
use crate::error::AppError;
use crate::events::PackageRef;
use crate::installs::{InstalledPackage, ListContext};
use crate::state::AppState;

/// Installed (and incomplete) packages of the active server.
#[tauri::command]
#[specta::specta]
pub async fn installs_list(state: State<'_, AppState>) -> Result<Vec<InstalledPackage>, AppError> {
    let Ok(server_id) = state.catalog.active_server().await else {
        return Ok(Vec::new());
    };
    let rows = store::list(&state.db, server_id)
        .await
        .map_err(|e| AppError::internal("read the installs", &e))?;
    let favorites: HashSet<_> = collections::favorite_ids(&state.db, server_id)
        .await
        .map_err(|e| AppError::internal("read the favorites", &e))?
        .into_iter()
        .collect();
    let memberships = collections::memberships(&state.db, server_id)
        .await
        .map_err(|e| AppError::internal("read the collections", &e))?;
    let jobs: HashMap<PackageRef, download_jobs::JobKind> = download_jobs::list(&state.db)
        .await
        .map_err(|e| AppError::internal("read the download queue", &e))?
        .into_iter()
        .map(|job| (job.package, job.kind))
        .collect();
    let installs = Arc::clone(&state.installs);
    let games = state.games.clone();
    let covers = Arc::clone(state.catalog.covers());
    let host = state.catalog.host();
    tokio::task::spawn_blocking(move || {
        let running = |package| games.is_running(package);
        let cover = |package: PackageRef, asset| covers.url(package.server_id, asset);
        installs.list(
            rows,
            &ListContext {
                host,
                favorites,
                memberships,
                jobs,
                running: &running,
                cover: &cover,
            },
        )
    })
    .await
    .map_err(|e| AppError::internal("list the installs", &e))
}
