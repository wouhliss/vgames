//! Admin publishing commands (INS-06). Every server call checks that the signed-in account is an
//! admin or owner; the key file and passphrase stay in Rust (`crate::publishing`).

use std::path::PathBuf;

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use crate::publishing::{
    PackageCreate, PublishCommandError, PublishJob, PublishPackage, PublishPackagePage,
    PublishPlan, PublishResume, PublishStart, PublishVersion,
};
use crate::state::AppState;

async fn pick(app: &AppHandle, folder: bool) -> Result<Option<String>, PublishCommandError> {
    let (reply, selected) = tokio::sync::oneshot::channel();
    let dialog = app.dialog().file();
    if folder {
        dialog.pick_folder(move |path| {
            let _ = reply.send(path);
        });
    } else {
        dialog
            .add_filter("Publisher key", &["vgkey"])
            .pick_file(move |path| {
                let _ = reply.send(path);
            });
    }
    let Some(path) = selected
        .await
        .map_err(|e| PublishCommandError::io("open the file picker", &e))?
    else {
        return Ok(None);
    };
    let path = path
        .into_path()
        .map_err(|e| PublishCommandError::io("read the chosen path", &e))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Packages of the server, any status, matching `query` (title or slug).
#[tauri::command]
#[specta::specta]
pub async fn publish_packages(
    state: State<'_, AppState>,
    server_id: Uuid,
    query: Option<String>,
    cursor: Option<String>,
) -> Result<PublishPackagePage, PublishCommandError> {
    state.publisher.packages(server_id, query, cursor).await
}

#[tauri::command]
#[specta::specta]
pub async fn publish_package_create(
    state: State<'_, AppState>,
    server_id: Uuid,
    create: PackageCreate,
) -> Result<PublishPackage, PublishCommandError> {
    state.publisher.package_create(server_id, create).await
}

/// Every version of a package, newest first.
#[tauri::command]
#[specta::specta]
pub async fn publish_versions(
    state: State<'_, AppState>,
    server_id: Uuid,
    package_id: Uuid,
) -> Result<Vec<PublishVersion>, PublishCommandError> {
    state.publisher.versions(server_id, package_id).await
}

/// Native folder picker; null when cancelled.
#[tauri::command]
#[specta::specta]
pub async fn publish_pick_folder(app: AppHandle) -> Result<Option<String>, PublishCommandError> {
    pick(&app, true).await
}

/// Native file picker for a publisher key file; null when cancelled.
#[tauri::command]
#[specta::specta]
pub async fn publish_pick_key(app: AppHandle) -> Result<Option<String>, PublishCommandError> {
    pick(&app, false).await
}

/// Scans a folder (no upload): files, packs, runnable files and entries that block publishing.
#[tauri::command]
#[specta::specta]
pub async fn publish_plan(
    state: State<'_, AppState>,
    folder: String,
) -> Result<PublishPlan, PublishCommandError> {
    state.publisher.plan(PathBuf::from(folder)).await
}

/// Checks the key, creates the version and starts uploading (`publish-progress` follows).
#[tauri::command]
#[specta::specta]
pub async fn publish_start(
    state: State<'_, AppState>,
    server_id: Uuid,
    start: PublishStart,
) -> Result<PublishJob, PublishCommandError> {
    state.publisher.start(server_id, start).await
}

/// Jobs not yet dismissed, including ones from before a restart.
#[tauri::command]
#[specta::specta]
pub async fn publish_jobs(
    state: State<'_, AppState>,
) -> Result<Vec<PublishJob>, PublishCommandError> {
    Ok(state.publisher.jobs())
}

/// Stops uploading; what is uploaded is kept and the job can be resumed.
#[tauri::command]
#[specta::specta]
pub async fn publish_cancel(
    state: State<'_, AppState>,
    job_id: Uuid,
) -> Result<PublishJob, PublishCommandError> {
    state.publisher.cancel(job_id).await
}

/// `key` is required while `resume_needs_key`, ignored otherwise.
#[tauri::command]
#[specta::specta]
pub async fn publish_resume(
    state: State<'_, AppState>,
    job_id: Uuid,
    key: Option<PublishResume>,
) -> Result<PublishJob, PublishCommandError> {
    state.publisher.resume(job_id, key).await
}

/// Forgets a job that is not running (aborting its unpublished version on the server).
#[tauri::command]
#[specta::specta]
pub async fn publish_dismiss(
    state: State<'_, AppState>,
    job_id: Uuid,
) -> Result<(), PublishCommandError> {
    state.publisher.dismiss(job_id).await
}

/// Makes a `ready` version the current release of its platform.
#[tauri::command]
#[specta::specta]
pub async fn publish_release(
    state: State<'_, AppState>,
    server_id: Uuid,
    version_id: Uuid,
) -> Result<PublishVersion, PublishCommandError> {
    state.publisher.release(server_id, version_id).await
}

/// Withdraws a published version (`reason`: 3–500 characters, kept by the server).
#[tauri::command]
#[specta::specta]
pub async fn version_yank(
    state: State<'_, AppState>,
    server_id: Uuid,
    version_id: Uuid,
    reason: String,
) -> Result<PublishVersion, PublishCommandError> {
    state.publisher.yank(server_id, version_id, reason).await
}
