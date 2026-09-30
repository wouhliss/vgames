//! Install queue commands (A2-T08): pause, resume, retry, cancel, remove and
//! reorder. The queue worker (`queue`) does the work; these only validate and
//! translate errors.

use serde::Serialize;
use specta::Type;
use tauri::State;

use crate::db::download_jobs::JobStoreError;
use crate::events::PackageRef;
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadActionError {
    #[error("not found")]
    NotFound,
    #[error("not enough free space")]
    InsufficientSpace {
        required_bytes: u64,
        available_bytes: u64,
    },
    #[error("the library is offline")]
    LibraryOffline { library_path: String },
    #[error("the server could not be reached")]
    Offline,
    #[error("{detail}")]
    Io { detail: String },
}

impl From<JobStoreError> for DownloadActionError {
    fn from(error: JobStoreError) -> Self {
        match error {
            JobStoreError::NotFound => Self::NotFound,
            other => {
                tracing::warn!(error = %other, "install queue action refused");
                Self::Io {
                    detail: other.to_string(),
                }
            }
        }
    }
}

// `pkg`, not `package`: that is a reserved word in strict-mode TypeScript.
#[tauri::command]
#[specta::specta]
pub async fn download_pause(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    Ok(state.installs.pause(pkg).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn download_resume(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    Ok(state.installs.resume(pkg).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn download_retry(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    Ok(state.installs.retry(pkg).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn download_cancel(
    state: State<'_, AppState>,
    pkg: PackageRef,
    keep_partial: bool,
) -> Result<(), DownloadActionError> {
    Ok(state.installs.cancel(pkg, keep_partial).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn download_remove(
    state: State<'_, AppState>,
    pkg: PackageRef,
) -> Result<(), DownloadActionError> {
    Ok(state.installs.remove(pkg).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn downloads_reorder(
    state: State<'_, AppState>,
    packages: Vec<PackageRef>,
) -> Result<(), DownloadActionError> {
    Ok(state.installs.reorder(packages).await?)
}
