//! A session started in an environment can use every folder of it and nothing else: its agent
//! reads a file in the environment's second folder and is refused one outside all folders. The
//! session records its environment, and survives a restart in it.

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

fn read(id: &str, path: &str) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,
        "function":{"name":"read_file","arguments":json!({"path": path}).to_string()}}]}}]}),
    )
}

/// Reads `inside`, then `outside`, then reports.
async fn fake_model(inside: String, outside: String) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let (inside, outside) = (inside.clone(), outside.clone());
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let tool_results = messages.iter().filter(|m| m["role"] == "tool").count();
                let body = match tool_results {
                    0 => read("a", &inside),
                    1 => read("b", &outside),
                    _ => sse(json!({"choices":[{"delta":{"content":"Done."}}]})),
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

#[tokio::test]
async fn a_session_uses_every_folder_of_its_environment_and_nothing_else() {
    let home = tempfile::tempdir().unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        second.path().join("notes.txt"),
        "kept in the second folder\n",
    )
    .unwrap();
    std::fs::write(outside.path().join("secret.txt"), "not for the agent\n").unwrap();
    let url = fake_model(
        second.path().join("notes.txt").display().to_string(),
        outside.path().join("secret.txt").display().to_string(),
    )
    .await;

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
        json!({"name": "Both", "folders": [first.path(), second.path()]}),
    );
    let env_id = env["id"].as_str().unwrap().to_string();
    let created = call(
        &mut connection,
        3,
        "session.create",
        json!({"environment_id": env_id}),
    );
    assert_eq!(created["environment_id"], env_id.as_str());
    assert_eq!(created["folders"].as_array().unwrap().len(), 2);
    let session_id = created["session_id"].as_str().unwrap().to_string();
    let listed = call(&mut connection, 4, "session.list", json!({}));
    assert_eq!(listed["sessions"][0]["environment_id"], env_id.as_str());

    let session = state.session(&session_id).unwrap();
    let mut events = session.subscribe();
    session.send("read both".into()).unwrap();
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
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(
        results[0].0 && results[0].1.contains("kept in the second folder"),
        "{results:?}"
    );
    assert!(
        !results[1].0 && results[1].1.contains("outside the workspace"),
        "{results:?}"
    );

    // After a restart the session continues in its environment.
    drop(events);
    drop(session);
    drop(connection);
    drop(state);
    let state = daemon(home.path(), &url);
    assert_eq!(state.resume_sessions(), 1);
    let mut connection = Connection::new(Arc::clone(&state));
    call(
        &mut connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    let listed = call(&mut connection, 2, "session.list", json!({}));
    assert_eq!(listed["sessions"][0]["environment_id"], env_id.as_str());
    assert_eq!(
        listed["sessions"][0]["folders"].as_array().unwrap().len(),
        2
    );
}

#[tokio::test]
async fn a_bare_project_folder_becomes_an_environment() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1");
    let mut connection = Connection::new(Arc::clone(&state));
    call(
        &mut connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    let first = call(
        &mut connection,
        2,
        "session.create",
        json!({"project_dir": project.path()}),
    );
    let second = call(
        &mut connection,
        3,
        "session.create",
        json!({"project_dir": project.path()}),
    );
    assert!(first["environment_id"].is_string());
    assert_eq!(first["environment_id"], second["environment_id"]);
    let envs = call(&mut connection, 4, "environments.list", json!({}));
    assert_eq!(envs["environments"].as_array().unwrap().len(), 1);
}
