//! Turning GitHub's auto-merge on for a session's pull request: the one place the desktop app asks
//! the daemon for it (docs/07-desktop-ui.md#pull-request-card-implemented,
//! docs/14-projects-and-automation.md#github-auto-fix).
//!
//! Auto-merge lands code without another look, so the daemon turns it on only for this app's own
//! process, over the local socket, with a single-use challenge it issued to the same connection
//! for exactly this pull request, head commit and merge method. The whole flow stays in Rust and
//! the webview only triggers it, as plugin approval does:
//!
//! 1. the command is callable only from the app's own windows (`plugin_approval::own_window`, and
//!    the access control list: `build.rs` and `capabilities/default.json`);
//! 2. the app's own environment is checked at start for the loader and WebView2 overrides the
//!    daemon refuses in a peer; if one is set nothing is sent;
//! 3. the app connects on the local socket, never loopback TCP, and reads the pull request's
//!    status from the daemon: the dialog is written from that, never from text the webview sent,
//!    and the head commit the card showed must still be the daemon's current one;
//! 4. a native dialog names the repository, the pull request, the base and head branches, the
//!    commit and the merge method, and says that GitHub then merges without anyone looking again;
//! 5. on *Turn on auto-merge* the app requests a challenge for exactly that and answers it with
//!    `pull_requests.set_auto_merge`, on that one connection.
//!
//! Turning it off needs none of this: the webview asks the daemon itself.

use std::time::Duration;

use imperium_protocol::methods::{
    PrMergeMethod, PrStatus, PullRequestsAutoMergeChallenge, PullRequestsAutoMergeChallengeParams,
    PullRequestsSetAutoMerge, PullRequestsSetAutoMergeParams, PullRequestsStatus,
    PullRequestsStatusParams,
};
use serde::Serialize;
use tauri::{AppHandle, Runtime, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::plugin_approval::{Guard, ask, connect, own_window, show};

/// How long a quick call (status, challenge) may take.
const QUICK_TIMEOUT: Duration = Duration::from_secs(15);
/// How long turning it on may take: the daemon reads GitHub, asks it, and reads it again.
const ENABLE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the webview is told.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Outcome {
    /// Auto-merge is now on.
    pub enabled: bool,
    /// The user closed the dialog without confirming: nothing was asked of GitHub.
    pub cancelled: bool,
    /// Why it was not turned on.
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

fn method_name(method: PrMergeMethod) -> &'static str {
    match method {
        PrMergeMethod::Squash => "Squash and merge",
        PrMergeMethod::Merge => "Create a merge commit",
        PrMergeMethod::Rebase => "Rebase and merge",
    }
}

/// What the dialog says, from the daemon's status alone. Every GitHub-supplied part goes through
/// `show`, so control and direction-changing characters cannot change how it reads.
fn confirmation(status: &PrStatus, method: PrMergeMethod) -> String {
    let commit: String = status.head_sha.chars().take(12).collect();
    format!(
        "Turn on auto-merge for {repo}#{number}?\n\n\
         Branch: {branch} into {base}\n\
         Commit: {commit}\n\
         Method: {method}\n\n\
         GitHub will merge this pull request as soon as its own required checks and reviews pass, \
         with nobody looking at the code again. If the branch changes first, GitHub refuses. \
         Imperium never merges it itself, and turns this off before it pushes to the branch.",
        repo = show(&status.repo),
        number = status.number,
        branch = show(&status.branch),
        base = show(&status.base),
        method = method_name(method),
    )
}

/// Why this pull request cannot be offered auto-merge now, from the daemon's own status.
fn unavailable(status: &PrStatus, shown: &str) -> Option<String> {
    if status.head_sha != shown {
        return Some(
            "the branch moved since the card showed it: look at the pull request again".into(),
        );
    }
    if let Some(reason) = &status.auto_merge.reason_unavailable {
        return Some(show(reason));
    }
    if status.auto_merge.enabled {
        return Some("auto-merge is already on".into());
    }
    None
}

/// Shows the dialog and waits: `true` for *Turn on auto-merge*, `false` for *Cancel* or closing it.
async fn confirm<R: Runtime>(app: &AppHandle<R>, window: &WebviewWindow<R>, text: String) -> bool {
    let (answer, answered) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(text)
        .title("Turn on auto-merge")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Turn on auto-merge".into(),
            "Cancel".into(),
        ))
        .parent(window)
        .show(move |confirmed| {
            let _ = answer.send(confirmed);
        });
    answered.await.unwrap_or(false)
}

async fn flow<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    session: &str,
    head: &str,
    method: PrMergeMethod,
) -> Result<Outcome, String> {
    let mut client = connect(app.package_info().version.to_string()).await?;
    let status = ask::<PullRequestsStatus>(
        &mut client,
        PullRequestsStatusParams {
            session_id: session.to_string(),
        },
        QUICK_TIMEOUT,
    )
    .await?;
    if let Some(reason) = unavailable(&status, head) {
        return Ok(Outcome::refused(reason));
    }
    if !status.allowed_methods.contains(&method) {
        return Ok(Outcome::refused(format!(
            "{} is not allowed in this repository",
            method_name(method)
        )));
    }
    if !confirm(app, window, confirmation(&status, method)).await {
        return Ok(Outcome {
            cancelled: true,
            reason: Some("cancelled: nothing was asked of GitHub".into()),
            ..Outcome::default()
        });
    }
    // The challenge is for what the dialog showed: the daemon's address and head, this method.
    let challenge = ask::<PullRequestsAutoMergeChallenge>(
        &mut client,
        PullRequestsAutoMergeChallengeParams {
            session_id: session.to_string(),
            url: status.url.clone(),
            head_sha: status.head_sha.clone(),
            merge_method: method,
        },
        QUICK_TIMEOUT,
    )
    .await?;
    let answer = ask::<PullRequestsSetAutoMerge>(
        &mut client,
        PullRequestsSetAutoMergeParams {
            session_id: session.to_string(),
            url: status.url.clone(),
            head_sha: status.head_sha.clone(),
            enabled: true,
            merge_method: Some(method),
            challenge: Some(challenge.challenge),
        },
        ENABLE_TIMEOUT,
    )
    .await;
    Ok(match answer {
        Ok(after) => Outcome {
            enabled: after.auto_merge.enabled,
            ..Outcome::default()
        },
        Err(reason) => Outcome::refused(reason),
    })
}

/// Turns auto-merge on for a session's pull request after the user confirms it in a native
/// dialog. The webview names the session, the head commit the card showed and the method; the
/// challenge, the dialog and the request never pass through it.
#[tauri::command]
pub async fn auto_merge_enable<R: Runtime>(
    window: WebviewWindow<R>,
    app: AppHandle<R>,
    guard: tauri::State<'_, Guard>,
    session: String,
    head: String,
    method: PrMergeMethod,
) -> Result<Outcome, String> {
    if !own_window(window.label()) {
        return Err("auto-merge is turned on only from the app's own windows".into());
    }
    if let Some(variable) = guard.refused() {
        return Ok(Outcome::refused(format!(
            "{variable} is set in this app's environment, which could change what the app loads: \
             start Imperium without it. Nothing was sent."
        )));
    }
    if session.is_empty() || session.chars().count() > 256 || session.chars().any(char::is_control)
    {
        return Err("the session id must be 1 to 256 characters of plain text".into());
    }
    if head.is_empty() || head.chars().count() > 64 || !head.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err("the commit must be a hexadecimal id".into());
    }
    flow(&app, &window, &session, &head, method).await
}

#[cfg(test)]
mod tests {
    use imperium_protocol::methods::{PrAutoMerge, PrChecks, PrConflictState, PrReview, PrState};

    use super::*;

    fn status() -> PrStatus {
        PrStatus {
            url: "https://github.com/owner/repo/pull/7".into(),
            repo: "owner/repo".into(),
            number: 7,
            title: "Add the thing".into(),
            state: PrState::Open,
            draft: false,
            head_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            base: "main".into(),
            branch: "feat/\u{202e}evil\nbranch".into(),
            checks: PrChecks {
                passed: 1,
                failed: 0,
                pending: 0,
                failing_names: Vec::new(),
            },
            review: PrReview::Approved,
            mergeable: PrConflictState::Mergeable,
            merge_state: "blocked".into(),
            allowed_methods: vec![PrMergeMethod::Squash, PrMergeMethod::Merge],
            auto_fix: false,
            auto_merge: PrAutoMerge {
                enabled: false,
                method: None,
                reason_unavailable: None,
            },
            auto_archive_on_close: false,
            untrusted_handled: false,
            updated_at: 0,
            error: None,
        }
    }

    #[test]
    fn the_dialog_names_the_method_the_branches_and_the_commit_and_says_it_lands_code_unseen() {
        let text = confirmation(&status(), PrMergeMethod::Merge);
        assert!(text.contains("owner/repo#7"));
        assert!(text.contains("Create a merge commit"));
        assert!(text.contains("into main"));
        assert!(text.contains("0123456789ab"));
        assert!(text.contains("nobody looking at the code again"));
        // The branch name is GitHub's text: its newline and direction override are written out.
        assert!(text.contains("feat/\\u{202e}evil\\nbranch"), "{text}");
        assert!(!text.contains('\u{202e}'));
        assert!(!text.contains("evil\nbranch"));
    }

    #[test]
    fn a_moved_branch_a_refusal_and_an_existing_request_are_not_offered() {
        let mut status = status();
        let head = status.head_sha.clone();
        assert_eq!(unavailable(&status, &head), None);
        assert!(
            unavailable(&status, "deadbeef")
                .unwrap()
                .contains("branch moved")
        );
        status.auto_merge.reason_unavailable =
            Some("the repository does not allow auto-merge".into());
        assert!(
            unavailable(&status, &head)
                .unwrap()
                .contains("does not allow")
        );
        status.auto_merge.reason_unavailable = None;
        status.auto_merge.enabled = true;
        assert!(unavailable(&status, &head).unwrap().contains("already on"));
    }
}
