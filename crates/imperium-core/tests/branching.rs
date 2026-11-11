//! Branching and comparison (roadmap item 7): `session.branch` copies a session's journal and
//! conversation up to a message into a new session that carries on independently;
//! `session.compare` sends one message to branches on several models, tagged with a shared
//! comparison id; `session.compare_pick` keeps one and archives the rest.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::methods::{
    SessionBranchResult, SessionComparePickResult, SessionCompareResult,
};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use serde_json::{Value, json};

/// A model that answers `seen:<user messages in the conversation>:<model>`.
async fn serve() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let users = body["messages"].as_array().map_or(0, |messages| {
                messages.iter().filter(|m| m["role"] == "user").count()
            });
            let model = body["model"].as_str().unwrap_or_default();
            let chunk =
                json!({"choices": [{"delta": {"content": format!("seen:{users}:{model}")}}]});
            (
                [("content-type", "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
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
         [workflow]\nenabled = false\n[memory]\nenabled = false\n"
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
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 13}}),
    )
    .await
    .unwrap();
    connection
}

/// The session's events once it has finished `turns` root turns.
async fn after_turns(state: &Arc<DaemonState>, id: &str, turns: usize) -> Vec<AgentEvent> {
    for _ in 0..600 {
        if let Some(session) = state.session(id) {
            let (history, _) = session.subscribe_with_history();
            let done = history
                .iter()
                .filter(|event| {
                    event.agent_id == ROOT_AGENT
                        && matches!(
                            event.event,
                            SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
                        )
                })
                .count();
            if done >= turns {
                return history;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("session {id} did not finish {turns} turns");
}

/// What the root said, turn by turn.
fn replies(history: &[AgentEvent]) -> Vec<String> {
    let mut said = Vec::new();
    for event in history.iter().filter(|event| event.agent_id == ROOT_AGENT) {
        match &event.event {
            SessionEvent::TurnStarted { .. } => said.push(String::new()),
            SessionEvent::TextDelta { text } => {
                if let Some(last) = said.last_mut() {
                    last.push_str(text);
                }
            }
            _ => {}
        }
    }
    said
}

fn prompts(history: &[AgentEvent]) -> Vec<String> {
    history
        .iter()
        .filter_map(|event| match &event.event {
            SessionEvent::TurnStarted { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn index_of_turn(history: &[AgentEvent], text: &str) -> u32 {
    history
        .iter()
        .position(|event| {
            matches!(&event.event, SessionEvent::TurnStarted { text: said, .. } if said == text)
        })
        .unwrap()
        .try_into()
        .unwrap()
}

#[tokio::test]
async fn a_branch_has_the_messages_up_to_its_point_and_continues_on_its_own() {
    let url = serve().await;
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state).await;
    let (session, _) = state
        .create_session(&folder.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    for (turn, text) in ["one", "two", "three"].into_iter().enumerate() {
        session.send(text.into()).unwrap();
        after_turns(&state, &id, turn + 1).await;
    }
    let history = after_turns(&state, &id, 3).await;
    assert_eq!(replies(&history), ["seen:1:m", "seen:2:m", "seen:3:m"]);

    // Branch at the message "three": the turn is left out and its text comes back.
    let at = index_of_turn(&history, "three");
    let branched: SessionBranchResult = serde_json::from_value(
        call(
            &mut connection,
            "session.branch",
            json!({"session_id": id, "at": at}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(branched.prompt.as_deref(), Some("three"));
    assert!(!branched.worktree);
    let branch = state
        .session(&branched.session_id)
        .expect("the branch runs");
    let (copied, _) = branch.subscribe_with_history();
    assert_eq!(
        prompts(&copied),
        ["one", "two"],
        "exactly the messages before it"
    );
    assert_eq!(copied.len(), at as usize);
    let summary = state.summary(&branch);
    let origin = summary
        .branched_from
        .expect("it records where it came from");
    assert_eq!((origin.session_id.as_str(), origin.at), (id.as_str(), at));
    assert!(summary.title.ends_with("(branch)"));

    // Both go on from the same point without affecting each other.
    branch.send("four".into()).unwrap();
    session.send("five".into()).unwrap();
    let branch_history = after_turns(&state, &branched.session_id, 3).await;
    let source_history = after_turns(&state, &id, 4).await;
    assert_eq!(prompts(&branch_history), ["one", "two", "four"]);
    assert_eq!(
        replies(&branch_history),
        ["seen:1:m", "seen:2:m", "seen:3:m"]
    );
    assert_eq!(prompts(&source_history), ["one", "two", "three", "five"]);
    assert_eq!(
        replies(&source_history),
        ["seen:1:m", "seen:2:m", "seen:3:m", "seen:4:m"]
    );

    // On a reply, the whole turn is kept. The branch itself can be branched again.
    let on_reply = index_of_turn(&source_history, "two") + 1;
    let again: SessionBranchResult = serde_json::from_value(
        call(
            &mut connection,
            "session.branch",
            json!({"session_id": branched.session_id, "at": on_reply, "title": "second look"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(again.prompt, None);
    let (second, _) = state
        .session(&again.session_id)
        .expect("the second branch runs")
        .subscribe_with_history();
    assert_eq!(prompts(&second), ["one", "two"]);
    assert_eq!(replies(&second), ["seen:1:m", "seen:2:m"]);

    // Unknown events are refused.
    let refused = call(
        &mut connection,
        "session.branch",
        json!({"session_id": id, "at": 99999}),
    )
    .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_comparison_runs_each_model_on_its_own_branch_and_a_pick_archives_the_rest() {
    let url = serve().await;
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state).await;
    let (session, _) = state
        .create_session(&folder.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    session.send("setup".into()).unwrap();
    after_turns(&state, &id, 1).await;

    // One model, or an empty message, is not a comparison.
    assert!(
        call(
            &mut connection,
            "session.compare",
            json!({"session_id": id, "message": "go", "models": ["fake:m"]}),
        )
        .await
        .is_err()
    );

    let compared: SessionCompareResult = serde_json::from_value(
        call(
            &mut connection,
            "session.compare",
            json!({"session_id": id, "message": "compare me", "models": ["fake:m", "fake:n"]}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(compared.branches.len(), 2);
    let mut said = Vec::new();
    for branch in &compared.branches {
        let history = after_turns(&state, &branch.session_id, 2).await;
        assert_eq!(prompts(&history), ["setup", "compare me"]);
        said.push(replies(&history)[1].clone());
        let summary = state.summary(&state.session(&branch.session_id).unwrap());
        assert_eq!(
            summary.comparison_id.as_deref(),
            Some(compared.comparison_id.as_str())
        );
        assert_eq!(summary.branched_from.unwrap().session_id, id);
    }
    said.sort();
    assert_eq!(
        said,
        ["seen:2:m", "seen:2:n"],
        "each branch ran on its model"
    );
    // The source was not touched.
    assert_eq!(
        prompts(&after_turns(&state, &id, 1).await),
        ["setup"],
        "the source did not get the message"
    );

    let keep = compared.branches[0].session_id.clone();
    let other = compared.branches[1].session_id.clone();
    let picked: SessionComparePickResult = serde_json::from_value(
        call(
            &mut connection,
            "session.compare_pick",
            json!({"comparison_id": compared.comparison_id, "session_id": keep}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(picked.kept, keep);
    assert_eq!(picked.archived, [other.clone()]);
    assert!(state.session(&keep).is_some());
    assert!(state.session(&other).is_none());
    let archived = state.archived_sessions();
    let archived = archived
        .iter()
        .find(|summary| summary.session_id == other)
        .expect("the other branch is archived");
    assert_eq!(
        archived.comparison_id.as_deref(),
        Some(compared.comparison_id.as_str())
    );

    // A session outside the comparison cannot be picked.
    assert!(
        call(
            &mut connection,
            "session.compare_pick",
            json!({"comparison_id": compared.comparison_id, "session_id": id}),
        )
        .await
        .is_err()
    );
}

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

#[tokio::test]
async fn a_branch_with_a_worktree_gets_the_files_of_its_point_and_leaves_the_source_alone() {
    let url = serve().await;
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "committed\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state).await;
    let (session, _) = state
        .create_session(&dir.display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    session.send("hello".into()).unwrap();
    let history = after_turns(&state, &id, 1).await;
    // Work the session's files hold at this point, committed or not.
    std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
    std::fs::write(dir.join("new.txt"), "untracked\n").unwrap();
    let before = git(dir, &["status", "--porcelain"]);

    let at = u32::try_from(history.len() - 1).unwrap();
    let branched: SessionBranchResult = serde_json::from_value(
        call(
            &mut connection,
            "session.branch",
            json!({"session_id": id, "at": at, "worktree": true}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(branched.worktree);
    let branch = state.summary(&state.session(&branched.session_id).unwrap());
    assert_eq!(branch.worktrees.len(), 1);
    let tree = Path::new(&branch.worktrees[0].path);
    assert_ne!(tree, dir, "the branch works in a worktree of its own");
    assert_eq!(
        std::fs::read_to_string(tree.join("a.txt")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("new.txt")).unwrap(),
        "untracked\n"
    );
    assert_eq!(
        git(dir, &["status", "--porcelain"]),
        before,
        "the source's files"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "edited\n"
    );
}
