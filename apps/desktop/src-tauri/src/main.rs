//! Native shell: opens the window, connects the interface to `imperium-core` (starting the bundled
//! daemon when none is running), and keeps the app up to date.
//!
//! Started with `--background-update` (the login entry for "Check for updates when the computer
//! starts"), it opens no window: it checks for an update, installs one if found, and exits.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod mod_frames;
mod store;
mod update;

use tauri::Manager;

/// Colours the window's native background to the saved theme (the system's under "system"),
/// matching `--bg` in styles.css. The configuration's dark colour covers the gap before this runs.
fn paint_background(window: &tauri::WebviewWindow) {
    let light = match store::saved_theme().as_deref() {
        Some("light") => true,
        Some("dark") => false,
        _ => window
            .theme()
            .is_ok_and(|theme| theme == tauri::Theme::Light),
    };
    let color = if light {
        tauri::window::Color(0xf4, 0xf5, 0xf7, 0xff)
    } else {
        tauri::window::Color(0x0b, 0x0c, 0x0e, 0xff)
    };
    let _ = window.set_background_color(Some(color));
}

fn main() {
    let background = update::background();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .app_name(update::LOGIN_ENTRY)
                .arg(update::BACKGROUND_FLAG)
                .build(),
        )
        .manage(daemon::DaemonControl::default())
        .register_uri_scheme_protocol(mod_frames::SCHEME, |_context, request| {
            mod_frames::serve(request)
        })
        .setup(move |app| {
            // The window is made here rather than from the configuration, so the login run
            // never shows one.
            if !background {
                for window in &app.config().app.windows {
                    // Built hidden and shown once its native background matches the theme, so
                    // the system's white never shows before the interface paints.
                    let built = tauri::WebviewWindowBuilder::from_config(app.handle(), window)?
                        .visible(false)
                        .build()?;
                    paint_background(&built);
                    built.show()?;
                }
            }
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
            store::save_export,
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
