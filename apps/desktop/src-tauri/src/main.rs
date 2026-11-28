//! Native shell: opens the window, connects the interface to `imperium-core` (starting the bundled
//! daemon when none is running), and keeps the app up to date.
//!
//! Started with `--background-update` (the login entry for "Check for updates when the computer
//! starts"), it opens no window: it checks for an update, installs one if found, and exits.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auto_merge;
mod browser;
mod browser_takeover;
mod daemon;
mod mod_frames;
mod plugin_approval;
mod store;
mod update;

use tauri::Manager;

/// Colours the window's native background to the saved theme (the system's under "system"),
/// matching `--bg` in styles/tokens.css. The configuration's dark colour covers the gap before this runs.
fn paint_background(window: &tauri::WebviewWindow) {
    let light = match store::saved_theme() {
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
    // First, before anything loads a DLL by name: search only the app directory and System32.
    // A failure is logged, not fatal: the app still runs, only without the narrower search.
    #[cfg(windows)]
    if let Err(error) = imperium_ipc::harden_dll_search() {
        eprintln!("could not narrow the DLL search path: {error}");
    }
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
        // The app's own environment, read at start (see `plugin_approval`).
        .manage(plugin_approval::Guard::capture())
        // The connection the Browser panel's takeover goes through (see `browser_takeover`).
        .manage(browser_takeover::Takeover::default())
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
                    let mut builder = tauri::WebviewWindowBuilder::from_config(app.handle(), window)?
                        .visible(false);
                    // With no remembered theme in the webview (first launch after an upgrade,
                    // cleared data) the saved one is handed to boot.js before the first paint.
                    if let Some(theme) = store::saved_theme() {
                        builder = builder.initialization_script(format!(
                            "try{{if(window.top===window)localStorage.setItem(\"imperium.theme\",\"{theme}\")}}catch(e){{}}"
                        ));
                    }
                    let built = builder.build()?;
                    paint_background(&built);
                    built.show()?;
                    // Once, after the first show: the OS then gives the window keyboard focus, so
                    // shortcuts (Ctrl+,) work without a click first. Best effort.
                    let _ = built.set_focus();
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
            browser::open_external,
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
            update::update_switch_install,
            plugin_approval::plugin_approve,
            auto_merge::auto_merge_enable,
            browser_takeover::browser_control,
            browser_takeover::browser_act,
        ])
        .build(tauri::generate_context!())
        .expect("the Imperium window could not start")
        .run(move |app, event| {
            if let tauri::RunEvent::Exit = event {
                // The daemon is not stopped here: the app's lease on it ends with the process,
                // and a daemon the app started exits once nothing else uses it. The login run
                // never started the daemon, so leaves it alone.
                if !background {
                    update::install_on_exit(app);
                }
            }
        });
}
