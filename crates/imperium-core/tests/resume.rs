//! A session continues after a restart: clients see its earlier transcript, the model sees the
//! earlier conversation, and a turn the restart cut off is reported as failed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;
use serde_json::{Value, json};

/// Answers every call and records how many messages it was sent.
async fn model(seen: Arc<Mutex<Vec<usize>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let seen = Arc::clone(&seen);
            async move {
                seen.lock()
                    .unwrap()
                    .push(body["messages"].as_array().map_or(0, Vec::len));
                let chunk = json!({"choices":[{"delta":{"content":"noted"}}]});
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

fn start(url: &str, dir: &std::path::Path, resume: bool) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: url.into(),
                api_key: None,
            },
            model: "model".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "openai".into(),
        },
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        clients: Default::default(),
        local: None,
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume,
        resume_policy: imperium_core::default_resume_policy(),
        instructions: None,
        max_steps: 3,
        background: None,
        journal_path: dir.join(".sessions/s.jsonl"),
    })
    .unwrap()
}

async fn finish_turn(session: &Session, text: &str) {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if matches!(event, SessionEvent::TurnCompleted { .. }) {
            return;
        }
    }
}

#[tokio::test]
async fn sessions_continue_after_a_restart() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = model(Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();

    let first = start(&url, dir.path(), false);
    finish_turn(&first, "remember the number 7").await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let resumed = start(&url, dir.path(), true);
    let (history, _) = resumed.subscribe_with_history();
    assert!(history.iter().any(|event| matches!(&event.event,
        SessionEvent::TurnStarted { text } if text == "remember the number 7")));
    finish_turn(&resumed, "what was the number?").await;
    // First call: system + user. After resuming: system + user + reply + new user.
    assert_eq!(*seen.lock().unwrap(), [2, 4]);

    // A journal ending mid-turn is closed off when the session resumes.
    drop(resumed);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let journal = dir.path().join(".sessions/s.jsonl");
    let cut =
        r#"{"at_ms":0,"agent_id":"root","event":{"type":"turn_started","text":"interrupted"}}"#;
    let mut text = std::fs::read_to_string(&journal).unwrap();
    text.push_str(cut);
    text.push('\n');
    std::fs::write(&journal, text).unwrap();
    let after_crash = start(&url, dir.path(), true);
    // The session reports the cut-off turn as soon as its task starts.
    let mut closed_off = false;
    for _ in 0..50 {
        let (history, _) = after_crash.subscribe_with_history();
        closed_off = history.iter().rev().take(3).any(|event| {
            matches!(&event.event,
            SessionEvent::TurnFailed { message } if message.contains("stopped during this turn"))
        });
        if closed_off {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(closed_off);
}
