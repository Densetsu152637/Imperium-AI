//! Native shell: opens the window, connects the interface to `imperiumd` (starting the bundled
//! daemon when none is running), and keeps the app up to date.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod update;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            update::setup(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            daemon::daemon_endpoint,
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
