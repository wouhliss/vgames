//! Overlay commands and events (05-social-notes §7). The overlay window (label `overlay`) may
//! call only `overlay_view` and `overlay_action`; Settings uses `package_overlays_list` and
//! `package_overlay_set` (main-window commands never start with `overlay_`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event as _;

use super::OverlayService;
use super::hub::Hub;
use super::model::{OverlayAction, OverlayView, PackageOverlay};
use crate::db::settings;
use crate::error::AppError;
use crate::events::PackageRef;
use crate::social::service::{SocialService, SocialSettingsKey};
use crate::state::AppState;

/// `overlay-view`: the overlay window's view model changed.
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[tauri_specta(event_name = "overlay-view")]
pub struct OverlayViewChanged(pub OverlayView);

/// `overlay-package-disabled`: the crash safety valve turned the overlay off for a game
/// (show a notice with "Turn it back on").
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[tauri_specta(event_name = "overlay-package-disabled")]
pub struct OverlayPackageDisabled {
    pub package: PackageRef,
}

/// Starts the overlay service (called from the social init) and manages it.
pub fn init(app: &AppHandle, state: &AppState, social: SocialService, hub: Arc<Hub>) {
    let service = OverlayService::start(
        state.db.clone(),
        social,
        hub,
        &state.bus,
        state.shutdown.child_token(),
    );
    tauri::async_runtime::spawn_blocking(super::vulkan_layer::register_for_this_install);
    // Every game the launcher starts gets the overlay environment.
    state.launcher.set_hooks(Arc::new(service.clone()));
    // Do not disturb and the hotkey from the stored settings.
    let stored =
        tauri::async_runtime::block_on(state.db.call(|c| settings::get::<SocialSettingsKey>(c)))
            .unwrap_or_default();
    service.hub().set_do_not_disturb(stored.do_not_disturb);
    service.set_hotkey_host(
        Arc::new(super::hotkey::TauriHotkeys(app.clone())),
        &stored.overlay_hotkey,
    );
    let handle = app.clone();
    service.on_open_launcher(move || {
        if let Some(w) = handle.get_webview_window("main") {
            crate::commands::show_main_window(&w);
        }
    });
    let handle = app.clone();
    service.on_valve_tripped(move |package| {
        let _ = OverlayPackageDisabled { package }.emit(&handle);
    });
    super::fallback::install(app, &service);
    let mut views = service.hub().subscribe();
    let handle = app.clone();
    let stop = state.shutdown.child_token();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                r = views.changed() => if r.is_err() { return },
            }
            let view = views.borrow_and_update().clone();
            let _ = OverlayViewChanged(view).emit(&handle);
        }
    });
    app.manage(service);
}

/// The overlay window's current view model.
#[tauri::command]
#[specta::specta]
pub fn overlay_view(overlay: State<'_, OverlayService>) -> OverlayView {
    overlay.hub().current()
}

/// An action from the overlay window (accept/decline, quick reply, open launcher, close).
#[tauri::command]
#[specta::specta]
pub fn overlay_action(overlay: State<'_, OverlayService>, action: OverlayAction) {
    overlay.act(action);
}

/// Settings → Overlay: the per-game switches.
#[tauri::command]
#[specta::specta]
pub async fn package_overlays_list(
    overlay: State<'_, OverlayService>,
) -> Result<Vec<PackageOverlay>, AppError> {
    overlay
        .packages()
        .await
        .map_err(|e| AppError::internal("read overlay settings", &e))
}

/// Turns the overlay on or off for one game. Turning it on also clears the safety valve.
#[tauri::command]
#[specta::specta]
pub async fn package_overlay_set(
    overlay: State<'_, OverlayService>,
    package_ref: PackageRef,
    enabled: bool,
) -> Result<(), AppError> {
    overlay
        .package_set(package_ref, enabled)
        .await
        .map_err(|e| AppError::internal("save overlay settings", &e))
}
