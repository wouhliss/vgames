//! Cloud-save commands (PLAY-06, 06-cloud-saves-notes). Every package key is
//! a `PackageRef`; no command takes a path, URL or account from the WebView.

use tauri::State;

use crate::error::AppError;
use crate::events::PackageRef;
use uuid::Uuid;

use crate::saves::service::{ConflictChoice, SaveConflict, SaveHistory, SaveSource, SaveStatus};
use crate::state::AppState;

/// The package's cloud-save state from durable records (no network request).
#[tauri::command]
#[specta::specta]
pub async fn saves_status(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<SaveStatus, AppError> {
    state
        .saves
        .status(pkg)
        .await
        .map_err(|error| AppError::internal("read the cloud-save status", &error))
}

/// An open conflict of the signed-in account, for the dialog.
#[tauri::command]
#[specta::specta]
pub async fn save_conflict_get(
    state: State<'_, AppState>,
    conflict_id: Uuid,
) -> Result<SaveConflict, AppError> {
    state.saves.conflict(conflict_id).await
}

/// Applies the player's choice; `cancel` changes nothing. Never launches the
/// game: the player retries the launch.
#[tauri::command]
#[specta::specta]
pub async fn save_conflict_resolve(
    state: State<'_, AppState>,
    conflict_id: Uuid,
    choice: ConflictChoice,
) -> Result<SaveStatus, AppError> {
    state.saves.resolve(conflict_id, choice).await
}

/// Cloud snapshots and this device's retained backups, newest first.
#[tauri::command]
#[specta::specta]
pub async fn saves_history(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<SaveHistory, AppError> {
    state.saves.history(pkg).await
}

/// Restores an earlier save after backing up the current files. The UI
/// confirms first; refused while the game runs or a conflict is open.
#[tauri::command]
#[specta::specta]
pub async fn saves_restore(
    state: State<'_, AppState>,
    pkg: PackageRef,
    source: SaveSource,
) -> Result<SaveStatus, AppError> {
    state.saves.restore(pkg, source).await
}
