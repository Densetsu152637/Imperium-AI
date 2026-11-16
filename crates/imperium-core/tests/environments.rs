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
    assert_eq!(
        envs["environments"].as_array().unwrap().len(),
        2,
        "Home and the project's environment"
    );
}

#[tokio::test]
async fn pins_on_environments_and_sessions_survive_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let handshake =
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}});
    let state = daemon(home.path(), "http://127.0.0.1:9/v1");
    let mut connection = Connection::new(Arc::clone(&state));
    call(&mut connection, 1, "system.handshake", handshake.clone());
    let env = call(
        &mut connection,
        2,
        "environments.create",
        json!({"name": "Pinned", "folders": [folder.path()]}),
    );
    assert_eq!(env["pinned"], false);
    let env_id = env["id"].as_str().unwrap().to_string();
    let pinned = call(
        &mut connection,
        3,
        "environments.update",
        json!({"id": env_id, "pinned": true}),
    );
    assert_eq!(pinned["pinned"], true);
    // Updating something else leaves the pin alone.
    let renamed = call(
        &mut connection,
        4,
        "environments.update",
        json!({"id": env_id, "name": "Still pinned"}),
    );
    assert_eq!(renamed["pinned"], true);

    let created = call(
        &mut connection,
        5,
        "session.create",
        json!({"environment_id": env_id}),
    );
    let session_id = created["session_id"].as_str().unwrap().to_string();
    let listed = call(&mut connection, 6, "session.list", json!({}));
    assert_eq!(listed["sessions"][0]["pinned"], false);
    let mut announcements = state.announcements();
    let control = |pinned: Option<bool>| {
        let mut params = json!({"session_id": session_id, "action": "set_pinned"});
        if let Some(pinned) = pinned {
            params["pinned"] = json!(pinned);
        }
        json!({"jsonrpc": "2.0", "id": 7, "method": "session.control", "params": params})
            .to_string()
    };
    let handled = connection.handle_text(&control(Some(true)));
    let response = handled.deferred.expect("a deferred reply").await;
    assert!(
        matches!(response.outcome, Outcome::Result(_)),
        "{response:?}"
    );
    let updated = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let notice = announcements.recv().await.unwrap();
            if notice.method == "session.updated" {
                break notice.params;
            }
        }
    })
    .await
    .expect("session.updated is announced");
    assert_eq!(updated["pinned"], true);
    let listed = call(&mut connection, 8, "session.list", json!({}));
    assert_eq!(listed["sessions"][0]["pinned"], true);
    // set_pinned needs `pinned`.
    let handled = connection.handle_text(&control(None));
    let response = handled.deferred.expect("a deferred reply").await;
    assert!(
        matches!(response.outcome, Outcome::Error(_)),
        "{response:?}"
    );

    // After a restart both are still pinned.
    drop(announcements);
    drop(connection);
    drop(state);
    let state = daemon(home.path(), "http://127.0.0.1:9/v1");
    assert_eq!(state.resume_sessions(), 1);
    let mut connection = Connection::new(Arc::clone(&state));
    call(&mut connection, 1, "system.handshake", handshake);
    let envs = call(&mut connection, 2, "environments.list", json!({}));
    assert_eq!(envs["environments"][1]["pinned"], true);
    let listed = call(&mut connection, 3, "session.list", json!({}));
    assert_eq!(listed["sessions"][0]["session_id"], session_id.as_str());
    assert_eq!(listed["sessions"][0]["pinned"], true);
}

#[tokio::test]
async fn environment_grants_open_paths_and_rules_deny_calls() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(cache.path().join("index.txt"), "from the cache\n").unwrap();
    std::fs::write(project.path().join("secret.txt"), "not for the agent\n").unwrap();
    let url = fake_model(
        cache.path().join("index.txt").display().to_string(),
        "secret.txt".into(),
    )
    .await;

    let state = daemon(home.path(), &url);
    let mut connection = Connection::new(Arc::clone(&state));
    call(
        &mut connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 5}}),
    );
    let env = call(
        &mut connection,
        2,
        "environments.create",
        json!({
            "name": "Granted",
            "folders": [project.path()],
            "permissions": {
                "grants": [{"path": cache.path(), "mode": "read"}],
                "rules": [{"tool": "read_file", "pattern": "secret.txt", "action": "deny"}]
            }
        }),
    );
    assert_eq!(env["permissions"]["grants"][0]["mode"], "read");
    let env_id = env["id"].as_str().unwrap().to_string();
    let created = call(
        &mut connection,
        3,
        "session.create",
        json!({"environment_id": env_id}),
    );
    let session = state
        .session(created["session_id"].as_str().unwrap())
        .unwrap();
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
    assert!(
        results[0].0 && results[0].1.contains("from the cache"),
        "{results:?}"
    );
    assert!(
        !results[1].0 && results[1].1.contains("permission rule denies"),
        "{results:?}"
    );

    // Global rules are saved in the user config; "always allow" adds to the environment.
    let saved = call(
        &mut connection,
        4,
        "permissions.set",
        json!({"rules": [{"tool": "run_command", "pattern": "cargo test", "action": "allow"}]}),
    );
    assert_eq!(saved["global"]["rules"][0]["pattern"], "cargo test");
    assert!(!saved["presets"].as_array().unwrap().is_empty());
    let added = call(
        &mut connection,
        5,
        "permissions.add_rule",
        json!({
            "session_id": created["session_id"],
            "rule": {"tool": "write_file", "pattern": "src/**", "action": "allow"}
        }),
    );
    assert_eq!(added["environment_id"], env_id.as_str());
    let listed = call(&mut connection, 6, "environments.list", json!({}));
    assert_eq!(
        listed["environments"][1]["permissions"]["rules"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let global = call(&mut connection, 7, "permissions.get", json!({}));
    assert_eq!(global["global"]["rules"].as_array().unwrap().len(), 1);
}
