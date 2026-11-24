// Every custom `#[tauri::command]` of the desktop app, by name.
//
// This file is the single list behind the app ACL manifest: `build.rs`
// includes it (`AppManifest::commands`) so Tauri generates an `allow-<name>`
// / `deny-<name>` permission for each entry, and the unit tests in `lib.rs`
// compare it with the registered handlers and the capability files. A command
// that is not listed here is not part of the manifest and cannot be called
// from any window. Keep it free of `use`/`mod` items: `build.rs` `include!`s it.

/// Names are the Rust function names (`snake_case`), exactly as registered in
/// `tauri::generate_handler!`.
pub const COMMAND_NAMES: &[&str] = &[
    "open_composio_link",
    "get_control_state",
    "is_human_driving",
    "begin_action",
    "start_bot_run",
    "pause_bot_run",
    "resume_bot_run",
    "take_the_wheel",
    "give_back_control",
    "stop_bot_run",
    "finish_bot_run",
    "ack_stop",
    "dismiss_stop_widget",
    "set_stop_hotkey",
    "overlay_show_target",
    "overlay_click",
    "overlay_clear",
    "simulate_bot_run",
    "get_monitors_geometry",
    "daemon_connect",
    "owner_key",
    "daemon_restart",
    "daemon_diagnostics",
    "open_log_folder",
    "prereq_check",
    "open_webview_download",
    "providers_cli",
    "get_autostart",
    "set_autostart",
    "autostart_supported",
    "notify_show",
    "notify_take_actions",
    "set_needs_count",
    "set_tray_labels",
    "set_keep_in_tray",
    "update_status",
    "update_check",
    "update_install",
    "update_rollback",
    "quick_ask_configure",
    "quick_ask_connect",
    "quick_ask_hide",
    "quick_ask_open_in_app",
];
