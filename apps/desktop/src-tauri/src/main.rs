//! Native shell: opens the window and tells the interface how to reach the running daemon.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod update;

use imperium_config::Paths;
use imperium_protocol::discovery::read_endpoint;
use serde::Serialize;

/// What the interface needs to open its WebSocket to `imperiumd`.
#[derive(Serialize)]
struct DaemonConnection {
    url: String,
    token: String,
}

/// Reads the running daemon's endpoint file.
#[tauri::command]
fn daemon_endpoint() -> Result<DaemonConnection, String> {
    let paths = Paths::from_env().map_err(|error| error.to_string())?;
    let endpoint =
        read_endpoint(&paths.daemon_endpoint_file()).map_err(|error| error.to_string())?;
    Ok(DaemonConnection {
        url: endpoint.url,
        token: endpoint.token,
    })
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            update::setup(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            daemon_endpoint,
            update::update_status,
            update::update_check,
            update::update_set_channel,
            update::update_restart,
            update::update_install_now,
        ])
        .build(tauri::generate_context!())
        .expect("the Imperium window could not start")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                update::install_on_exit(app);
            }
        });
}
