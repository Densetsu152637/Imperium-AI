//! An environment holds zero or more persistent agents. A new one has none; agents of one
//! environment run independently, each with its own name, role and conversation; an environment
//! saved with the old single persistent session gets it back as an agent, history and all; and
//! the deprecated `persistent_agent` field still works for one protocol minor.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<Value>>>;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Answers "Echo: <last user message>" and records every request.
async fn fake_model(log: Log) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let log = Arc::clone(&log);
            async move {
                let said = body["messages"]
                    .as_array()
                    .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "user"))
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or_default()
                    .to_string();
                log.lock().unwrap().push(body);
                (
                    [("content-type", "text/event-stream")],
                    sse(json!({"choices":[{"delta":{"content":format!("Echo: {said}")}}]})),
                )
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

fn try_call(connection: &mut Connection, method: &str, params: Value) -> Result<Value, String> {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match handled.response {
        Some(Response {
            outcome: Outcome::Result(value),
            ..
        }) => Ok(value),
        Some(Response {
            outcome: Outcome::Error(error),
            ..
        }) => Err(error.message),
        None => panic!("no immediate reply to {method}"),
    }
}

fn call(connection: &mut Connection, method: &str, params: Value) -> Value {
    try_call(connection, method, params).unwrap_or_else(|error| panic!("{method}: {error}"))
}

fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    call(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    connection
}

/// Sends a message to a session and returns its reply.
async fn say(state: &Arc<DaemonState>, session_id: &str, text: &str) -> String {
    let session = state.session(session_id).unwrap();
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::TextDelta { text } => reply.push_str(&text),
                SessionEvent::TurnCompleted { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");
    reply
}

fn new_environment(connection: &mut Connection, folder: &std::path::Path) -> String {
    let env = call(
        connection,
        "environments.create",
        json!({"name": "Work", "folders": [folder]}),
    );
    env["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_new_environment_has_no_agents_and_nothing_starts() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &fake_model(Arc::default()).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());

    let listed = call(&mut connection, "environments.list", json!({}));
    let env = &listed["environments"][0];
    assert_eq!(env["agents"], json!([]));
    assert_eq!(env["persistent_agent"], false);
    assert!(env["agent_session_id"].is_null());
    let agents = call(
        &mut connection,
        "environments.agent_list",
        json!({"environment_id": id}),
    );
    assert_eq!(agents["agents"], json!([]));
    state.start_project_agents();
    assert!(state.project_agents().environment_agents().is_empty());
    assert!(state.sessions().is_empty());
}

#[tokio::test]
async fn two_agents_in_one_environment_run_independently() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let log: Log = Arc::default();
    let state = daemon(home.path(), &fake_model(Arc::clone(&log)).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());

    let planner = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Planner", "role": "Keep the roadmap current."}),
    );
    let reviewer = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Reviewer", "role": "Critique open designs."}),
    );
    assert_ne!(planner["session_id"], reviewer["session_id"]);
    assert_eq!(state.project_agents().agents_of(&id).len(), 2);
    // The same name twice is refused, ignoring case.
    let duplicate = try_call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "planner"}),
    );
    assert!(duplicate.unwrap_err().contains("already has an agent"));

    let planner_session = planner["session_id"].as_str().unwrap().to_string();
    let reviewer_session = reviewer["session_id"].as_str().unwrap().to_string();
    assert_eq!(
        say(&state, &planner_session, "plan the week").await,
        "Echo: plan the week"
    );
    assert_eq!(
        say(&state, &reviewer_session, "review the memory design").await,
        "Echo: review the memory design"
    );

    // Each has its own role in its own instructions, and its conversation holds only what it
    // was told.
    {
        let log = log.lock().unwrap();
        let system_of = |needle: &str| -> String {
            log.iter()
                .filter_map(|body| {
                    let messages = body["messages"].as_array()?;
                    let said = messages
                        .iter()
                        .any(|m| m["content"].as_str().is_some_and(|text| text == needle));
                    said.then(|| {
                        messages[0]["content"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                })
                .next()
                .unwrap()
        };
        let planner_system = system_of("plan the week");
        assert!(
            planner_system.contains("Your name is Planner."),
            "{planner_system}"
        );
        assert!(planner_system.contains("Keep the roadmap current."));
        assert!(!planner_system.contains("Critique open designs."));
        let reviewer_system = system_of("review the memory design");
        assert!(reviewer_system.contains("Your name is Reviewer."));
        assert!(reviewer_system.contains("Critique open designs."));
        let reviewer_saw_planner = log.iter().any(|body| {
            body["messages"].as_array().is_some_and(|messages| {
                messages
                    .iter()
                    .any(|m| m["content"].as_str() == Some("plan the week"))
                    && messages
                        .iter()
                        .any(|m| m["content"].as_str() == Some("review the memory design"))
            })
        });
        assert!(!reviewer_saw_planner, "the agents share no conversation");
    }

    // Deleting one stops and archives it; the other keeps running.
    call(
        &mut connection,
        "environments.agent_delete",
        json!({"environment_id": id, "agent_id": planner["id"]}),
    );
    assert!(state.session(&planner_session).is_none());
    assert!(
        state
            .archived_sessions()
            .iter()
            .any(|s| s.session_id == planner_session)
    );
    assert!(state.session(&reviewer_session).is_some());
    assert_eq!(
        say(&state, &reviewer_session, "still here?").await,
        "Echo: still here?"
    );

    // Renaming restarts the agent with its conversation and shows the new name.
    let renamed = call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": reviewer["id"], "name": "Critic"}),
    );
    assert_eq!(renamed["name"], "Critic");
    assert_eq!(renamed["session_id"], reviewer_session.as_str());
    let session = state.session(&reviewer_session).unwrap();
    assert_eq!(state.summary(&session).title, "Critic");
}

#[tokio::test]
async fn an_old_persistent_session_becomes_an_agent_with_its_history() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let url = fake_model(Arc::default()).await;
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());
    // The deprecated field still turns an agent on.
    let env = call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": true}),
    );
    assert_eq!(env["agents"].as_array().unwrap().len(), 1);
    assert_eq!(env["agents"][0]["name"], "Persistent agent");
    assert_eq!(env["persistent_agent"], true);
    let session_id = env["agent_session_id"].as_str().unwrap().to_string();
    assert_eq!(env["agents"][0]["session_id"], session_id.as_str());
    say(&state, &session_id, "remember this").await;
    drop((connection, state));

    // Rewrite the saved environments as an older daemon wrote them: one persistent session,
    // no agents.
    let file = home.path().join("data").join("environments.json");
    let file = if file.exists() {
        file
    } else {
        find_file(home.path(), "environments.json")
    };
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for env in saved["environments"].as_array_mut().unwrap() {
        let object = env.as_object_mut().unwrap();
        object.remove("agents");
        object.insert("persistent_agent".into(), json!(true));
        object.insert("agent_session_id".into(), json!(session_id));
    }
    std::fs::write(&file, saved.to_string()).unwrap();

    let state = daemon(home.path(), &url);
    let mut connection = connect(&state);
    let listed = call(&mut connection, "environments.list", json!({}));
    let env = &listed["environments"][0];
    assert_eq!(env["agents"].as_array().unwrap().len(), 1, "{env}");
    assert_eq!(env["agents"][0]["session_id"], session_id.as_str());
    state.start_project_agents();
    let session = state.session(&session_id).unwrap();
    assert_eq!(session.title(), "remember this", "its history came back");
    assert!(state.summary(&session).persistent);
    // The migration was saved: another start finds the agent without migrating again.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        saved["environments"][0]["agents"].as_array().unwrap().len(),
        1
    );

    // Turning the deprecated field off deletes the agents.
    call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": false}),
    );
    assert!(state.project_agents().environment_agents().is_empty());
    let env = call(&mut connection, "environments.list", json!({}));
    assert_eq!(env["environments"][0]["agents"], json!([]));
    assert_eq!(env["environments"][0]["persistent_agent"], false);
}

fn find_file(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                return path;
            }
        }
    }
    panic!("no {name} under {}", root.display());
}
