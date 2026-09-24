// Every app command, grouped by the window allowed to call it.
//
// This file is the single list that `build.rs` turns into Tauri permissions
// (`allow-<command>`), and that the capability files grant per window. It is
// `include!`d by `build.rs`, so keep it free of `use` items and crate paths.
// Tests in `commands/mod.rs` check that it matches the registered commands and
// `capabilities/*.json`.
//
// Adding a command: add it to `collect_commands!` in `commands/mod.rs`, add its
// name here, and grant `allow-<name-with-dashes>` in the right capability.

/// Commands the main launcher window may call.
pub const MAIN_WINDOW_COMMANDS: &[&str] = &["app_ready", "app_info"];

/// Commands the in-game overlay window may call. Only `overlay_*` commands
/// belong here (Agent 4 adds them).
pub const OVERLAY_WINDOW_COMMANDS: &[&str] = &[];
