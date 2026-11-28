//! Permanently deleting a session: the one place the desktop app asks the daemon for it
//! (docs/07-desktop-ui.md#sessions-panel, docs/14-projects-and-automation.md#archiving-sessions).
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
//! 3. the session id is checked to be plain text, and the daemon is asked for that session's own
//!    title and state;
//! 4. a native dialog, which the webview cannot answer, names that session and says the delete
//!    is permanent. The webview's own dialog is for the user's convenience only: a script in the
//!    page could skip it, never this one.
//!
//! The daemon refuses an environment's persistent agent and any unknown session itself.

use std::time::Duration;

use imperium_protocol::methods::{
    SessionErase, SessionEraseParams, SessionList, SessionListParams,
};
use serde::Serialize;
use tauri::{AppHandle, Runtime, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::plugin_approval::{Guard, ask, connect, own_window, show};

/// How long reading the session list may take.
const QUICK_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the delete may take: the daemon stops the session and removes its files.
const TIMEOUT: Duration = Duration::from_secs(60);

/// What the webview is told.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Outcome {
    /// The session was deleted.
    pub erased: bool,
    /// The user closed the native dialog without confirming: nothing was deleted.
    pub cancelled: bool,
    /// Why it was not.
    pub reason: Option<String>,
}

impl Outcome {
    fn refused(reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
            ..Self::default()
        }
    }
}

/// Whether `session` is a plain-text session id: 1 to 256 characters, none a control character.
fn plain_id(session: &str) -> bool {
    !session.is_empty() && session.chars().count() <= 256 && !session.chars().any(char::is_control)
}

/// What the native dialog says, from the daemon's own record of the session.
fn confirmation(title: &str, archived: bool) -> String {
    let name = if title.trim().is_empty() {
        "Untitled session".to_string()
    } else {
        show(title)
    };
    format!(
        "Delete the {kind}session \u{201c}{name}\u{201d}?\n\n\
         This permanently deletes it, its transcript and its stored session data. It cannot be \
         undone. A running session is stopped first. Files in your project folders are not \
         touched.",
        kind = if archived { "archived " } else { "" },
    )
}

/// Shows the dialog and waits: `true` for *Delete session*, `false` for *Cancel* or closing it.
async fn confirm<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>, text: String) -> bool {
    let (answer, answered) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(text)
        .title("Delete session")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Delete session".into(),
            "Cancel".into(),
        ))
        .parent(window)
        .show(move |confirmed| {
            let _ = answer.send(confirmed);
        });
    answered.await.unwrap_or(false)
}

/// Deletes a session, running or archived, after a native confirmation that names it. The webview
/// names only the session.
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
    let listed = ask::<SessionList>(&mut client, SessionListParams {}, QUICK_TIMEOUT).await?;
    let Some(known) = listed
        .sessions
        .iter()
        .find(|known| known.session_id == session)
    else {
        return Ok(Outcome::refused("no such session"));
    };
    if !confirm(&app, &window, confirmation(&known.title, known.archived)).await {
        return Ok(Outcome {
            cancelled: true,
            reason: Some("cancelled: nothing was deleted".into()),
            ..Outcome::default()
        });
    }
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
                ..Outcome::default()
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
    fn the_dialog_names_the_session_and_says_it_is_permanent() {
        let text = confirmation("Fix the\u{202e} build\n", false);
        assert!(text.contains("Fix the"));
        assert!(text.contains("permanently"));
        assert!(text.contains("cannot be undone"));
        assert!(!text.contains('\u{202e}'));
        assert!(!text.contains("build\n\u{201d}"));
        assert!(confirmation("x", true).contains("archived session"));
        assert!(confirmation("  ", false).contains("Untitled session"));
    }

    #[test]
    fn a_refusal_names_why_and_deleted_nothing() {
        let outcome = Outcome::refused("no session x");
        assert!(!outcome.erased);
        assert_eq!(outcome.reason.as_deref(), Some("no session x"));
    }
}
