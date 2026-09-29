//! Server, trust and sign-in commands (A2-T07). Thin wrappers over
//! [`crate::servers::Servers`]; the UI never sees a token or key.

use std::sync::Arc;

use serde::Serialize;
use specta::Type;
use tauri::{Manager as _, State};
use uuid::Uuid;

use crate::error::AppError;
use crate::servers::auth::{AuthError, AuthFlow};
use crate::servers::{Account, ServerError, ServerPreview, ServerProfile, Servers};
use crate::state::AppState;

/// Every stored server, the active one flagged. A database failure is logged
/// and reads as an empty list (the UI then offers to add a server).
#[tauri::command]
#[specta::specta]
pub async fn servers_list(app: tauri::AppHandle) -> Vec<ServerProfile> {
    let servers = Arc::clone(&app.state::<AppState>().servers);
    match servers.list().await {
        Ok(list) => list,
        Err(error) => {
            tracing::error!(error = %crate::error::DisplayChain(&error), "cannot list servers");
            Vec::new()
        }
    }
}

/// Fetches `/.well-known/vgames.json`. `expected_fingerprint` comes from a
/// `vgames://server/add` link and must match.
#[tauri::command]
#[specta::specta]
pub async fn server_preview(
    state: State<'_, AppState>,
    url: String,
    expected_fingerprint: Option<String>,
) -> Result<ServerPreview, ServerError> {
    state
        .servers
        .preview(&url, expected_fingerprint.as_deref())
        .await
}

/// Pins the previewed root key and makes the server active.
#[tauri::command]
#[specta::specta]
pub async fn server_confirm(
    state: State<'_, AppState>,
    preview_id: Uuid,
) -> Result<ServerProfile, ServerError> {
    state.servers.confirm(preview_id).await
}

/// Makes a server active, then checks its identity and trust bundle in the
/// background (`connectivity-changed` / `trust-problem` report the result).
#[tauri::command]
#[specta::specta]
pub async fn server_switch(
    state: State<'_, AppState>,
    server_id: Uuid,
) -> Result<ServerProfile, AppError> {
    let profile = state.servers.switch(server_id).await?;
    spawn_connect(Arc::clone(&state.servers), server_id);
    Ok(profile)
}

/// Signs out and forgets a server (refused while its games are installed).
#[tauri::command]
#[specta::specta]
pub async fn server_remove(state: State<'_, AppState>, server_id: Uuid) -> Result<(), AppError> {
    state.servers.remove(server_id).await
}

/// Starts Discord sign-in for a server and opens the system browser.
#[tauri::command]
#[specta::specta]
pub async fn auth_start(
    state: State<'_, AppState>,
    server_id: Uuid,
) -> Result<AuthFlow, AuthError> {
    state.servers.auth_start(server_id).await
}

#[tauri::command]
#[specta::specta]
pub fn auth_open_browser(state: State<'_, AppState>, flow_id: Uuid) -> Result<(), AuthError> {
    state.servers.auth_open_browser(flow_id)
}

/// The paste-code fallback when the `vgames://` callback cannot reach the launcher.
#[tauri::command]
#[specta::specta]
pub async fn auth_submit_code(
    state: State<'_, AppState>,
    flow_id: Uuid,
    code: String,
) -> Result<Account, AuthError> {
    state.servers.auth_submit_code(flow_id, &code).await
}

#[tauri::command]
#[specta::specta]
pub fn auth_cancel(state: State<'_, AppState>, flow_id: Uuid) {
    state.servers.auth_cancel(flow_id);
}

/// Revokes this device's session on the server and deletes its tokens.
#[tauri::command]
#[specta::specta]
pub async fn auth_sign_out(state: State<'_, AppState>, server_id: Uuid) -> Result<(), AppError> {
    state.servers.sign_out(server_id).await
}

/// Where session tokens are stored.
#[derive(Debug, Clone, Serialize, Type)]
pub struct TokenStorage {
    /// True when no OS keychain is available and tokens are in a private
    /// file; Settings shows a persistent warning.
    pub fallback_file: bool,
}

#[tauri::command]
#[specta::specta]
pub fn auth_token_storage(state: State<'_, AppState>) -> TokenStorage {
    TokenStorage {
        fallback_file: state.servers.token_storage_is_fallback(),
    }
}

/// Identity check + trust refresh for `server_id`, in the background.
pub(crate) fn spawn_connect(servers: Arc<Servers>, server_id: Uuid) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = servers.connect(server_id).await {
            tracing::warn!(%server_id, %error, "server connection check failed");
        }
    });
}
