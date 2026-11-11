//! Permission modes: sessions start in `permissions.mode`, `session.control`
//! `set_permission_mode` changes it (refusing unknown modes), lists show it, and a session
//! resumed after a restart keeps the mode it was set to.

use std::sync::Arc;
use std::time::Duration;

use imperium_api::PermissionMode;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::SessionControlResult;
use serde_json::{Value, json};

fn daemon(home: &std::path::Path) -> Arc<DaemonState> {
    // Nothing listens on this endpoint; no model call is made.
    let config = r#"
[models]
default = "a:gpt-4o"

[endpoints.a]
base_url = "http://127.0.0.1:9/v1"

[permissions]
mode = "accept_edits"

[workflow]
enabled = false
"#;
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

async fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Response {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(20), deferred)
            .await
            .expect("an answer within 20 s"),
        (None, None) => panic!("no reply"),
    }
}

fn result(response: Response) -> Value {
    match response.outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{error:?}"),
    }
}

#[tokio::test]
async fn set_permission_mode_changes_the_session_and_survives_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path());
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let session_id = session.id().to_string();
    // New sessions start in `permissions.mode`.
    assert_eq!(session.permission_mode(), PermissionMode::AcceptEdits);

    let mut connection = Connection::new(Arc::clone(&state));
    let handshake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": {"major": 0, "minor": 14}});
    result(call(&mut connection, 1, "system.handshake", handshake).await);
    let listed = result(call(&mut connection, 2, "session.list", json!({})).await);
    assert_eq!(listed["sessions"][0]["permission_mode"], "accept_edits");

    let control = |mode: Value| json!({"session_id": session_id, "action": "set_permission_mode", "permission_mode": mode});
    let set: SessionControlResult = serde_json::from_value(result(
        call(
            &mut connection,
            3,
            "session.control",
            control(json!("plan")),
        )
        .await,
    ))
    .unwrap();
    assert_eq!(set.permission_mode.as_deref(), Some("plan"));
    assert_eq!(session.permission_mode(), PermissionMode::Plan);

    for (id, mode) in [(4, json!("yolo")), (5, Value::Null)] {
        let refused = call(&mut connection, id, "session.control", control(mode)).await;
        match refused.outcome {
            Outcome::Error(error) => assert_eq!(error.code, codes::INVALID_PARAMS),
            Outcome::Result(value) => panic!("expected an error, got {value}"),
        }
    }
    assert_eq!(session.permission_mode(), PermissionMode::Plan);
    let listed = result(call(&mut connection, 6, "session.list", json!({})).await);
    assert_eq!(listed["sessions"][0]["permission_mode"], "plan");

    // After a restart, the session keeps the mode it was set to.
    drop(connection);
    drop(session);
    drop(state);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let restarted = daemon(home.path());
    assert_eq!(restarted.resume_sessions(), 1);
    assert_eq!(
        restarted.session(&session_id).unwrap().permission_mode(),
        PermissionMode::Plan
    );
}
