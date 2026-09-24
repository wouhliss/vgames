//! Application lifecycle commands.

use serde::Serialize;
use specta::Type;
use tauri::{State, WebviewWindow};

use crate::MAIN_WINDOW;
use crate::error::{CommandError, CommandResult, ErrorCode};
use crate::state::AppState;

/// Called by the UI once its first screen has rendered. Shows the main window
/// (it starts hidden so the user never sees a blank WebView).
#[tauri::command]
#[specta::specta]
pub fn app_ready(window: WebviewWindow, state: State<'_, AppState>) -> CommandResult<()> {
    if window.label() != MAIN_WINDOW {
        return Err(CommandError::new(
            ErrorCode::NotAllowed,
            "Only the main window can do this.",
        ));
    }
    state.ui_ready.notify_one();
    show_main_window(&window);
    Ok(())
}

pub(crate) fn show_main_window(window: &WebviewWindow) {
    if let Err(error) = window.show() {
        tracing::warn!(%error, "cannot show the main window");
    }
    if let Err(error) = window.unminimize() {
        tracing::debug!(%error, "cannot unminimize the main window");
    }
    if let Err(error) = window.set_focus() {
        tracing::debug!(%error, "cannot focus the main window");
    }
}

/// Build information for the About screen and diagnostics.
#[derive(Debug, Clone, Serialize, Type)]
pub struct AppInfo {
    pub version: String,
    /// Debug profile name (`VGAMES_PROFILE`), debug builds only.
    pub profile: Option<String>,
    pub debug_build: bool,
    /// `windows`, `linux` or `macos`.
    pub os: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
}

#[tauri::command]
#[specta::specta]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        profile: state.paths.profile.clone(),
        debug_build: cfg!(debug_assertions),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
    }
}
