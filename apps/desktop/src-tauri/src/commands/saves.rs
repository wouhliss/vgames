//! Cloud-save commands (PLAY-06, 06-cloud-saves-notes). Every package key is
//! a `PackageRef`; no command takes a path, URL or account from the WebView.

use tauri::State;

use crate::error::AppError;
use crate::events::PackageRef;
use crate::saves::service::SaveStatus;
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
