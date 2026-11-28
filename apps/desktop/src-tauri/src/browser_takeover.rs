//! The user taking over a session's browser from the Browser panel: the one place the desktop app
//! asks the daemon for `session.browser_control` and `session.browser_act`
//! (docs/07-desktop-ui.md#preview-and-browser-panels-implemented,
//! docs/05-tools-and-isolation.md#the-browsers-debugger-pipe-and-profile-implemented).
//!
//! The session's browser is signed in to whatever the user signed in to, and while the user
//! drives it no approval is asked. So the daemon answers the two calls only to this app's own
//! process on the local socket (and never on a server), as it does for turning auto-merge on. The
//! webview cannot make that connection: its own one is the loopback socket with the token, which
//! any mod's page could also open. So the webview asks the app, in Rust, which sends the call on
//! its own connection:
//!
//! 1. the commands are callable only from the app's own windows (`plugin_approval::own_window`,
//!    and the access control list: `build.rs` and `capabilities/default.json`);
//! 2. the app's own environment is checked at start for the loader and WebView2 overrides the
//!    daemon refuses in a peer; if one is set nothing is sent;
//! 3. the session id is checked to be plain text, and the action is one the protocol knows
//!    (`BrowserAction`, decoded here, so nothing else reaches the daemon).
//!
//! No dialog is shown: the user is driving the page they see, action by action. The connection
//! is kept between actions (one per app), and made again after a failure.

use std::time::Duration;

use imperium_client::Client;
use imperium_protocol::methods::{
    BrowserActParams, BrowserActResult, BrowserAction, BrowserControlParams, BrowserControlResult,
    SessionBrowserAct, SessionBrowserControl,
};
use tauri::{AppHandle, Runtime, WebviewWindow};
use tokio::sync::Mutex;

use crate::plugin_approval::{Guard, ask, connect, own_window};

/// How long taking the browser over may take: the daemon may start the browser first.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(150);
/// How long one action may take: the page loads, then a screenshot is taken.
const ACT_TIMEOUT: Duration = Duration::from_secs(60);

/// The app's connection to the daemon for the browser, made on first use.
#[derive(Default)]
pub struct Takeover {
    client: Mutex<Option<Client>>,
}

/// Whether `session` is a plain-text session id: 1 to 256 characters, none a control character.
fn plain_id(session: &str) -> bool {
    !session.is_empty() && session.chars().count() <= 256 && !session.chars().any(char::is_control)
}

/// The checks every call makes before anything is sent (steps 1 to 3).
fn check(window: &str, guard: &Guard, session: &str) -> Result<(), String> {
    if !own_window(window) {
        return Err("the browser is driven only from the app's own windows".into());
    }
    if let Some(variable) = guard.refused() {
        return Err(format!(
            "{variable} is set in this app's environment, which could change what the app loads: \
             start Imperium without it"
        ));
    }
    if !plain_id(session) {
        return Err("the session id must be 1 to 256 characters of plain text".into());
    }
    Ok(())
}

impl Takeover {
    /// Calls `M` on the kept connection, making it first if there is none. A failed call drops
    /// the connection, so the next one starts afresh.
    async fn call<M: imperium_protocol::methods::Method>(
        &self,
        version: String,
        params: M::Params,
        limit: Duration,
    ) -> Result<M::Result, String> {
        let mut held = self.client.lock().await;
        if held.is_none() {
            *held = Some(connect(version).await?);
        }
        let Some(client) = held.as_mut() else {
            return Err("imperium-core is not connected".into());
        };
        let answer = ask::<M>(client, params, limit).await;
        if answer.is_err() {
            *held = None;
        }
        answer
    }
}

/// Takes the session's browser over (`user` true) or gives it back to the agent.
#[tauri::command]
pub async fn browser_control<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    guard: tauri::State<'_, Guard>,
    takeover: tauri::State<'_, Takeover>,
    session: String,
    user: bool,
) -> Result<BrowserControlResult, String> {
    check(window.label(), &guard, &session)?;
    takeover
        .call::<SessionBrowserControl>(
            app.package_info().version.to_string(),
            BrowserControlParams {
                session_id: session,
                user,
            },
            CONTROL_TIMEOUT,
        )
        .await
}

/// Does what the user did in the session's browser while driving it.
#[tauri::command]
pub async fn browser_act<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    guard: tauri::State<'_, Guard>,
    takeover: tauri::State<'_, Takeover>,
    session: String,
    action: BrowserAction,
) -> Result<BrowserActResult, String> {
    check(window.label(), &guard, &session)?;
    takeover
        .call::<SessionBrowserAct>(
            app.package_info().version.to_string(),
            BrowserActParams {
                session_id: session,
                action,
            },
            ACT_TIMEOUT,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_session_ids_are_sent() {
        assert!(plain_id("3fa2c1d0-aaaa"));
        assert!(!plain_id(""));
        assert!(!plain_id("a\nb"));
        assert!(!plain_id("a\u{0}b"));
        assert!(!plain_id(&"x".repeat(257)));
        assert!(plain_id(&"x".repeat(256)));
    }

    #[test]
    fn a_mod_frame_or_another_window_is_refused_before_anything_is_sent() {
        let guard = Guard::capture();
        if guard.refused().is_some() {
            // The test runner's own environment sets a variable the daemon refuses.
            return;
        }
        assert!(check("main", &guard, "s1").is_ok());
        assert!(check("panel-browser", &guard, "s1").is_ok());
        for window in ["mod-frame", "panel-", "other"] {
            let error = check(window, &guard, "s1").unwrap_err();
            assert!(error.contains("app's own windows"), "{window}: {error}");
        }
        assert!(check("main", &guard, "a\nb").is_err());
    }
}
