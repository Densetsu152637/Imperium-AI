//! The review pane (roadmap item 6): a session's changes since its first checkpoint as diff
//! hunks; accepting a hunk hides it without touching the working tree, rejecting one reverts it
//! there; whole files are accepted or rejected too; committing asks unless a rule allows it,
//! is refused when a rule denies it, and commits only the session's files; opening a pull
//! request asks first too.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::methods::{
    SessionReviewCommitResult, SessionReviewHunkResult, SessionReviewPullRequestResult,
    SessionReviewResult,
};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use serde_json::{Value, json};
use tokio::sync::broadcast;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

const BEFORE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n";
const AFTER: &str = "1\nTWO\n3\n4\n5\n6\n7\n8\n9\n10\n11\nTWELVE\n";

fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    // The user's own git settings must not leak into the commits the test checks.
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "core.autocrlf", "false"]);
    std::fs::write(dir.join("a.txt"), BEFORE).unwrap();
    std::fs::write(dir.join("b.txt"), "keep\n").unwrap();
    std::fs::write(dir.join("user.txt"), "the user's\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
    repo
}

/// `edit` deletes `b.txt` and writes `a.txt` (two far-apart changes) and `new.txt`.
async fn serve(repo: PathBuf) -> String {
    let repo = Arc::new(repo);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let repo = Arc::clone(&repo);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let last = messages.last().cloned().unwrap_or_default();
                let said = last["content"].as_str().unwrap_or_default().to_string();
                let delta = if last["role"] == "tool" {
                    json!({"content": "done"})
                } else if last["role"] == "user" && said.trim() == "edit" {
                    let _ = std::fs::remove_file(repo.join("b.txt"));
                    let call = |index: u32, path: &str, content: &str| {
                        json!({"index": index, "id": format!("w{index}"), "function": {
                            "name": "write_file",
                            "arguments": json!({"path": path, "content": content}).to_string()}})
                    };
                    json!({"tool_calls": [call(0, "a.txt", AFTER), call(1, "new.txt", "new\n")]})
                } else if last["role"] == "user" && said.trim() == "glob" {
                    json!({"tool_calls": [{"index": 0, "id": "g0", "function": {
                        "name": "write_file",
                        "arguments": json!({"path": "[ab].txt", "content": "glob\n"}).to_string()}}]})
                } else if last["role"] == "user" && said.trim() == "slow" {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    json!({"content": "ok"})
                } else {
                    json!({"content": "ok"})
                };
                let chunk = json!({"choices": [{"delta": delta}]});
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn daemon(home: &Path, url: &str, commit_rule: Option<&str>) -> Arc<DaemonState> {
    let extra: Vec<(&str, &str)> = commit_rule
        .map(|action| vec![("git_commit", action)])
        .unwrap_or_default();
    daemon_with(home, url, &extra)
}

/// A daemon whose permission rules allow `write_file` plus `extra` (tool, action) rules.
fn daemon_with(home: &Path, url: &str, extra: &[(&str, &str)]) -> Arc<DaemonState> {
    let mut rules = String::from("{ tool = \"write_file\", action = \"allow\" }");
    for (tool, action) in extra {
        rules.push_str(&format!(", {{ tool = \"{tool}\", action = \"{action}\" }}"));
    }
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{url}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n\
         [permissions]\nrules = [{rules}]\n"
    );
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    DaemonState::new(imperium_config::Paths::under(home), settings, None)
}

async fn call(connection: &mut Connection, method: &str, params: Value) -> Result<Value, String> {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response: Response = match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(30), deferred)
            .await
            .expect("a reply within 30 s"),
        (None, None) => panic!("no reply to {method}"),
    };
    match response.outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => Err(error.message),
    }
}

async fn ok<T: serde::de::DeserializeOwned>(
    connection: &mut Connection,
    method: &str,
    params: Value,
) -> T {
    let value = call(connection, method, params)
        .await
        .unwrap_or_else(|error| panic!("{method}: {error}"));
    serde_json::from_value(value).unwrap()
}

async fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    call(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 13}}),
    )
    .await
    .unwrap();
    connection
}

/// Starts a session in `dir` and runs the `edit` turn, until its checkpoint is kept.
async fn edited(state: &Arc<DaemonState>, dir: &Path) -> String {
    let (session, _) = state
        .create_session(&dir.display().to_string(), None)
        .unwrap();
    let mut events: broadcast::Receiver<AgentEvent> = session.subscribe();
    let mut announced = state.announcements();
    session.send("edit".into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        match event.event {
            SessionEvent::TurnCompleted { .. } if event.agent_id == ROOT_AGENT => break,
            SessionEvent::TurnFailed { message } => panic!("the turn failed: {message}"),
            _ => {}
        }
    }
    loop {
        let notification = tokio::time::timeout(Duration::from_secs(20), announced.recv())
            .await
            .expect("a checkpoint within 20 s")
            .unwrap();
        if notification.method == "session.checkpoint" {
            break;
        }
    }
    session.id().to_string()
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn hunks_are_accepted_and_rejected_and_the_rest_committed_after_asking() {
    let repo = repository();
    let dir = repo.path().to_path_buf();
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url, None);
    let mut connection = connect(&state).await;
    let id = edited(&state, &dir).await;
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let index = git(&dir, &["ls-files", "--stage"]);
    let review: SessionReviewResult =
        ok(&mut connection, "session.review", json!({"session_id": id})).await;
    assert!(review.available);
    assert_eq!(review.since, Some(1));
    assert_eq!(review.repos.len(), 1);
    let root = review.repos[0].clone();
    let paths: Vec<(&str, &str)> = review
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status.as_str()))
        .collect();
    assert_eq!(
        paths,
        [
            ("a.txt", "modified"),
            ("b.txt", "deleted"),
            ("new.txt", "added")
        ]
    );
    let a = &review.files[0];
    assert_eq!(a.hunks.len(), 2, "far-apart changes are separate hunks");
    assert_eq!(a.hunks[0].header, "@@ -1,5 +1,5 @@");
    assert!(a.hunks[0].lines.contains(&"-2".to_string()));
    assert!(a.hunks[0].lines.contains(&"+TWO".to_string()));
    assert!(review.suggested_message.contains("- a.txt"));

    // Accepting the first hunk hides it; the file is untouched.
    let accepted: SessionReviewHunkResult = ok(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "a.txt",
               "hunk": a.hunks[0].id, "action": "accept"}),
    )
    .await;
    let left = accepted.file.expect("one hunk left");
    assert_eq!(left.hunks.len(), 1);
    assert!(left.hunks[0].lines.contains(&"+TWELVE".to_string()));
    assert_eq!(read(dir.join("a.txt")), AFTER);

    // A stale hunk id is refused.
    let stale = call(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "a.txt",
               "hunk": a.hunks[0].id, "action": "reject"}),
    )
    .await;
    assert!(stale.unwrap_err().contains("changed since"));

    // Rejecting the other reverts it in the working tree, and leaves nothing to review.
    let rejected: SessionReviewHunkResult = ok(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "a.txt",
               "hunk": left.hunks[0].id, "action": "reject"}),
    )
    .await;
    assert!(rejected.file.is_none());
    assert_eq!(
        read(dir.join("a.txt")),
        "1\nTWO\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n"
    );

    // Whole files: the deletion is rejected (the file comes back), the new file accepted.
    let shown: SessionReviewResult =
        ok(&mut connection, "session.review", json!({"session_id": id})).await;
    let file_id = |path: &str| {
        let file = shown.files.iter().find(|file| file.path == path).unwrap();
        format!("file:{}", file.id)
    };
    ok::<SessionReviewHunkResult>(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "b.txt",
               "hunk": file_id("b.txt"), "action": "reject"}),
    )
    .await;
    assert_eq!(read(dir.join("b.txt")), "keep\n");
    // A whole-file action on a file that changed since it was shown is refused.
    std::fs::write(dir.join("new.txt"), "newer\n").unwrap();
    let stale = call(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "new.txt",
               "hunk": file_id("new.txt"), "action": "reject"}),
    )
    .await;
    assert!(stale.unwrap_err().contains("changed since"));
    assert_eq!(read(dir.join("new.txt")), "newer\n");
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    ok::<SessionReviewHunkResult>(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "new.txt",
               "hunk": file_id("new.txt"), "action": "accept"}),
    )
    .await;
    // A file the session's turns never changed is not the review's to change.
    std::fs::write(dir.join("user.txt"), "the user's edit\n").unwrap();
    let untouched = call(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "user.txt",
               "hunk": "file:x", "action": "reject"}),
    )
    .await;
    assert!(
        untouched
            .unwrap_err()
            .contains("not changed by this session")
    );
    assert_eq!(read(dir.join("user.txt")), "the user's edit\n");
    std::fs::write(dir.join("user.txt"), "the user's\n").unwrap();
    let review: SessionReviewResult =
        ok(&mut connection, "session.review", json!({"session_id": id})).await;
    assert!(review.files.is_empty(), "everything reviewed");
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&dir, &["ls-files", "--stage"]),
        index,
        "the index is untouched"
    );

    // Committing asks first (no rule), changing nothing.
    let asked: SessionReviewCommitResult = ok(
        &mut connection,
        "session.review_commit",
        json!({"session_id": id, "repo": root, "message": "Session work"}),
    )
    .await;
    assert!(asked.needs_confirmation && !asked.committed);
    assert_eq!(asked.files, ["a.txt", "new.txt"]);
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);

    // Confirmed, it commits the files the session changed as they now are; a file the user
    // changed between turns is not among them.
    std::fs::write(dir.join("user.txt"), "changed by the user before\n").unwrap();
    let committed: SessionReviewCommitResult = ok(
        &mut connection,
        "session.review_commit",
        json!({"session_id": id, "repo": root, "message": "Session work", "confirmed": true}),
    )
    .await;
    assert!(committed.committed);
    let new_head = git(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(committed.commit.as_deref(), Some(new_head.as_str()));
    assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "Session work");
    assert_eq!(
        git(&dir, &["show", "--name-only", "--format=", "HEAD"]),
        "a.txt\nnew.txt"
    );
    assert_eq!(
        git(&dir, &["show", "HEAD:a.txt"]),
        BEFORE.replace("\n2\n", "\nTWO\n").trim()
    );
    assert!(
        git(&dir, &["status", "--porcelain"]).contains("user.txt"),
        "the user's change is left uncommitted"
    );

    // A pull request asks first too (or, without gh, only shows the commands).
    let pr: SessionReviewPullRequestResult = ok(
        &mut connection,
        "session.review_pull_request",
        json!({"session_id": id, "repo": root, "title": "Session work"}),
    )
    .await;
    assert!(!pr.opened);
    assert_eq!(pr.branch, "main");
    assert!(pr.command.contains("git push -u origin -- refs/heads/main"));
    assert!(
        pr.command
            .contains("gh pr create --head main --title 'Session work'")
    );
}

/// Sends `text` to a new session in `dir` and waits for its turn to end.
async fn turn(state: &Arc<DaemonState>, dir: &Path, text: &str) -> String {
    let (session, _) = state
        .create_session(&dir.display().to_string(), None)
        .unwrap();
    let mut events = session.subscribe();
    let mut announced = state.announcements();
    session.send(text.into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        if let SessionEvent::TurnCompleted { .. } = event.event
            && event.agent_id == ROOT_AGENT
        {
            break;
        }
    }
    loop {
        let notification = tokio::time::timeout(Duration::from_secs(20), announced.recv())
            .await
            .expect("a checkpoint within 20 s")
            .unwrap();
        if notification.method == "session.checkpoint" {
            break;
        }
    }
    session.id().to_string()
}

#[tokio::test]
async fn paths_are_literal_so_a_glob_named_file_commits_only_itself() {
    let repo = repository();
    let dir = repo.path().to_path_buf();
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url, Some("allow"));
    let mut connection = connect(&state).await;
    let id = turn(&state, &dir, "glob").await;
    // The user's edit to a.txt would match the pattern `[ab].txt`.
    std::fs::write(dir.join("a.txt"), "the user's\n").unwrap();
    let root =
        ok::<SessionReviewResult>(&mut connection, "session.review", json!({"session_id": id}))
            .await
            .repos[0]
            .clone();
    let committed: SessionReviewCommitResult = ok(
        &mut connection,
        "session.review_commit",
        json!({"session_id": id, "repo": root, "message": "Glob"}),
    )
    .await;
    assert!(committed.committed);
    assert_eq!(
        git(&dir, &["show", "--name-only", "--format=", "HEAD"]),
        "[ab].txt"
    );
    assert!(git(&dir, &["status", "--porcelain"]).contains("a.txt"));
}

#[tokio::test]
async fn a_repositorys_own_config_that_would_run_programs_is_refused() {
    let repo = repository();
    let dir = repo.path().to_path_buf();
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url, Some("allow"));
    let mut connection = connect(&state).await;
    let id = edited(&state, &dir).await;
    let root =
        ok::<SessionReviewResult>(&mut connection, "session.review", json!({"session_id": id}))
            .await
            .repos[0]
            .clone();
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let params = json!({"session_id": id, "repo": root, "message": "x"});
    // A filter driver in the repository's own config.
    git(&dir, &["config", "filter.evil.clean", "touch marker"]);
    let refused = call(&mut connection, "session.review_commit", params.clone()).await;
    assert!(refused.unwrap_err().contains("filter.evil.clean"));
    git(&dir, &["config", "--unset", "filter.evil.clean"]);
    // SSH signing whose key comes from a command the repository names.
    git(&dir, &["config", "gpg.format", "ssh"]);
    git(&dir, &["config", "commit.gpgSign", "true"]);
    git(
        &dir,
        &["config", "gpg.ssh.defaultKeyCommand", "touch marker"],
    );
    let refused = call(&mut connection, "session.review_commit", params).await;
    let message = refused.unwrap_err();
    assert!(message.contains("gpg.ssh.defaultkeycommand"), "{message}");
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);
    assert!(!dir.join("marker").exists());
}

/// A stand-in for `gh` that records that it ran.
fn fake_gh(dir: &Path) -> (PathBuf, PathBuf) {
    let log = dir.join("gh-ran");
    #[cfg(windows)]
    let gh = {
        let gh = dir.join("gh.cmd");
        std::fs::write(
            &gh,
            format!(
                "@echo off\r\necho ran> \"{}\"\r\necho https://example.com/pr/1\r\n",
                log.display()
            ),
        )
        .unwrap();
        gh
    };
    #[cfg(not(windows))]
    let gh = {
        use std::os::unix::fs::PermissionsExt;
        let gh = dir.join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\necho ran > '{}'\necho https://example.com/pr/1\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        gh
    };
    (gh, log)
}

#[tokio::test]
async fn pull_requests_follow_the_rules_and_refuse_repository_transports() {
    let tools = tempfile::tempdir().unwrap();
    let (gh, log) = fake_gh(tools.path());
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "-q", "--bare"]);

    // Denied: nothing runs.
    let repo = repository();
    let dir = repo.path().to_path_buf();
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            &origin.path().display().to_string(),
        ],
    );
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon_with(home.path(), &url, &[("open_pull_request", "deny")]);
    state.use_gh(gh.clone());
    let mut connection = connect(&state).await;
    let id = edited(&state, &dir).await;
    let root =
        ok::<SessionReviewResult>(&mut connection, "session.review", json!({"session_id": id}))
            .await
            .repos[0]
            .clone();
    let params = json!({"session_id": id, "repo": root, "title": "PR", "confirmed": true});
    let denied = call(&mut connection, "session.review_pull_request", params).await;
    assert!(denied.unwrap_err().contains("denies open_pull_request"));
    assert!(!log.exists());

    // Allowed: the repository's own transport settings are refused before any push.
    let repo = repository();
    let dir = repo.path().to_path_buf();
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            &origin.path().display().to_string(),
        ],
    );
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon_with(home.path(), &url, &[("open_pull_request", "allow")]);
    state.use_gh(gh.clone());
    let mut connection = connect(&state).await;
    let id = edited(&state, &dir).await;
    let root =
        ok::<SessionReviewResult>(&mut connection, "session.review", json!({"session_id": id}))
            .await
            .repos[0]
            .clone();
    let params = json!({"session_id": id, "repo": root, "title": "PR"});
    for (key, value) in [
        ("core.sshCommand", "touch marker"),
        ("protocol.ext.allow", "always"),
    ] {
        git(&dir, &["config", key, value]);
        let refused = call(
            &mut connection,
            "session.review_pull_request",
            params.clone(),
        )
        .await;
        let message = refused.unwrap_err();
        assert!(message.contains(&key.to_lowercase()), "{message}");
        git(&dir, &["config", "--unset", key]);
    }
    // Without them the push runs, with network transports only: a local `file` origin is
    // refused, so gh never runs either.
    let failed = call(&mut connection, "session.review_pull_request", params).await;
    let message = failed.unwrap_err();
    assert!(message.contains("file"), "{message}");
    assert!(!log.exists());
    assert!(!dir.join("marker").exists());
    assert_eq!(
        git(origin.path(), &["for-each-ref"]),
        "",
        "nothing was pushed"
    );
}

#[tokio::test]
async fn nothing_changes_while_a_turn_runs() {
    let repo = repository();
    let dir = repo.path().to_path_buf();
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url, Some("allow"));
    let mut connection = connect(&state).await;
    let id = edited(&state, &dir).await;
    let session = state.session(&id).unwrap();
    let mut events = session.subscribe();
    let root =
        ok::<SessionReviewResult>(&mut connection, "session.review", json!({"session_id": id}))
            .await
            .repos[0]
            .clone();
    session.send("slow".into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .unwrap()
            .unwrap();
        if let SessionEvent::TurnStarted { .. } = event.event {
            break;
        }
    }
    let commit = call(
        &mut connection,
        "session.review_commit",
        json!({"session_id": id, "repo": root, "message": "x"}),
    )
    .await;
    assert!(commit.unwrap_err().contains("a turn is running"));
    let hunk = call(
        &mut connection,
        "session.review_hunk",
        json!({"session_id": id, "since": 1, "repo": root, "path": "new.txt",
               "hunk": "file:x", "action": "reject"}),
    )
    .await;
    assert!(hunk.unwrap_err().contains("a turn is running"));
    assert!(dir.join("new.txt").exists());
}

#[tokio::test]
async fn permission_rules_deny_or_allow_commits() {
    let repo = repository();
    let dir = repo.path().to_path_buf();
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url, Some("deny"));
    let mut connection = connect(&state).await;
    let id = edited(&state, &dir).await;
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let root =
        ok::<SessionReviewResult>(&mut connection, "session.review", json!({"session_id": id}))
            .await
            .repos[0]
            .clone();
    let denied = call(
        &mut connection,
        "session.review_commit",
        json!({"session_id": id, "repo": root, "message": "x", "confirmed": true}),
    )
    .await;
    assert!(denied.unwrap_err().contains("denies git_commit"));
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head);

    let repo = repository();
    let dir = repo.path().to_path_buf();
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url, Some("allow"));
    let mut connection = connect(&state).await;
    let id = edited(&state, &dir).await;
    let root =
        ok::<SessionReviewResult>(&mut connection, "session.review", json!({"session_id": id}))
            .await
            .repos[0]
            .clone();
    let committed: SessionReviewCommitResult = ok(
        &mut connection,
        "session.review_commit",
        json!({"session_id": id, "repo": root, "message": "Allowed"}),
    )
    .await;
    assert!(committed.committed, "an allow rule commits without asking");
    assert_eq!(
        git(&dir, &["show", "--name-only", "--format=", "HEAD"]),
        "a.txt\nb.txt\nnew.txt"
    );
}
