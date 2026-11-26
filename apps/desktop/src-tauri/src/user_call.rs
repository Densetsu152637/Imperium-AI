//! The few daemon calls that change what the daemon launches or trusts, made over the app's own
//! local-socket connection.
//!
//! The daemon refuses these from anything but a person's own process on its local socket (not an
//! agent, a mod, a phone or a client over loopback TCP, which is what the webview uses):
//! `config.set` of a runtime binary, path, argument or update-policy key or of
//! `mcp.trusted_projects`, `runtime.set_server` with a path, and `models.set_config` with
//! `extra_args`. So the webview asks this command to make the call instead, and the command
//! checks four things before it sends anything:
//!
//! 1. it is called from the app's own windows (`own_window`, and the access control list in
//!    `build.rs` and `capabilities/default.json`);
//! 2. the method is one of [`ALLOWED`], nothing else;
//! 3. the app's own environment has none of the loader overrides the daemon refuses in a peer;
//! 4. the connection is the local socket, never loopback TCP (`plugin_approval::connect`).
//!
//! Anything the webview sends is data for the daemon's own checks; the daemon still validates
//! every key and value as it does for any caller.

use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Runtime, WebviewWindow};

use crate::plugin_approval::{Guard, connect, own_window};

/// The methods this command makes, and no others.
const ALLOWED: &[&str] = &["config.set", "runtime.set_server", "models.set_config"];

/// How long the call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether this command may make a call to `method`.
fn allowed(method: &str) -> bool {
    ALLOWED.contains(&method)
}

/// Makes `method` on the daemon over the app's own local connection and answers its result.
#[tauri::command]
pub async fn user_call<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    guard: tauri::State<'_, Guard>,
    method: String,
    params: Value,
) -> Result<Value, String> {
    if !own_window(window.label()) {
        return Err("this is called only from the app's own windows".into());
    }
    if !allowed(&method) {
        return Err(format!("{method} is not a call the app makes this way"));
    }
    if let Some(variable) = guard.refused() {
        return Err(format!(
            "{variable} is set in this app's environment, which could change what the app loads: \
             start Imperium without it. Nothing was sent."
        ));
    }
    let mut client = connect(app.package_info().version.to_string()).await?;
    match tokio::time::timeout(CALL_TIMEOUT, client.call_value(&method, params)).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err(format!("{method} did not answer in time")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_settings_calls_are_made_this_way() {
        for method in ["config.set", "runtime.set_server", "models.set_config"] {
            assert!(allowed(method), "{method}");
        }
        for method in [
            "",
            "config.describe",
            "session.create",
            "plugins.approve",
            "mods.install",
            "runtime.install",
            "CONFIG.SET",
            "config.set ",
        ] {
            assert!(!allowed(method), "{method}");
        }
    }

    #[test]
    fn only_the_apps_own_windows_may_call_it() {
        assert!(own_window("main"));
        assert!(own_window("panel-3"));
        assert!(!own_window("mod-frame"));
        assert!(!own_window("panel-"));
    }
}
