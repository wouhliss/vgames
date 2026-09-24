//! vgames desktop core. Owner: Agent 2 (Tauri & Systems); Agent 4 owns `social/`
//! and `overlay/`.
//!
//! Invariant: the WebView never talks to the network, never touches the
//! filesystem, and never sees a token or key. Every capability is a typed Tauri
//! command implemented here (docs/architecture/00-overview.md, "Desktop").
//!
//! Target layout: app_state · db (SQLite) · servers · auth · library · installs
//! · transfer · launch · shortcuts · deeplink · saves · controllers · updater
//! · social (Agent 4) · overlay (Agent 4) · commands (tauri-specta bindings).

pub fn run() {
    // Plugin order matters: single-instance must be registered first so a
    // second launch (e.g. from a `vgames://` shortcut) forwards its URL to the
    // running instance instead of starting a new process.
    #[allow(clippy::expect_used)]
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {
            // Agent 2 (task 5): focus the main window; deep-link plugin delivers the URL.
        }))
        .plugin(tauri_plugin_deep_link::init())
        .run(tauri::generate_context!())
        .expect("failed to start vgames");
}
