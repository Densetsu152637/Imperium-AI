//! `session.control` `set_model`: a model table id switches the session (no window is known,
//! so nothing is compacted), unknown models and actions are refused, and a session resumed
//! after a restart keeps the model it switched to. A local model is checked against the model
//! folders but not loaded when it is selected; the first message loads it.

use std::sync::Arc;
use std::time::Duration;

use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::SessionControlResult;
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};

fn daemon(home: &std::path::Path) -> Arc<DaemonState> {
    // Nothing listens on these endpoints; no model call is made.
    let config = "[models]\ndefault = \"a:old\"\n\
                  [endpoints.a]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
                  [endpoints.b]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
                  [[models.catalog]]\nid = \"newer\"\nspec = \"b:new\"\nlocation = \"cloud\"\n\
                  tier = \"standard\"\n\
                  [workflow]\nenabled = false";
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
async fn set_model_switches_by_table_id_and_survives_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path());
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let session_id = session.id().to_string();
    assert_eq!(session.model().as_deref(), Some("a:old"));

    let mut connection = Connection::new(Arc::clone(&state));
    let handshake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": {"major": 0, "minor": 2}});
    result(call(&mut connection, 1, "system.handshake", handshake).await);

    let control = |action: &str, model: Option<&str>| json!({"session_id": session_id, "action": action, "model": model});
    let switched: SessionControlResult = serde_json::from_value(result(
        call(
            &mut connection,
            2,
            "session.control",
            control("set_model", Some("newer")),
        )
        .await,
    ))
    .unwrap();
    assert_eq!(switched.model.as_deref(), Some("b:new"));
    let context = switched.context.expect("the conversation was measured");
    // The model table gives no window, so the conversation was not checked or compacted.
    assert_eq!((context.context_window, context.budget), (None, None));
    assert!(context.compaction.is_none());
    assert_eq!(session.model().as_deref(), Some("b:new"));
    // session.list shows the new model.
    let listed = result(call(&mut connection, 7, "session.list", json!({})).await);
    assert_eq!(listed["sessions"][0]["model"], "b:new");
    let (history, _) = session.subscribe_with_history();
    assert!(history.iter().any(|event| event.event
        == SessionEvent::ModelChanged {
            from: "a:old".into(),
            to: "b:new".into(),
            compacted: false,
        }));

    // Unknown models and actions are refused, and nothing changes.
    let unknown = call(
        &mut connection,
        3,
        "session.control",
        control("set_model", Some("nowhere:x")),
    )
    .await;
    assert_eq!(error_code(unknown), codes::MODEL_UNAVAILABLE);
    let bad = call(&mut connection, 4, "session.control", control("swap", None)).await;
    assert_eq!(error_code(bad), codes::INVALID_PARAMS);
    // Compacting needs a known window.
    let compact = call(
        &mut connection,
        5,
        "session.control",
        control("compact", None),
    )
    .await;
    assert_eq!(error_code(compact), codes::INVALID_PARAMS);
    // Other actions still answer with an empty result.
    let resumed = result(
        call(
            &mut connection,
            6,
            "session.control",
            control("resume_now", None),
        )
        .await,
    );
    assert_eq!(resumed, json!({"model": null, "context": null}));
    assert_eq!(session.model().as_deref(), Some("b:new"));

    // After a restart, the session continues on the model it switched to.
    drop(connection);
    drop(session);
    drop(state);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let restarted = daemon(home.path());
    assert_eq!(restarted.resume_sessions(), 1);
    assert_eq!(
        restarted.session(&session_id).unwrap().model().as_deref(),
        Some("b:new")
    );
}

/// The smallest GGUF the scanner accepts: a version 3 header with no tensors or keys.
fn gguf() -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_model_is_checked_but_not_loaded_until_the_session_sends_to_it() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let models = tempfile::tempdir().unwrap();
    let repo = models.path().join("unsloth").join("Tiny-GGUF");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("tiny-Q4_K_M.gguf"), gguf()).unwrap();
    // A server that does not exist: any load fails.
    let server = home.path().join("no-such-llama-server");
    let config = format!(
        "[models]\ndefault = \"a:old\"\nfolders = ['{}']\n\
         [endpoints.a]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
         [runtime.llamacpp]\ncustom_path = '{}'\n\
         [hardware]\nvram_mb = 0\nram_mb = 16384\n\
         [workflow]\nenabled = false",
        models.path().display(),
        server.display()
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
    let paths = imperium_config::Paths::under(home.path());
    let local = imperium_core::LocalRuntime::start(&paths, &settings).expect("a model folder");
    let state = DaemonState::new(paths, settings, Some(local));
    let id = state.local_models().models[0].id.clone();
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let session_id = session.id().to_string();
    let mut connection = Connection::new(Arc::clone(&state));
    let handshake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": {"major": 0, "minor": 2}});
    result(call(&mut connection, 1, "system.handshake", handshake).await);
    let control =
        |model: &str| json!({"session_id": session_id, "action": "set_model", "model": model});

    // Selecting the local model succeeds without starting its server.
    let spec = format!("llamacpp:{id}");
    let switched: SessionControlResult = serde_json::from_value(result(
        call(&mut connection, 2, "session.control", control(&spec)).await,
    ))
    .unwrap();
    assert_eq!(switched.model.as_deref(), Some(spec.as_str()));
    let context = switched.context.expect("an estimate");
    assert_eq!(
        context.method,
        imperium_protocol::session::TokenCountMethod::Estimated
    );
    assert!(context.context_window.is_some(), "the planned context");
    assert!(state.loaded_local_models().is_empty());

    // A local model the folders do not have is refused at once.
    let missing = call(
        &mut connection,
        3,
        "session.control",
        control("llamacpp:no-such-model"),
    )
    .await;
    assert_eq!(error_code(missing), codes::MODEL_UNAVAILABLE);
    assert_eq!(session.model().as_deref(), Some(spec.as_str()));

    // Sending loads it, and the load failure fails that turn with the reason.
    let mut events = session.subscribe();
    session.send("hello".into()).unwrap();
    let failure = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.recv())
            .await
            .expect("an event within 30 s")
            .unwrap()
            .event;
        if let SessionEvent::TurnFailed { message } = event {
            break message;
        }
    };
    assert!(
        failure.contains(&format!("could not load local model {id}")),
        "{failure}"
    );

    // The session can switch to another model afterwards.
    let back: SessionControlResult = serde_json::from_value(result(
        call(&mut connection, 4, "session.control", control("a:old")).await,
    ))
    .unwrap();
    assert_eq!(back.model.as_deref(), Some("a:old"));
}
