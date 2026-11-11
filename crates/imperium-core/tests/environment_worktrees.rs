//! An environment's sessions get its project's git worktrees without extra setup: a session in
//! an environment whose folder is a git repository can list and read inside the repository's
//! linked worktrees (also one added after the session started), and turning the environment's
//! `project_worktrees` off takes them away again. Everything runs on temporary repositories.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// One tool call per request, in order, with the arguments exactly as given; then a reply.
async fn fake_model(calls: Vec<(&'static str, String)>) -> String {
    let calls = Arc::new(calls);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let calls = Arc::clone(&calls);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let done = messages.iter().filter(|m| m["role"] == "tool").count();
                let body = match calls.get(done) {
                    Some((name, arguments)) => sse(json!({"choices":[{"delta":{"tool_calls":[{
                        "index": 0, "id": format!("c{done}"),
                        "function": {"name": name, "arguments": arguments}
                    }]}}]})),
                    None => sse(json!({"choices":[{"delta":{"content":"Done."}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn daemon(home: &std::path::Path, base_url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false"
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

fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let Some(Response { outcome, .. }) = handled.response else {
        panic!("no immediate reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {error:?}"),
    }
}

fn path_args(path: &str) -> String {
    json!({ "path": path }).to_string()
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs one turn and returns each tool call's (ok, output).
async fn turn(state: &Arc<DaemonState>, session_id: &str) -> Vec<(bool, String)> {
    let session = state.session(session_id).unwrap();
    let mut events = session.subscribe();
    session.send("look at the worktrees".into()).unwrap();
    let mut results = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::ToolFinished { ok, output, .. } => results.push((ok, output)),
                SessionEvent::TurnCompleted { .. } => break,
                SessionEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");
    results
}

#[tokio::test]
async fn an_environments_sessions_get_its_project_worktrees_unless_turned_off() {
    let home = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("project");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("src").join("lib.rs"), "// in the project\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    // Worktrees outside the project folder, where only discovery can open them.
    let trees = base.path().join("trees");
    let feature = trees.join("feature");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            feature.to_str().unwrap(),
        ],
    );
    let later = trees.join("later");

    let feature_text = feature.display().to_string();
    let later_text = later.display().to_string();
    let calls: Vec<(&'static str, String)> = vec![
        ("list_dir", path_args(&feature_text)),
        (
            "read_file",
            path_args(&format!("{}/src/lib.rs", feature_text.replace('\\', "/"))),
        ),
        // Added after the session started, as an agent would with `git worktree add`.
        (
            "read_file",
            path_args(&format!("{}/src/lib.rs", later_text.replace('\\', "/"))),
        ),
    ];
    let url = fake_model(calls).await;

    let state = daemon(home.path(), &url);
    let mut connection = Connection::new(Arc::clone(&state));
    call(
        &mut connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    let env = call(
        &mut connection,
        2,
        "environments.create",
        json!({"name": "Project", "folders": [repo]}),
    );
    assert_eq!(env["project_worktrees"], true, "on by default");
    let created = call(
        &mut connection,
        3,
        "session.create",
        json!({"environment_id": env["id"]}),
    );
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "later",
            later.to_str().unwrap(),
        ],
    );
    let results = turn(&state, created["session_id"].as_str().unwrap()).await;
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(results[0].0, "list: {}", results[0].1);
    assert!(results[0].1.contains("src/"), "{}", results[0].1);
    for (ok, output) in &results[1..] {
        assert!(*ok, "read: {output}");
        assert!(output.contains("in the project"), "{output}");
    }

    // Turned off, a new session cannot reach them.
    let updated = call(
        &mut connection,
        4,
        "environments.update",
        json!({"id": env["id"], "project_worktrees": false}),
    );
    assert_eq!(updated["project_worktrees"], false);
    let created = call(
        &mut connection,
        5,
        "session.create",
        json!({"environment_id": env["id"]}),
    );
    let results = turn(&state, created["session_id"].as_str().unwrap()).await;
    assert_eq!(results.len(), 3, "{results:?}");
    for (ok, output) in &results {
        assert!(!*ok, "should be refused: {output}");
        assert!(!output.contains("in the project"), "{output}");
    }
}
