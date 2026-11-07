//! Native shell: opens the window and tells the interface how to reach the running daemon.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
        .invoke_handler(tauri::generate_handler![daemon_endpoint])
        .run(tauri::generate_context!())
        .expect("the Imperium window could not start");
}
