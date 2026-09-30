//! The overlay when it is not drawn inside the game (05-social §6.2–§6.3): an always-on-top,
//! borderless window at the right edge of the screen (macOS, Windows, X11), or OS
//! notifications for toasts (Wayland). The window lets clicks through to the game unless the
//! panel is open, never takes focus when it appears, and hides when the last game stops.
//!
//! macOS: `app.macOSPrivateApi` is on (Tauri's transparent windows need it; fine for direct
//! distribution, not for the Mac App Store), so the window is transparent there too. It shows
//! on every Space and above fullscreen apps through `visible_on_all_workspaces` +
//! always-on-top. An `NSPanel` (non-activating, `.fullScreenAuxiliary`) would replace it, but
//! needs a Mac to verify "above a fullscreen Space without taking focus" (A4-T10, open).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_notification::NotificationExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{Fallback, OverlayService};

/// Window label (the `overlay` capability applies to it).
pub const LABEL: &str = "overlay";
const WIDTH: f64 = 412.0;

fn window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    if let Some(w) = app.get_webview_window(LABEL) {
        return Ok(w);
    }
    let (height, x) = match app.primary_monitor() {
        Ok(Some(m)) => {
            let scale = m.scale_factor();
            let size = m.size().to_logical::<f64>(scale);
            let pos = m.position().to_logical::<f64>(scale);
            (size.height, pos.x + size.width - WIDTH)
        }
        _ => (900.0, 0.0),
    };
    let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay.html".into()))
        .title("vgames overlay")
        .decorations(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .shadow(false)
        .visible(false)
        .inner_size(WIDTH, height)
        .position(x, 0.0);
    builder.transparent(true).build()
}

/// Wires the service's fallback to the window and notifications.
pub fn install<R: Runtime>(app: &AppHandle<R>, service: &OverlayService) {
    let state: Arc<Mutex<Option<CancellationToken>>> = Arc::default();
    let handle = app.clone();
    let svc = service.clone();
    service.on_fallback(move |mode| {
        let mut running = state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(stop) = running.take() {
            stop.cancel();
        }
        match mode {
            None => {
                if let Some(w) = handle.get_webview_window(LABEL)
                    && let Err(error) = w.hide()
                {
                    tracing::debug!(%error, "cannot hide the overlay window");
                }
            }
            Some(Fallback::Window) => {
                let stop = CancellationToken::new();
                *running = Some(stop.clone());
                tauri::async_runtime::spawn(follow_window(handle.clone(), svc.clone(), stop));
            }
            Some(Fallback::Notifications) => {
                let stop = CancellationToken::new();
                *running = Some(stop.clone());
                tauri::async_runtime::spawn(follow_notifications(
                    handle.clone(),
                    svc.clone(),
                    stop,
                ));
            }
        }
    });
}

/// Shows the window and keeps it click-through unless the panel is open.
async fn follow_window<R: Runtime>(
    app: AppHandle<R>,
    svc: OverlayService,
    stop: CancellationToken,
) {
    let w = match window(&app) {
        Ok(w) => w,
        Err(error) => {
            tracing::warn!(%error, "cannot create the overlay window; using notifications");
            return follow_notifications(app, svc, stop).await;
        }
    };
    let mut views = svc.hub().subscribe();
    let mut shown = false;
    loop {
        let panel = views.borrow_and_update().visible_panel;
        if let Err(error) = w.set_ignore_cursor_events(!panel) {
            tracing::debug!(%error, "cannot change click-through");
        }
        if !shown {
            shown = w.show().is_ok();
        }
        if panel && let Err(error) = w.set_focus() {
            tracing::debug!(%error, "cannot focus the overlay panel");
        }
        tokio::select! {
            () = stop.cancelled() => return,
            r = views.changed() => if r.is_err() { return },
        }
    }
}

/// Wayland: each new toast becomes an OS notification.
async fn follow_notifications<R: Runtime>(
    app: AppHandle<R>,
    svc: OverlayService,
    stop: CancellationToken,
) {
    let mut views = svc.hub().subscribe();
    let mut seen: HashSet<Uuid> = views.borrow().toasts.iter().map(|t| t.id).collect();
    loop {
        tokio::select! {
            () = stop.cancelled() => return,
            r = views.changed() => if r.is_err() { return },
        }
        let toasts = views.borrow_and_update().toasts.clone();
        for t in toasts {
            if !seen.insert(t.id) {
                continue;
            }
            if let Err(error) = app
                .notification()
                .builder()
                .title(&t.title)
                .body(&t.body)
                .show()
            {
                tracing::debug!(%error, "notification not shown");
            }
        }
        if seen.len() > 256 {
            seen = views.borrow().toasts.iter().map(|t| t.id).collect();
        }
    }
}
