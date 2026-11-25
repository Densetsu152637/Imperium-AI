//! Reasoning effort: sessions start at `models.effort`, `session.control` `set_effort` changes
//! it (refusing unknown levels), lists and model rows say whether the model can reason, and a
//! session resumed after a restart keeps the effort it was set to.

use std::sync::Arc;
use std::time::Duration;

use imperium_api::Effort;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::SessionControlResult;
use serde_json::{Value, json};

fn daemon(home: &std::path::Path) -> Arc<DaemonState> {
    // Nothing listens on this endpoint; no model call is made.
    let config = r#"
[models]
default = "a:o3"
effort = "high"

[endpoints.a]
base_url = "http://127.0.0.1:9/v1"

[[models.catalog]]
id = "plain"
spec = "a:gpt-4o"
location = "cloud"
tier = "standard"

[[models.catalog]]
id = "tagged"
spec = "a:house-model"
location = "cloud"
tier = "standard"
tags = ["thinking"]

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

fn error_code(response: Response) -> i32 {
    match response.outcome {
        Outcome::Error(error) => error.code,
        Outcome::Result(value) => panic!("expected an error, got {value}"),
    }
}

#[tokio::test]
async fn set_effort_changes_the_session_and_survives_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path());
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let session_id = session.id().to_string();
    // New sessions start at `models.effort`.
    assert_eq!(session.effort(), Effort::High);

    let mut connection = Connection::new(Arc::clone(&state));
    let handshake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": {"major": 0, "minor": 9}});
    result(call(&mut connection, 1, "system.handshake", handshake).await);

    let listed = result(call(&mut connection, 2, "session.list", json!({})).await);
    assert_eq!(listed["sessions"][0]["effort"], "high");
    assert_eq!(listed["sessions"][0]["reasoning"], true);

    let control = |id: &str, action: &str, extra: Value| {
        let mut params = json!({"session_id": id, "action": action});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        params
    };
    let set: SessionControlResult = serde_json::from_value(result(
        call(
            &mut connection,
            3,
            "session.control",
            control(&session_id, "set_effort", json!({"effort": "max"})),
        )
        .await,
    ))
    .unwrap();
    assert_eq!(set.effort.as_deref(), Some("max"));
    assert_eq!(set.reasoning, Some(true));
    assert_eq!(session.effort(), Effort::Max);

    // Unknown or missing levels are refused, and nothing changes.
    for (id, extra) in [(4, json!({"effort": "extreme"})), (5, json!({}))] {
        let refused = call(
            &mut connection,
            id,
            "session.control",
            control(&session_id, "set_effort", extra),
        )
        .await;
        assert_eq!(error_code(refused), codes::INVALID_PARAMS);
    }
    assert_eq!(session.effort(), Effort::Max);

    // Switching to a model that cannot reason says so; the effort stays for later.
    let switched: SessionControlResult = serde_json::from_value(result(
        call(
            &mut connection,
            6,
            "session.control",
            control(&session_id, "set_model", json!({"model": "plain"})),
        )
        .await,
    ))
    .unwrap();
    assert_eq!(switched.reasoning, Some(false));
    let listed = result(call(&mut connection, 7, "session.list", json!({})).await);
    assert_eq!(listed["sessions"][0]["effort"], "max");
    assert_eq!(listed["sessions"][0]["reasoning"], false);

    // After a restart, the session keeps the effort it was set to.
    drop(connection);
    drop(session);
    drop(state);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let restarted = daemon(home.path());
    assert_eq!(restarted.resume_sessions(), 1);
    assert_eq!(
        restarted.session(&session_id).unwrap().effort(),
        Effort::Max
    );
}

#[tokio::test]
async fn model_rows_say_which_models_can_reason() {
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path());
    let table = state.model_table();
    let reasoning = |id: &str| {
        table
            .models
            .iter()
            .find(|row| row.id == id)
            .unwrap()
            .reasoning
    };
    assert!(!reasoning("plain"));
    assert!(reasoning("tagged"));
    assert!(state.model_reasons("a:o3"));
    assert!(state.model_reasons("tagged"));
    assert!(!state.model_reasons("plain"));
}
