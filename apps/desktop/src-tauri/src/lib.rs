//! vgames desktop core. Owner: Agent 2 (Tauri & Systems); Agent 4 owns `social/`
//! and `overlay/`, Agent 5 owns `updater/`.
//!
//! Invariant: the WebView never talks to the network, never touches the
//! filesystem, and never sees a token or key. Every capability is a typed Tauri
//! command implemented here (docs/architecture/00-overview.md §3.1).
//!
//! Idle means idle: no polling timers. Background work waits on channels, OS
//! notifications or cancellation tokens.

pub mod commands;
pub mod db;
pub mod error;
pub mod events;
pub mod logging;
pub mod paths;
pub mod social;
pub mod state;
pub mod updater;

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_deep_link::DeepLinkExt;

use crate::events::AppEvent;
use crate::paths::AppPaths;
use crate::state::AppState;

/// Label of the main launcher window (`tauri.conf.json`).
pub const MAIN_WINDOW: &str = "main";

/// If the UI never calls `app_ready` (it crashed while loading), show the
/// window anyway so the user is not left with an invisible app.
const UI_READY_FALLBACK: Duration = Duration::from_secs(15);

/// Holds the log writer until exit so pending lines are flushed.
struct LogGuardSlot(Mutex<Option<logging::LogGuard>>);

pub fn run() {
    logging::install_panic_hook();

    let profile = match paths::debug_profile() {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("vgames: {error}");
            std::process::exit(2);
        }
    };

    let mut context = tauri::generate_context!();
    // A profile gets its own identifier, hence its own data dir, WebView
    // storage and single-instance lock.
    let identifier = paths::profile_identifier(&context.config().identifier, profile.as_deref());
    context.config_mut().identifier = identifier;

    let specta = commands::specta_builder();
    #[cfg(debug_assertions)]
    match commands::write_bindings_if_changed(&specta) {
        Ok(true) => eprintln!("vgames: regenerated apps/desktop/src/bindings.ts"),
        Ok(false) => {}
        Err(error) => eprintln!("vgames: {}", error::DisplayChain(&error)),
    }

    let app = tauri::Builder::default()
        // Must be first: a second launch (for example from a `vgames://`
        // shortcut) hands over to the running instance and exits.
        .plugin(tauri_plugin_single_instance::init(on_second_instance))
        .plugin(tauri_plugin_deep_link::init())
        // Used from Rust only: no capability grants these to the WebView.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        // Self-update (Agent 5, `updater/`): minisign-verified, driven from Rust only.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(specta.invoke_handler())
        .setup(move |app| {
            specta.mount_events(app);
            setup(app.handle(), profile)?;
            Ok(())
        })
        .build(context);

    match app {
        Ok(app) => app.run(on_run_event),
        Err(error) => {
            eprintln!("vgames: failed to start: {}", error::DisplayChain(&error));
            std::process::exit(1);
        }
    }
}

fn setup(app: &AppHandle, profile: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::resolve(app, profile)?;
    logging::set_crash_dir(paths.log_dir.clone());
    let guard = logging::init(&paths.log_dir)?;
    app.manage(LogGuardSlot(Mutex::new(Some(guard))));
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        profile = paths.profile.as_deref().unwrap_or("default"),
        data_dir = %paths.data_dir.display(),
        "starting vgames"
    );

    let db = db::Db::open(&paths.database_file())?;
    let state = AppState::new(paths, db);
    events::spawn_ui_bridge(app.clone(), &state.bus, state.shutdown.child_token());
    spawn_show_fallback(app.clone(), &state);

    // Deep links: from the first launch's arguments, and later from the OS
    // (macOS open-url) or a second instance (forwarded by single-instance).
    let bus = state.bus.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            publish_deep_link(&bus, &url);
        }
    });
    match app.deep_link().get_current() {
        Ok(Some(urls)) => {
            for url in urls {
                publish_deep_link(&state.bus, &url);
            }
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(%error, "cannot read the launch deep link"),
    }

    updater::init(app, &state);
    app.manage(state);
    Ok(())
}

fn publish_deep_link(bus: &events::EventBus, url: &url::Url) {
    // Only the route is logged: the rest may carry one-time codes.
    tracing::debug!(
        scheme = url.scheme(),
        route = url.host_str().unwrap_or_default(),
        "deep link received"
    );
    bus.publish(AppEvent::DeepLinkReceived {
        url: url.to_string(),
    });
}

fn spawn_show_fallback(app: AppHandle, state: &AppState) {
    let ready = state.ui_ready.clone();
    let shutdown = state.shutdown.child_token();
    tauri::async_runtime::spawn(async move {
        tokio::select! {
            () = ready.cancelled() => {}
            () = shutdown.cancelled() => {}
            () = tokio::time::sleep(UI_READY_FALLBACK) => {
                tracing::warn!("the UI did not call app_ready; showing the window anyway");
                if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                    commands::show_main_window(&window);
                }
            }
        }
    });
}

/// A second process was started: bring our window forward. Its `vgames://`
/// arguments reach `on_open_url` through the deep-link integration.
fn on_second_instance(app: &AppHandle, argv: Vec<String>, _cwd: String) {
    tracing::info!(
        args = argv.len().saturating_sub(1),
        "second launch forwarded"
    );
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        commands::show_main_window(&window);
    }
}

fn on_run_event(app: &AppHandle, event: RunEvent) {
    if let RunEvent::Exit = event {
        if let Some(state) = app.try_state::<AppState>() {
            state.shutdown.cancel();
        }
        tracing::info!("vgames exiting");
        if let Some(slot) = app.try_state::<LogGuardSlot>() {
            // Dropping the guard flushes the log writer.
            if let Ok(mut guard) = slot.0.lock() {
                guard.take();
            }
        }
    }
}
