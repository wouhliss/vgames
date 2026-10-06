//! Install queue commands (INS-03): the Downloads screen and Settings →
//! Downloads. Every argument is validated here; the worker does the rest.
//! Package arguments are `pkg`: `package` is reserved in strict TypeScript.

use serde::Serialize;
use specta::Type;
use tauri::State;

use crate::downloads::{DownloadActionError, DownloadQueue, DownloadSettings};
use crate::error::AppError;
use crate::events::PackageRef;
use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub async fn downloads_list(state: State<'_, AppState>) -> Result<DownloadQueue, AppError> {
    let covers = state.catalog.covers();
    state
        .downloads
        .list(|package, asset| covers.url(package.server_id, asset))
        .await
        .map_err(|e| AppError::internal("read the download queue", &e))
}

#[tauri::command]
#[specta::specta]
pub async fn download_pause(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    state.downloads.pause(pkg).await
}

/// Resumes a paused job (it runs when its turn comes).
#[tauri::command]
#[specta::specta]
pub async fn download_resume(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    state.downloads.resume(pkg).await
}

/// Queues a failed job again, from its journal.
#[tauri::command]
#[specta::specta]
pub async fn download_retry(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    state.downloads.retry(pkg).await
}

/// Removes a failed job from the queue (its partial files are kept).
#[tauri::command]
#[specta::specta]
pub async fn download_remove(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    state.downloads.remove(pkg).await
}

/// Stops the job. `keep_partial`: keep the downloaded files so the install
/// can be resumed later; otherwise delete them.
#[tauri::command]
#[specta::specta]
pub async fn download_cancel(
    state: State<'_, AppState>,
    pkg: PackageRef,
    keep_partial: bool,
) -> Result<(), DownloadActionError> {
    state.downloads.cancel(pkg, keep_partial).await
}

/// The new order of the waiting jobs; running jobs keep running.
#[tauri::command]
#[specta::specta]
pub async fn downloads_reorder(
    state: State<'_, AppState>,
    packages: Vec<PackageRef>,
) -> Result<(), DownloadActionError> {
    if packages.len() > 10_000 {
        return Err(DownloadActionError::Io {
            detail: "Too many downloads".into(),
        });
    }
    state.downloads.reorder(packages).await
}

#[tauri::command]
#[specta::specta]
pub async fn downloads_history_clear(state: State<'_, AppState>) -> Result<(), AppError> {
    state
        .downloads
        .clear_history()
        .await
        .map_err(|e| AppError::internal("clear the download history", &e))
}

#[tauri::command]
#[specta::specta]
pub fn download_settings_get(state: State<'_, AppState>) -> DownloadSettings {
    state.downloads.settings()
}

/// A setting that could not be saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettingsError {
    #[error("{field}: {detail}")]
    Invalid { field: String, detail: String },
    #[error("{detail}")]
    Io { detail: String },
}

#[tauri::command]
#[specta::specta]
pub async fn download_settings_set(
    state: State<'_, AppState>,
    settings: DownloadSettings,
) -> Result<DownloadSettings, SettingsError> {
    if !(1..=DownloadSettings::MAX_CONCURRENT).contains(&settings.concurrent_installs) {
        return Err(SettingsError::Invalid {
            field: "concurrent_installs".into(),
            detail: "Choose 1 to 3 installs at a time".into(),
        });
    }
    if settings.bandwidth_limit_kib == Some(0) {
        return Err(SettingsError::Invalid {
            field: "bandwidth_limit_kib".into(),
            detail: "The limit must be at least 1 KiB/s".into(),
        });
    }
    state
        .downloads
        .set_settings(settings)
        .await
        .map_err(|error| {
            tracing::error!(%error, "cannot save the download settings");
            SettingsError::Io {
                detail: "Cannot save the download settings".into(),
            }
        })
}
