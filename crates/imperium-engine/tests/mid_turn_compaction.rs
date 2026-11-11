//! Mid-turn compaction: an agent that nears its model's context window during a turn replaces
//! older messages with a summary before its next model call and carries on, keeping the system
//! prompt as it was. With the threshold off, the turn is left alone.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

const SUMMARY: &str = "Summary: the agent read a.txt several times.";

#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<Value>>,
}

impl Fake {
    fn turns(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|body| !is_summary(body))
            .cloned()
            .collect()
    }

    fn summaries(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|body| is_summary(body))
            .count()
    }
}

fn is_summary(body: &Value) -> bool {
    body["messages"][0]["content"]
        .as_str()
        .is_some_and(|system| system.starts_with("You summarise"))
}

fn stream(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Reads a.txt until its conversation holds a summary (or eight results), then finishes.
async fn serve(fake: Arc<Fake>, finish_on_summary: bool) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let fake = Arc::clone(&fake);
            async move {
                fake.requests.lock().unwrap().push(body.clone());
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let summarised = messages.iter().any(|message| {
                    message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("<conversation-summary>"))
                });
                let reads = messages.iter().filter(|m| m["role"] == "tool").count();
                let reply = if is_summary(&body) {
                    stream(json!({"choices": [{"delta": {"content": SUMMARY}}]}))
                } else if (summarised && finish_on_summary) || fake.turns().len() > 8 {
                    stream(json!({"choices": [{"delta": {"content": "finished"}}]}))
                } else {
                    stream(json!({"choices": [{"delta": {"tool_calls": [{"index": 0,
                        "id": format!("call-{reads}"), "function": {"name": "read_file",
                        "arguments": "{\"path\":\"a.txt\"}"}}]}}]}))
                };
                ([("content-type", "text/event-stream")], reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn start(url: &str, dir: &std::path::Path, auto_compact_at: Option<f64>) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "a:model".into(),
            endpoint: Endpoint {
                base_url: url.into(),
                api_key: None,
            },
            model: "model".into(),
            pricing: None,
            local: false,
            context_window: Some(10_000),
            kind: "openai".into(),
            reasoning: None,
        }),
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
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 12,
        auto_compact: false,
        auto_compact_at,
        background: None,
        checkpoints: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".sessions/s.jsonl"),
    })
    .unwrap()
}

async fn turn(session: &Session, text: &str) -> Vec<SessionEvent> {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut seen = Vec::new();
    loop {
        let AgentEvent { event, .. } = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        let done = matches!(event, SessionEvent::TurnCompleted { .. });
        assert!(
            !matches!(event, SessionEvent::TurnFailed { .. }),
            "turn failed: {event:?}"
        );
        seen.push(event);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn a_turn_that_crosses_the_threshold_compacts_mid_turn_and_completes() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake), true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    let session = start(&url, dir.path(), Some(0.85));

    let events = turn(&session, "read a.txt until you are done").await;

    let started = events
        .iter()
        .position(|event| matches!(event, SessionEvent::CompactionStarted { .. }))
        .expect("compaction started mid-turn");
    let compacted = events
        .iter()
        .position(|event| matches!(event, SessionEvent::ContextCompacted { .. }))
        .expect("the context was compacted mid-turn");
    assert!(started < compacted);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SessionEvent::ContextCompacted { .. }))
            .count(),
        1,
        "compacted once, not in a loop"
    );
    let SessionEvent::ContextCompacted {
        before_tokens,
        after_tokens,
        ..
    } = &events[compacted]
    else {
        unreachable!()
    };
    assert!(after_tokens < before_tokens);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, SessionEvent::TurnCompleted { .. })),
        "the turn completed after compacting"
    );
    assert_eq!(fake.summaries(), 1);

    // The system prompt is the same byte for byte before and after; the last call read the
    // summary instead of the first file reads.
    let turns = fake.turns();
    let (first, last) = (turns.first().unwrap(), turns.last().unwrap());
    assert_eq!(
        first["messages"][0]["content"],
        last["messages"][0]["content"]
    );
    let after = last["messages"].as_array().unwrap();
    assert!(
        after[1]["content"]
            .as_str()
            .unwrap()
            .starts_with("<conversation-summary>")
    );
    assert!(after.len() < turns[turns.len() - 2]["messages"].as_array().unwrap().len());
}

#[tokio::test]
async fn nothing_compacts_with_the_threshold_off() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake), true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    let session = start(&url, dir.path(), None);

    let events = turn(&session, "read a.txt until you are done").await;

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SessionEvent::ContextCompacted { .. }))
    );
    assert_eq!(fake.summaries(), 0);
}

#[tokio::test]
async fn a_low_threshold_is_clamped_and_compaction_cannot_loop() {
    let fake = Arc::new(Fake::default());
    // The model keeps reading even after a summary, so the conversation keeps growing.
    let url = serve(Arc::clone(&fake), false).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    // 0.1 is clamped to 0.5 of the 10,000-token window (the built-in tools alone take about
    // a third of that, so a smaller window would leave nothing worth compacting).
    let session = start(&url, dir.path(), Some(0.1));

    let events = turn(&session, "read a.txt until you are done").await;

    let compactions: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ContextCompacted {
                before_tokens,
                after_tokens,
                ..
            } => Some((*before_tokens, *after_tokens)),
            _ => None,
        })
        .collect();
    assert!(
        !compactions.is_empty(),
        "the clamped threshold still compacts"
    );
    // Never more than one compaction per model call; the window is so small that each read
    // reaches the trigger again, yet every compaction shrinks the conversation.
    assert!(compactions.len() <= fake.turns().len(), "{compactions:?}");
    for (before, after) in compactions {
        assert!(after < before, "{before} -> {after}");
        // Aimed well under the 4,000-token trigger.
        assert!(after <= 3_200 + 400, "{after}");
    }
}
