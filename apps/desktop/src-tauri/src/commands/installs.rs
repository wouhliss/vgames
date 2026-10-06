//! Library commands for installed packages (INS-04).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tauri::State;

use crate::db::{collections, download_jobs, installs as store};
use crate::error::AppError;
use crate::events::{AppEvent, InstallsChanged, PackageRef};
use crate::installs::{InstallActionError, InstalledPackage, ListContext, actions};
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

async fn queue(
    state: &AppState,
    pkg: PackageRef,
    kind: download_jobs::JobKind,
) -> Result<(), InstallActionError> {
    actions::queue(&state.db, pkg, kind, state.games.is_running(pkg)).await?;
    state.downloads.queued();
    state
        .bus
        .publish(AppEvent::InstallsChanged(InstallsChanged {}));
    Ok(())
}

/// Queues the available update; it shows up in Downloads.
#[tauri::command]
#[specta::specta]
pub async fn install_update(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), InstallActionError> {
    queue(&state, pkg, download_jobs::JobKind::Update).await
}

/// Re-hashes every file and repairs mismatches (02 §9). An install whose
/// signing key was rotated adopts the server's new signature (F1).
#[tauri::command]
#[specta::specta]
pub async fn install_verify(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), InstallActionError> {
    queue(&state, pkg, download_jobs::JobKind::Repair).await
}

/// Queues an incomplete install again from its journal.
#[tauri::command]
#[specta::specta]
pub async fn install_resume(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), InstallActionError> {
    queue(&state, pkg, download_jobs::JobKind::Install).await
}
