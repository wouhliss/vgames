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
pub const MAIN_WINDOW_COMMANDS: &[&str] = &[
    "app_ready",
    "app_info",
    "servers_list",
    "server_preview",
    "server_confirm",
    "server_switch",
    "server_remove",
    "auth_start",
    "auth_open_browser",
    "auth_submit_code",
    "auth_cancel",
    "auth_sign_out",
    "auth_token_storage",
    "libraries_list",
    "library_pick_folder",
    "library_add",
    "library_set_default",
    "library_remove",
    "shortcut_create",
    "updater_status",
    "updater_check",
    "updater_whats_new",
    "updater_install",
    "social_connection",
    "social_settings_get",
    "social_settings_set",
    "friends_list",
    "friend_code_create",
    "friend_request_send",
    "friend_accept",
    "friend_decline",
    "friend_remove",
    "user_block",
    "user_unblock",
    "blocks_list",
    "user_profile",
    "conversations_list",
    "conversation_open_direct",
    "conversation_create_party",
    "messages_list",
    "message_send",
    "message_retry",
    "conversation_mark_read",
    "typing_start",
    "contact_security",
    "contact_set_verified",
    "contact_trust_device",
    "devices_list",
    "device_revoke",
];

/// Commands the in-game overlay window may call. Only `overlay_*` commands
/// belong here (Agent 4 adds them).
pub const OVERLAY_WINDOW_COMMANDS: &[&str] = &[];
