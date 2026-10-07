//! Library commands for installed packages (INS-04).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tauri::State;

use crate::db::{collections, download_jobs, installs as store};
use crate::error::AppError;
use crate::events::{AppEvent, InstallsChanged, PackageRef};
use crate::installs::manage::{self, UninstallPlan};
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

async fn target(state: &AppState, pkg: PackageRef) -> Result<manage::Target, InstallActionError> {
    manage::target(&state.db, pkg, state.games.is_running(pkg)).await
}

fn changed(state: &AppState, pkg: PackageRef, root: &std::path::Path) {
    state.launcher.prelaunch().forget(root);
    state.installs.forget(pkg);
    state
        .bus
        .publish(AppEvent::InstallsChanged(InstallsChanged {}));
}

/// Moves the install into another library (rename, or a verified copy).
#[tauri::command]
#[specta::specta]
pub async fn install_move(
    state: State<'_, AppState>,
    pkg: PackageRef,
    library_id: String,
) -> Result<(), InstallActionError> {
    let library_id =
        uuid::Uuid::parse_str(&library_id).map_err(|_| InstallActionError::NotFound)?;
    let target = target(&state, pkg).await?;
    let old_root = target.row.root.clone();
    let trust = state
        .servers
        .trust_state(pkg.server_id)
        .await
        .map_err(|e| InstallActionError::io("read the trust state", &e))?
        .ok_or(InstallActionError::Io {
            detail: "The server's trust bundle is not available".into(),
        })?;
    state
        .bus
        .publish(AppEvent::InstallsChanged(InstallsChanged {}));
    let result = manage::move_to(&state.db, pkg, target, library_id, trust).await;
    changed(&state, pkg, &old_root);
    result.map(|_| ())
}

/// What uninstalling removes and what the player decides about.
#[tauri::command]
#[specta::specta]
pub async fn install_uninstall_plan(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<UninstallPlan, InstallActionError> {
    let target = target(&state, pkg).await?;
    manage::uninstall_plan(
        &target,
        crate::compat::prefix_dir(&state.paths.data_dir, &pkg),
    )
    .await
}

/// Removes the install. Leftovers (files the package did not ship) and the
/// Proton/Wine prefix are removed only when asked. Links are never followed.
#[tauri::command]
#[specta::specta]
pub async fn install_uninstall(
    state: State<'_, AppState>,
    pkg: PackageRef,
    remove_leftovers: bool,
    remove_prefix: bool,
) -> Result<(), InstallActionError> {
    let target = target(&state, pkg).await?;
    let root = target.row.root.clone();
    let prefix = remove_prefix.then(|| crate::compat::prefix_dir(&state.paths.data_dir, &pkg));
    state
        .bus
        .publish(AppEvent::InstallsChanged(InstallsChanged {}));
    let result = manage::uninstall(&state.db, pkg, target, remove_leftovers, prefix).await;
    if result.is_ok() {
        if let Err(error) = super::shortcuts::remove_for_package(&state.db, pkg).await {
            tracing::warn!(%error, "cannot remove the package's shortcuts");
        }
        state.installs.uninstalled(pkg);
    }
    changed(&state, pkg, &root);
    result
}

/// Opens the install directory in the system file manager.
#[tauri::command]
#[specta::specta]
pub async fn install_open_folder(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), AppError> {
    let row = store::row(&state.db, pkg)
        .await
        .map_err(|e| AppError::internal("read the install", &e))?
        .ok_or(AppError::NotFound)?;
    tokio::task::spawn_blocking(move || {
        let is_dir = std::fs::symlink_metadata(&row.root).is_ok_and(|m| m.is_dir());
        if !is_dir {
            return Err(AppError::NotFound);
        }
        open::that_detached(&row.root).map_err(|e| AppError::internal("open the folder", &e))
    })
    .await
    .map_err(|e| AppError::internal("open the folder", &e))?
}
