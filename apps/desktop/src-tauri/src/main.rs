//! Native shell: opens the window, connects the interface to `imperium-core` (starting the bundled
//! daemon when none is running), and keeps the app up to date.
//!
//! Started with `--background-update` (the login entry for "Check for updates when the computer
//! starts"), it opens no window: it checks for an update, installs one if found, and exits.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod store;
mod update;

fn main() {
    let background = update::background();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .app_name(update::LOGIN_ENTRY)
                .arg(update::BACKGROUND_FLAG)
                .build(),
        )
        .manage(daemon::DaemonControl::default())
        .setup(move |app| {
            // The window is made here rather than from the configuration, so the login run
            // never shows one.
            if !background {
                for window in &app.config().app.windows {
                    tauri::WebviewWindowBuilder::from_config(app.handle(), window)?.build()?;
                }
            }
            update::setup(app.handle());
            Ok(())
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
            update::update_set_schedule,
            update::update_restart,
            update::update_install_now,
        ])
        .build(tauri::generate_context!())
        .expect("the Imperium window could not start")
        .run(move |app, event| {
            if let tauri::RunEvent::Exit = event {
                // The login run never started the daemon, so leaves it alone.
                if !background {
                    daemon::on_exit(app);
                    update::install_on_exit(app);
                }
            }
        });
}
