//! Model download suggestions against a fake Hugging Face Hub: a session that prefers local
//! models but has none is offered one once, turning `models.suggest_downloads` off suppresses
//! it, "never" turns the setting off in the user file, and accepting starts a download whose
//! model is then ready to switch to.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::Path as UrlPath;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// The smallest GGUF the scanner accepts: a version 3 header with no tensors or keys.
fn gguf() -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes
}

/// A Hub serving `acme/tiny-GGUF` with two quantisations (no checksums listed).
async fn fake_hub() -> String {
    let files: Arc<Vec<(String, Vec<u8>)>> = Arc::new(vec![
        ("tiny-Q4_K_M.gguf".into(), gguf()),
        ("tiny-Q8_0.gguf".into(), [gguf(), vec![0; 64]].concat()),
    ]);
    let listed = Arc::clone(&files);
    let tree = move || {
        let files = Arc::clone(&listed);
        async move {
            let entries: Vec<Value> = files
                .iter()
                .map(|(path, body)| json!({"type": "file", "path": path, "size": body.len()}))
                .collect();
            axum::Json(entries).into_response()
        }
    };
    let resolve = move |UrlPath((_, _, _, path)): UrlPath<(String, String, String, String)>| async move {
        match files.iter().find(|(name, _)| *name == path) {
            Some((_, body)) => body.clone().into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    };
    let app: Router = Router::new()
        .route("/api/models/{owner}/{name}/tree/{revision}", get(tree))
        .route("/{owner}/{name}/resolve/{revision}/{*path}", get(resolve));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn daemon(home: &std::path::Path, suggest: bool) -> Arc<DaemonState> {
    let downloads = home.join("models").display().to_string().replace('\\', "/");
    // Nothing listens on the endpoint, so every turn fails quickly; only its start matters.
    let config = format!(
        "[models]\ndefault = \"a:m\"\ndownload_dir = '{downloads}'\nsuggest_downloads = {suggest}\n\
         [[models.suggestions]]\nrepo = \"acme/tiny-GGUF\"\ncapabilities = [\"chat\"]\ncontext = 4096\n\
         [endpoints.a]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
         [routing]\nprefer = \"local\"\n\
         [hardware]\nvram_mb = 8192\nram_mb = 16384\n\
         [workflow]\nenabled = false"
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
    let state = DaemonState::new(imperium_config::Paths::under(home), settings, None);
    state.use_hub(imperium_local::hf::Hub::new(fake_hub().await, None));
    state
}

/// Waits for the first root event `wanted` accepts.
async fn wait_for<T>(
    events: &mut broadcast::Receiver<AgentEvent>,
    mut wanted: impl FnMut(&SessionEvent) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let event = events.recv().await.expect("the session is running");
            if event.agent_id == ROOT_AGENT
                && let Some(found) = wanted(&event.event)
            {
                return found;
            }
        }
    })
    .await
    .expect("the event arrives within 20 s")
}

/// Waits for the root's turn to end, then a little longer, and returns the ids of the
/// suggestions seen meanwhile.
async fn suggestions_in_turn(events: &mut broadcast::Receiver<AgentEvent>) -> Vec<String> {
    let mut seen = Vec::new();
    let mut count = |event: &SessionEvent| {
        if let Some(id) = suggestion_id(event) {
            seen.push(id);
        }
    };
    wait_for(events, |event| {
        count(event);
        matches!(
            event,
            SessionEvent::TurnFailed { .. } | SessionEvent::TurnCompleted { .. }
        )
        .then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    while let Ok(event) = events.try_recv() {
        if event.agent_id == ROOT_AGENT {
            count(&event.event);
        }
    }
    seen
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

async fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    let handshake =
        json!({"client": {"name": "test", "version": "0"}, "protocol": PROTOCOL_VERSION});
    call(&mut connection, 1, "system.handshake", handshake).await;
    connection
}

fn suggestion_id(event: &SessionEvent) -> Option<String> {
    match event {
        SessionEvent::ModelSuggestion {
            id,
            need,
            repo,
            quant,
            model,
            fit,
            ..
        } => {
            assert_eq!(
                (need.as_str(), repo.as_str(), quant.as_str(), fit.as_str()),
                ("local", "acme/tiny-GGUF", "Q4_K_M", "gpu")
            );
            assert_eq!(model, "llamacpp:tiny-Q4_K_M");
            Some(id.clone())
        }
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suggestion_is_made_once_and_accepting_downloads_the_model() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), true).await;
    let (session, mut events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let session_id = session.id().to_string();

    session.send("hello".into()).unwrap();
    let mut first = suggestions_in_turn(&mut events).await;
    if first.is_empty() {
        first.push(wait_for(&mut events, suggestion_id).await);
    }
    assert_eq!(first.len(), 1);
    let id = first.remove(0);
    // The same need is not suggested again in this session.
    session.send("hello again".into()).unwrap();
    assert!(suggestions_in_turn(&mut events).await.is_empty());

    let mut connection = connect(&state).await;
    let respond = |id: &str, action: &str| json!({"session_id": session_id, "suggestion_id": id, "action": action});
    let unknown = call(
        &mut connection,
        2,
        "session.suggestion_respond",
        respond("ms-99", "download"),
    )
    .await;
    assert!(matches!(unknown.outcome, Outcome::Error(_)));

    let accepted = call(
        &mut connection,
        3,
        "session.suggestion_respond",
        respond(&id, "download"),
    )
    .await;
    let Outcome::Result(result) = accepted.outcome else {
        panic!("download refused: {accepted:?}");
    };
    let job_id = result["job"]["job_id"].as_str().unwrap().to_string();
    assert_eq!(result["job"]["variant"], "Q4_K_M");

    let model = wait_for(&mut events, |event| match event {
        SessionEvent::ModelSuggestionUpdate {
            state,
            job_id: Some(job),
            model,
            message,
            ..
        } if state != "downloading" => {
            assert_eq!(job, &job_id);
            assert_eq!(state, "ready", "{message:?}");
            model.clone()
        }
        _ => None,
    })
    .await;
    assert_eq!(model, "llamacpp:tiny-Q4_K_M");
    let local = state.local_models();
    assert!(local.models.iter().any(|row| row.id == "tiny-Q4_K_M"));
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_the_setting_off_suppresses_suggestions() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), false).await;
    let (session, mut events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    session.send("hello".into()).unwrap();
    assert!(suggestions_in_turn(&mut events).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn never_turns_the_setting_off_and_saves_it() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), true).await;
    let (session, mut events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    session.send("hello".into()).unwrap();
    let id = wait_for(&mut events, suggestion_id).await;

    let mut connection = connect(&state).await;
    let params = json!({"session_id": session.id(), "suggestion_id": id, "action": "never"});
    let answered = call(&mut connection, 2, "session.suggestion_respond", params).await;
    assert!(matches!(answered.outcome, Outcome::Result(_)));
    wait_for(&mut events, |event| match event {
        SessionEvent::ModelSuggestionUpdate { state, .. } => (state == "disabled").then_some(()),
        _ => None,
    })
    .await;
    assert!(!state.suggestions().enabled());
    let saved =
        std::fs::read_to_string(imperium_config::Paths::under(home.path()).user_config_file())
            .unwrap();
    assert!(saved.contains("suggest_downloads = false"), "{saved}");
}
