//! GitHub auto-fix: watches `[[github.watch]]` repositories for the user's pull requests with
//! failing checks and runs the Fix CI responder for each new failing commit. The fix is made by
//! a session in the pull request's own worktree, committed with an attribution trailer, and
//! pushed to the pull request's branch only after approval (`push = "ask"`, the default) —
//! never force-pushed, merged, or closed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use imperium_config::{GithubWatch, Settings};
use imperium_github::{FailingPr, GitHub, git};
use imperium_protocol::methods::{AUTOFIX_EVENT, AutofixEventParams};

use crate::DaemonState;

/// Tools the fix session may use without asking: it works in its own worktree, and nothing
/// leaves the machine until the push is approved.
const FIX_TOOLS: [&str; 3] = ["write_file", "edit_file", "run_command"];
/// Longest failure log handed to the session.
const LOG_CHARS: usize = 6000;
const FIX_TIMEOUT: Duration = Duration::from_secs(1800);

/// Starts polling every watched repository. Returns immediately.
pub fn start(daemon: &Arc<DaemonState>, github: Arc<dyn GitHub>) {
    let settings = daemon.settings();
    if !settings.github.autofix.enabled || settings.github.watch.is_empty() {
        return;
    }
    let poll = Duration::from_secs(settings.github.autofix.poll_s.max(10));
    tokio::spawn(poll_loop(Arc::downgrade(daemon), github, poll));
}

async fn poll_loop(daemon: Weak<DaemonState>, github: Arc<dyn GitHub>, poll: Duration) {
    // Commits already handled, and attempts per pull request.
    let mut handled: HashMap<(String, u64), String> = HashMap::new();
    let attempts: Arc<Mutex<HashMap<(String, u64), u32>>> = Arc::default();
    loop {
        let Some(state) = daemon.upgrade() else {
            return;
        };
        for watch in state.settings().github.watch.clone() {
            let failing = match github.failing_prs(&watch.repo).await {
                Ok(failing) => failing,
                Err(error) => {
                    tracing::warn!(repo = %watch.repo, %error, "could not list pull requests");
                    continue;
                }
            };
            for pr in failing {
                let key = (watch.repo.clone(), pr.number);
                if handled.get(&key) == Some(&pr.head_sha) {
                    continue;
                }
                handled.insert(key.clone(), pr.head_sha.clone());
                let tries = {
                    let mut attempts = attempts.lock().unwrap_or_else(PoisonError::into_inner);
                    let tries = attempts.entry(key).or_insert(0);
                    *tries += 1;
                    *tries
                };
                let max = state.settings().github.autofix.max_attempts_per_pr;
                if tries > max {
                    announce(
                        &state,
                        &watch.repo,
                        pr.number,
                        "stopped",
                        None,
                        format!(
                            "{} failed again after {max} fix attempts; leaving it to you",
                            pr.checks.join(", ")
                        ),
                    );
                    continue;
                }
                let state = Arc::clone(&state);
                let github = Arc::clone(&github);
                let watch = watch.clone();
                tokio::spawn(async move { fix_ci(&state, github.as_ref(), &watch, &pr).await });
            }
        }
        drop(state);
        tokio::time::sleep(poll).await;
    }
}

/// The Fix CI responder for one failing pull request.
pub async fn fix_ci(
    state: &Arc<DaemonState>,
    github: &dyn GitHub,
    watch: &GithubWatch,
    pr: &FailingPr,
) {
    let repo = &watch.repo;
    let checks = pr.checks.join(", ");
    announce(
        state,
        repo,
        pr.number,
        "started",
        None,
        format!("fixing {checks}"),
    );
    let worktree = worktree_path(&state.settings(), state.data_dir(), repo, pr.number);
    let local_branch = format!("imperium/fix-pr-{}", pr.number);
    if let Err(error) =
        git::prepare_worktree(&watch.project, &worktree, &pr.branch, &local_branch).await
    {
        announce(state, repo, pr.number, "failed", None, error);
        return;
    }
    let log = match github.failure_log(repo, &pr.details_url).await {
        Ok(log) => tail(&log, LOG_CHARS),
        Err(error) => format!("(the log could not be read: {error})"),
    };
    let prompt = format!(
        "Pull request #{number} (branch {branch}) failed these checks: {checks}.\n\n\
         End of the failure log:\n```\n{log}\n```\n\n\
         Find the cause and fix it in this worktree, then run the relevant checks to confirm. \
         Do not commit or push: Imperium commits your changes and asks the user before pushing. \
         Finish with a short summary of the cause and the fix.",
        number = pr.number,
        branch = pr.branch,
    );
    let tools = FIX_TOOLS.iter().map(|tool| tool.to_string()).collect();
    let project = worktree.display().to_string();
    let model = state.settings().github.autofix.model.clone();
    let (ok, summary, session_id) = state
        .run_prompt(
            &project,
            model.as_deref(),
            prompt,
            crate::SessionOptions {
                pre_approved: tools,
                ..crate::SessionOptions::default()
            },
            FIX_TIMEOUT,
        )
        .await;
    if !ok {
        announce(state, repo, pr.number, "failed", session_id, summary);
        return;
    }
    let message = format!(
        "fix: {checks} on #{number}\n\n{summary}\n\nImperium-Auto-Fix: session {session}",
        number = pr.number,
        summary = summary.chars().take(1500).collect::<String>(),
        session = session_id.as_deref().unwrap_or("unknown"),
    );
    match git::commit_all(&worktree, &message).await {
        Ok(true) => {}
        Ok(false) => {
            announce(
                state,
                repo,
                pr.number,
                "nothing",
                session_id,
                format!("the session changed nothing: {summary}"),
            );
            return;
        }
        Err(error) => {
            announce(state, repo, pr.number, "failed", session_id, error);
            return;
        }
    }
    if state.settings().github.autofix.push != "auto" {
        let request = format!(
            "Push the fix for {repo}#{number} ({checks}) to branch {branch}: {summary}",
            number = pr.number,
            branch = pr.branch,
        );
        announce(
            state,
            repo,
            pr.number,
            "awaiting_approval",
            session_id.clone(),
            summary.clone(),
        );
        let approved = state
            .approvals()
            .request(state, "push", request)
            .await
            .unwrap_or(false);
        if !approved {
            announce(
                state,
                repo,
                pr.number,
                "declined",
                session_id,
                "the push was declined; the commit stays in the worktree".into(),
            );
            return;
        }
    }
    match git::push(&worktree, &pr.branch).await {
        Ok(()) => {
            // The fix session now watches the pull request until it merges.
            if let Some(session) = session_id.as_deref().and_then(|id| state.session(id)) {
                state.watch(&session, Some((repo.clone(), pr.number)));
            }
            announce(state, repo, pr.number, "pushed", session_id, summary);
        }
        Err(error) => announce(state, repo, pr.number, "failed", session_id, error),
    }
}

fn worktree_path(
    _settings: &Settings,
    data_dir: &std::path::Path,
    repo: &str,
    number: u64,
) -> PathBuf {
    data_dir
        .join("worktrees")
        .join(repo.replace('/', "__"))
        .join(format!("pr-{number}"))
}

fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(chars)).collect()
}

fn announce(
    state: &DaemonState,
    repo: &str,
    number: u64,
    status: &str,
    session_id: Option<String>,
    summary: String,
) {
    tracing::info!(%repo, pr = number, %status, "auto-fix");
    state.announce(
        AUTOFIX_EVENT,
        AutofixEventParams {
            repo: repo.to_string(),
            pull_request: number,
            status: status.to_string(),
            session_id,
            summary,
        },
    );
}

/// Checks pull requests that sessions watch: merged ones mark their session merged, closed
/// ones end the watch, and either way the session is archived when
/// `sessions.auto_archive_on_pr_close` allows (a merged session still working is archived at a
/// later check, once it is idle). Runs every `poll` while the daemon runs.
pub fn watch_pull_requests(daemon: &Arc<DaemonState>, github: Arc<dyn GitHub>, poll: Duration) {
    let daemon = Arc::downgrade(daemon);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(poll).await;
            let Some(state) = daemon.upgrade() else {
                return;
            };
            for (session_id, repo, number) in state.watched() {
                let Some(session) = state.session(&session_id) else {
                    continue;
                };
                match github.pr_state(&repo, number).await.as_deref() {
                    Ok("MERGED") => {
                        session.set_watch(Some((
                            imperium_protocol::session::SessionStatus::Merged,
                            format!("{repo}#{number}"),
                        )));
                        state.archive_after_pull_request(&session);
                    }
                    Ok("CLOSED") => {
                        state.watch(&session, None);
                        state.archive_after_pull_request(&session);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::debug!(%repo, number, %error, "could not check a watched pull request")
                    }
                }
            }
        }
    });
}
