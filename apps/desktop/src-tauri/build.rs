//! Lists the app's own commands for the access control list, so each one needs a permission
//! (`allow-<command>`) in a capability (`capabilities/default.json`) before a webview may call it.
//! Without this list every window could call every command, and a command like `plugin_approve`
//! would be reachable from any frame; with it, only the windows a capability names reach it.
//!
//! Keep this list equal to the `generate_handler!` list in `src/main.rs` and to the permissions in
//! the capability: `src/plugin_approval.rs` has a test that fails when they differ.

fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "open_external",
            "daemon_endpoint",
            "daemon_status",
            "daemon_start",
            "daemon_stop",
            "daemon_restart",
            "daemon_log",
            "daemon_set_keep_running",
            "store_read",
            "store_write",
            "store_folder",
            "save_export",
            "update_status",
            "update_check",
            "update_set_channel",
            "update_set_schedule",
            "update_restart",
            "update_install_now",
            "update_switch_install",
            "plugin_approve",
        ]),
    ))
    .expect("the Imperium window's build configuration is invalid");
}
