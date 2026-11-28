//! Permanently deleting a session: the one place the desktop app asks the daemon for it
//! (docs/07-desktop-ui.md#sessions-panel, docs/14-projects-and-automation.md#archiving).
//!
//! `session.erase` removes a session's transcript and stored data for good, so the daemon answers
//! it only to this app's own process on the local socket (and never on a server), the way it does
//! for turning auto-merge on. The webview cannot make that connection: its own one is the
//! loopback socket with the token, which any mod's page could also open. So the webview asks the
//! app, in Rust, which connects on the local socket and sends the one call:
//!
//! 1. the command is callable only from the app's own windows (`plugin_approval::own_window`, and
//!    the access control list: `build.rs` and `capabilities/default.json`);
//! 2. the app's own environment is checked at start for the loader and WebView2 overrides the
//!    daemon refuses in a peer; if one is set nothing is sent;
//! 3. the session id is checked to be plain text before it is sent.
//!
//! The confirmation, which names the session, is the webview's own dialog (it shows only what the
//! user is about to delete), and the daemon refuses an environment's persistent agent and any
//! unknown session itself.

use std::time::Duration;

use imperium_protocol::methods::{SessionErase, SessionEraseParams};
use serde::Serialize;
use tauri::{AppHandle, Runtime, WebviewWindow};

use crate::plugin_approval::{Guard, ask, connect, own_window};

/// How long the delete may take: it removes a session's files.
const TIMEOUT: Duration = Duration::from_secs(60);

/// What the webview is told.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Outcome {
    /// The session was deleted.
    pub erased: bool,
    /// Why it was not.
    pub reason: Option<String>,
}

impl Outcome {
    fn refused(reason: impl Into<String>) -> Self {
        Self {
            erased: false,
            reason: Some(reason.into()),
        }
    }
}

/// Whether `session` is a plain-text session id: 1 to 256 characters, none a control character.
fn plain_id(session: &str) -> bool {
    !session.is_empty() && session.chars().count() <= 256 && !session.chars().any(char::is_control)
}

/// Deletes a session, running or archived, after the webview's own confirmation. The webview names
/// only the session.
#[tauri::command]
pub async fn session_erase<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    guard: tauri::State<'_, Guard>,
    session: String,
) -> Result<Outcome, String> {
    if !own_window(window.label()) {
        return Err("a session is deleted only from the app's own windows".into());
    }
    if let Some(variable) = guard.refused() {
        return Ok(Outcome::refused(format!(
            "{variable} is set in this app's environment, which could change what the app loads: \
             start Imperium without it. Nothing was deleted."
        )));
    }
    if !plain_id(&session) {
        return Err("the session id must be 1 to 256 characters of plain text".into());
    }
    let mut client = connect(app.package_info().version.to_string()).await?;
    Ok(
        match ask::<SessionErase>(
            &mut client,
            SessionEraseParams {
                session_id: session,
            },
            TIMEOUT,
        )
        .await
        {
            Ok(_) => Outcome {
                erased: true,
                reason: None,
            },
            Err(reason) => Outcome::refused(reason),
        },
    )
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
    fn a_refusal_names_why_and_deleted_nothing() {
        let outcome = Outcome::refused("no session x");
        assert!(!outcome.erased);
        assert_eq!(outcome.reason.as_deref(), Some("no session x"));
    }
}
