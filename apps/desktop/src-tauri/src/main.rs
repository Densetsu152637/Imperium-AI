//! Native shell: opens the window, connects the interface to `imperium-core` (starting the bundled
//! daemon when none is running), and keeps the app up to date.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod store;
mod update;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(daemon::DaemonControl::default())
        .setup(|app| {
            update::setup(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window quits the app, taking any detached panel windows with it
            // (they are not docked, so they reopen at the next start). Exiting here runs the
            // `RunEvent::Exit` handling below.
            if window.label() == "main"
                && matches!(
                    event,
                    tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
                )
            {
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![
            daemon::daemon_endpoint,
            daemon::daemon_status,
            daemon::daemon_start,
            daemon::daemon_stop,
            daemon::daemon_restart,
            daemon::daemon_log,
            daemon::daemon_set_keep_running,
            store::store_read,
            store::store_write,
            store::store_folder,
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
                daemon::on_exit(app);
                update::install_on_exit(app);
            }
        });
}
