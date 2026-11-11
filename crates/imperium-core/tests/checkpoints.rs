//! Checkpoints and rewinding (roadmap item 6): a root turn that changes files in a git
//! repository leaves a checkpoint commit on a private ref without touching the repository's
//! index, HEAD, or branches; `session.rewind` puts the files back (changed, deleted, and
//! untracked ones; ignored ones are left alone), can be undone, and forks the conversation; a
//! turn that changes nothing leaves no checkpoint; deleting the session removes its refs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::methods::{SessionCheckpointsResult, SessionRewindResult};
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

/// A repository with two committed files, an ignored file, and an untracked one.
fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.join("b.txt"), "keep\n").unwrap();
    std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
    std::fs::write(dir.join("debug.log"), "ignored\n").unwrap();
    std::fs::create_dir_all(dir.join("notes")).unwrap();
    std::fs::write(dir.join("notes/u.txt"), "untracked\n").unwrap();
    repo
}

/// Root: `edit` deletes `b.txt` and `notes/u.txt` (as a command would) and writes `a.txt` and
/// `new.txt`; a tool result ends the turn; anything else is answered `ok`.
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
                    let _ = std::fs::remove_file(repo.join("notes/u.txt"));
                    let call = |index: u32, path: &str, content: &str| {
                        json!({"index": index, "id": format!("w{index}"), "function": {
                            "name": "write_file",
                            "arguments": json!({"path": path, "content": content}).to_string()}})
                    };
                    json!({"tool_calls": [call(0, "a.txt", "two\n"), call(1, "new.txt", "new\n")]})
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

fn daemon(home: &Path, url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{url}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n\
         [permissions]\nrules = [{{ tool = \"write_file\", action = \"allow\" }}]\n"
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

async fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    call(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 11}}),
    )
    .await
    .unwrap();
    connection
}

async fn turn(events: &mut broadcast::Receiver<AgentEvent>) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        match event.event {
            SessionEvent::TurnCompleted { .. } if event.agent_id == ROOT_AGENT => return,
            SessionEvent::TurnFailed { message } => panic!("the turn failed: {message}"),
            _ => {}
        }
    }
}

fn read(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// What git itself keeps for the repository: HEAD, branches, and the index.
fn repository_state(dir: &Path) -> (String, String, String) {
    (
        git(dir, &["rev-parse", "HEAD"]),
        git(dir, &["for-each-ref", "refs/heads"]),
        git(dir, &["ls-files", "--stage"]),
    )
}

#[tokio::test]
async fn turns_are_checkpointed_and_rewound_without_touching_the_repository() {
    let repo = repository();
    let dir = repo.path().to_path_buf();
    let url = serve(dir.clone()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state).await;
    let before = repository_state(&dir);
    let index = std::fs::read(dir.join(".git/index")).unwrap();

    let (session, _) = state
        .create_session(&dir.display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut events = session.subscribe();

    // A turn that changes nothing leaves no checkpoint.
    session.send("hello".into()).unwrap();
    turn(&mut events).await;
    let listed: SessionCheckpointsResult = serde_json::from_value(
        call(
            &mut connection,
            "session.checkpoints",
            json!({"session_id": id}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(listed.available);
    assert!(listed.checkpoints.is_empty());

    // A turn that changes files leaves one, of the files before it, announced once kept.
    let mut announced = state.announcements();
    session.send("edit".into()).unwrap();
    turn(&mut events).await;
    loop {
        let notification = tokio::time::timeout(Duration::from_secs(20), announced.recv())
            .await
            .expect("a checkpoint within 20 s")
            .unwrap();
        if notification.method == "session.checkpoint" {
            assert_eq!(notification.params["session_id"], id.as_str());
            break;
        }
    }
    assert_eq!(read(dir.join("a.txt")), "two\n");
    assert!(!dir.join("b.txt").exists() && !dir.join("notes/u.txt").exists());
    let listed: SessionCheckpointsResult = serde_json::from_value(
        call(
            &mut connection,
            "session.checkpoints",
            json!({"session_id": id}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(listed.checkpoints.len(), 1);
    let checkpoint = &listed.checkpoints[0];
    assert_eq!((checkpoint.number, checkpoint.kind.as_str()), (1, "turn"));
    assert_eq!(checkpoint.prompt, "edit");
    assert!(checkpoint.conversation);
    let reference = format!("refs/imperium/checkpoints/{id}/1");
    assert_eq!(checkpoint.repos[0].reference, reference);
    assert_eq!(
        git(&dir, &["rev-parse", &reference]),
        checkpoint.repos[0].commit
    );
    assert_eq!(
        git(&dir, &["rev-parse", &format!("{reference}^")]),
        before.0,
        "its parent is HEAD"
    );
    let tree = git(&dir, &["ls-tree", "-r", "--name-only", &reference]);
    let tree: Vec<&str> = tree.lines().collect();
    assert_eq!(tree, [".gitignore", "a.txt", "b.txt", "notes/u.txt"]);
    assert_eq!(
        git(&dir, &["show", &format!("{reference}:a.txt")]),
        "one",
        "the files as they were before the turn"
    );
    assert_eq!(repository_state(&dir), before, "HEAD, branches, index");
    assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index);

    // An ignored file changed since is left alone by a rewind.
    std::fs::write(dir.join("debug.log"), "changed\n").unwrap();

    let rewound: SessionRewindResult = serde_json::from_value(
        call(
            &mut connection,
            "session.rewind",
            json!({"session_id": id, "checkpoint": 1, "mode": "both"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(read(dir.join("a.txt")), "one\n");
    assert_eq!(read(dir.join("b.txt")), "keep\n", "a deleted tracked file");
    assert_eq!(
        read(dir.join("notes/u.txt")),
        "untracked\n",
        "a deleted untracked file"
    );
    assert!(
        !dir.join("new.txt").exists(),
        "a file added since is removed"
    );
    assert_eq!(read(dir.join("debug.log")), "changed\n");
    assert_eq!(rewound.restored.len(), 3);
    assert_eq!(rewound.removed.len(), 1);
    assert_eq!(rewound.undo_checkpoint, Some(2));
    assert_eq!(rewound.prompt.as_deref(), Some("edit"));
    assert_eq!(repository_state(&dir), before, "HEAD, branches, index");
    assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index);

    // The conversation was forked just before the turn's message.
    let forked = rewound.session_id.expect("a forked session");
    assert_ne!(forked, id);
    let forked = state.session(&forked).expect("the fork runs");
    let (history, _) = forked.subscribe_with_history();
    let said: Vec<String> = history
        .iter()
        .filter_map(|event| match &event.event {
            SessionEvent::TurnStarted { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(said, ["hello"]);
    assert!(state.session(&id).is_some(), "the original is kept");

    // The rewind is undone by rewinding the files to the checkpoint it made.
    let undone: SessionRewindResult = serde_json::from_value(
        call(
            &mut connection,
            "session.rewind",
            json!({"session_id": id, "checkpoint": 2, "mode": "files"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(read(dir.join("a.txt")), "two\n");
    assert_eq!(read(dir.join("new.txt")), "new\n");
    assert!(!dir.join("b.txt").exists());
    assert!(undone.session_id.is_none());
    // A rewind checkpoint has no conversation to go back to.
    let refused = call(
        &mut connection,
        "session.rewind",
        json!({"session_id": id, "checkpoint": 2, "mode": "conversation"}),
    )
    .await;
    assert!(refused.unwrap_err().contains("no conversation"));

    // Deleting the session removes its refs.
    call(&mut connection, "session.delete", json!({"session_id": id}))
        .await
        .unwrap();
    assert_eq!(
        git(
            &dir,
            &["for-each-ref", &format!("refs/imperium/checkpoints/{id}/")]
        ),
        ""
    );
    assert_eq!(repository_state(&dir), before);
}

#[tokio::test]
async fn folders_outside_git_have_no_checkpoints() {
    let plain = tempfile::tempdir().unwrap();
    let url = serve(plain.path().to_path_buf()).await;
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state).await;
    let (session, _) = state
        .create_session(&plain.path().display().to_string(), None)
        .unwrap();
    let mut events = session.subscribe();
    session.send("edit".into()).unwrap();
    turn(&mut events).await;
    let listed: SessionCheckpointsResult = serde_json::from_value(
        call(
            &mut connection,
            "session.checkpoints",
            json!({"session_id": session.id()}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(!listed.available);
    assert!(listed.reason.unwrap().contains("git"));
    assert!(listed.checkpoints.is_empty());
}
