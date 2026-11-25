//! `imperium pr status` and `imperium pr automerge`: the pull requests bound to sessions, as the
//! desktop's card shows them, and whether GitHub's auto-merge is on for one. The command line can
//! show auto-merge and turn it off, never on: asking GitHub to auto-merge lands code without
//! another look, so only the desktop app can (docs/14-projects-and-automation.md#github-auto-fix).
//! There is no call in this module that enables it.
//!
//! The daemon's cached `PrStatus` is the only source. Every text in it was written on GitHub, so
//! it is printed escaped: a control character (a terminal escape sequence, a carriage return) or
//! a character that changes the text's direction would otherwise act on the terminal or reorder
//! what the reader sees. They print as `\u{..}` instead.

use std::fmt::Write as _;

use anyhow::anyhow;
use imperium_protocol::methods::{
    PrConflictState, PrMergeMethod, PrReview, PrState, PrStatus, PullRequestsSetAutoMerge,
    PullRequestsSetAutoMergeParams, PullRequestsStatus, PullRequestsStatusParams, SessionList,
    SessionListParams,
};

use crate::client::Client;

/// What `imperium pr status [SESSION]` prints: one session's pull request, or with no session
/// every running session that has one bound.
pub async fn status(client: &mut Client, session: Option<&str>) -> anyhow::Result<String> {
    if let Some(id) = session {
        let status = read(client, id)
            .await
            .map_err(|error| anyhow!(escape(&error)))?;
        return Ok(render(&status));
    }
    let listing = client.call::<SessionList>(SessionListParams {}).await?;
    let mut blocks = Vec::new();
    for summary in listing
        .sessions
        .iter()
        .filter(|summary| !summary.archived && summary.pull_request_url.is_some())
    {
        let body = match read(client, &summary.session_id).await {
            Ok(status) => render(&status),
            Err(error) => format!(
                "{}\n  could not be read: {}\n",
                escape(summary.pull_request_url.as_deref().unwrap_or_default()),
                escape(&error)
            ),
        };
        blocks.push(format!("session {}\n{body}", escape(&summary.session_id)));
    }
    if blocks.is_empty() {
        return Ok("No session has a pull request bound.\n".to_string());
    }
    Ok(blocks.join("\n"))
}

/// What `imperium pr automerge status SESSION` prints: whether auto-merge is on for the session's
/// pull request, with the method, or why it cannot be turned on.
pub async fn automerge_status(client: &mut Client, session: &str) -> anyhow::Result<String> {
    let status = read(client, session)
        .await
        .map_err(|error| anyhow!(escape(&error)))?;
    Ok(format!(
        "{}#{}: auto-merge {}\n",
        escape(&status.repo),
        status.number,
        auto_merge_text(&status)
    ))
}

/// What `imperium pr automerge off SESSION` prints after it asks the daemon to turn auto-merge
/// off. Nothing is sent to GitHub when it was already off.
pub async fn automerge_off(client: &mut Client, session: &str) -> anyhow::Result<String> {
    let status = read(client, session)
        .await
        .map_err(|error| anyhow!(escape(&error)))?;
    let after = client
        .call::<PullRequestsSetAutoMerge>(PullRequestsSetAutoMergeParams {
            session_id: session.to_string(),
            url: status.url.clone(),
            head_sha: status.head_sha.clone(),
            enabled: false,
            merge_method: None,
            challenge: None,
        })
        .await
        .map_err(|error| anyhow!(escape(&error.to_string())))?;
    Ok(format!(
        "{}#{}: auto-merge {}\n",
        escape(&after.repo),
        after.number,
        auto_merge_text(&after)
    ))
}

/// `on (squash)`, `off`, or `off: <why it cannot be turned on>`; the reason is GitHub's or the
/// daemon's text, so it is escaped.
fn auto_merge_text(status: &PrStatus) -> String {
    let mut text = on_off(status.auto_merge.enabled).to_string();
    if let Some(chosen) = status.auto_merge.method {
        let _ = write!(text, " ({})", method(chosen));
    }
    if let Some(reason) = &status.auto_merge.reason_unavailable {
        let _ = write!(text, ": {}", escape(reason));
    }
    text
}

async fn read(client: &mut Client, session_id: &str) -> Result<PrStatus, String> {
    client
        .call::<PullRequestsStatus>(PullRequestsStatusParams {
            session_id: session_id.to_string(),
        })
        .await
        .map_err(|error| error.to_string())
}

/// A pull request's status as lines of text, every GitHub-supplied part escaped.
pub fn render(status: &PrStatus) -> String {
    let mut out = String::new();
    let state = match (status.state, status.draft) {
        (PrState::Open, true) | (PrState::Draft, _) => "draft",
        (PrState::Open, false) => "open",
        (PrState::Merged, _) => "merged",
        (PrState::Closed, _) => "closed",
    };
    let _ = writeln!(
        out,
        "{}#{}  {state}  {}",
        escape(&status.repo),
        status.number,
        escape(&status.url)
    );
    let _ = writeln!(out, "  title:      {}", escape(&status.title));
    let _ = writeln!(
        out,
        "  branch:     {} into {}",
        escape(&status.branch),
        escape(&status.base)
    );
    let checks = &status.checks;
    let mut line = format!(
        "{} passed, {} failed, {} running",
        checks.passed, checks.failed, checks.pending
    );
    if !checks.failing_names.is_empty() {
        let names: Vec<String> = checks.failing_names.iter().map(|n| escape(n)).collect();
        let _ = write!(line, " (failing: {})", names.join(", "));
    }
    let _ = writeln!(out, "  checks:     {line}");
    let _ = writeln!(
        out,
        "  review:     {}",
        match status.review {
            PrReview::Approved => "approved",
            PrReview::ChangesRequested => "changes requested",
            PrReview::ReviewRequired => "review required",
            PrReview::None => "no review yet",
        }
    );
    let _ = writeln!(
        out,
        "  conflicts:  {} (merge state: {})",
        match status.mergeable {
            PrConflictState::Mergeable => "none",
            PrConflictState::Conflicting => "has conflicts",
            PrConflictState::Unknown => "not known yet",
        },
        escape(&status.merge_state)
    );
    let methods: Vec<&str> = status.allowed_methods.iter().map(|m| method(*m)).collect();
    let _ = writeln!(
        out,
        "  methods:    {}",
        if methods.is_empty() {
            "not known (the repository's settings could not be read)".to_string()
        } else {
            methods.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "  auto-fix:   {} (not available yet)",
        on_off(status.auto_fix)
    );
    let _ = writeln!(out, "  auto-merge: {}", auto_merge_text(status));
    if let Some(error) = &status.error {
        let _ = writeln!(
            out,
            "  error:      {} (the rest is the last status that was read)",
            escape(error)
        );
    }
    out
}

fn method(method: PrMergeMethod) -> &'static str {
    match method {
        PrMergeMethod::Squash => "squash",
        PrMergeMethod::Merge => "merge",
        PrMergeMethod::Rebase => "rebase",
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Whether a terminal should not be sent this character as it is: a control character, or one
/// that is invisible or changes the direction of the text around it.
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{61c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        )
}

/// `text` with every control and direction character written as `\u{..}`.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe(c) {
            let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_terminal_and_direction_characters() {
        assert_eq!(escape("plain text, ünïcode ✓"), "plain text, ünïcode ✓");
        assert_eq!(escape("a\u{1b}[2Jb"), "a\\u{1b}[2Jb");
        assert_eq!(escape("a\rb\nc\td"), "a\\u{d}b\\u{a}c\\u{9}d");
        assert_eq!(
            escape("x\u{202e}y\u{2066}z\u{200b}"),
            "x\\u{202e}y\\u{2066}z\\u{200b}"
        );
        assert_eq!(escape("\u{9b}\u{7f}\u{feff}"), "\\u{9b}\\u{7f}\\u{feff}");
    }
}
