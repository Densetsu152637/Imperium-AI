//! Mid-turn compaction: an agent that nears its model's context window during a turn replaces
//! older messages with a summary before its next model call and carries on, keeping the system
//! prompt as it was. With the threshold off, the turn is left alone.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{BudgetGate, BudgetVerdict, ResolvedModel, Session, SessionConfig};
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
                    format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"choices": [{"delta": {"content": SUMMARY}}]}),
                        json!({"choices": [], "usage": {"prompt_tokens": 700, "completion_tokens": 40}})
                    )
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
    start_gated(url, dir, auto_compact_at, None)
}

fn start_gated(
    url: &str,
    dir: &std::path::Path,
    auto_compact_at: Option<f64>,
    budget_gate: Option<Arc<dyn BudgetGate>>,
) -> Session {
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
        harness: Default::default(),
        background: None,
        checkpoints: None,
        budget_gate,
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
        // Aimed well under the 5,000-token trigger.
        assert!(after <= 4_000 + 400, "{after}");
    }
}

/// Lets calls through while fewer than `allowed` requests reached the fake, then blocks.
struct BlockAfter {
    fake: Arc<Fake>,
    allowed: usize,
}

impl std::fmt::Debug for BlockAfter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BlockAfter")
    }
}

impl BudgetGate for BlockAfter {
    fn check(&self, _spec: &str) -> BudgetVerdict {
        if self.fake.requests.lock().unwrap().len() >= self.allowed {
            BudgetVerdict::Blocked("Daily is used up; model calls are blocked.".into())
        } else {
            BudgetVerdict::Go
        }
    }

    fn confirm(&self, _key: &str) {}
}

/// Blocks once switched on.
#[derive(Debug)]
struct Switch(std::sync::atomic::AtomicBool);

impl BudgetGate for Switch {
    fn check(&self, _spec: &str) -> BudgetVerdict {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            BudgetVerdict::Blocked("Daily is used up; model calls are blocked.".into())
        } else {
            BudgetVerdict::Go
        }
    }

    fn confirm(&self, _key: &str) {}
}

#[tokio::test]
async fn a_blocked_budget_stops_the_turn_before_it_compacts() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake), true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    // A threshold the conversation is over after the first read.
    let gate = Arc::new(BlockAfter {
        fake: Arc::clone(&fake),
        allowed: 1,
    });
    let session = start_gated(&url, dir.path(), Some(0.05), Some(gate));
    let mut events = session.subscribe();
    session
        .send("read a.txt until you are done".into())
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::TurnFailed { message } => {
                assert!(message.contains("blocked"), "{message}");
                break;
            }
            SessionEvent::TurnCompleted { .. } => panic!("the turn completed"),
            SessionEvent::CompactionStarted { .. } => panic!("it compacted"),
            _ => {}
        }
    }
    assert_eq!(fake.summaries(), 0, "no summary request was made");
    assert_eq!(fake.turns().len(), 1, "only the first call went out");
}

#[tokio::test]
async fn a_manual_compaction_is_gated_on_its_model() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake), true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    // The turns run freely; the budget is used up after them.
    let gate = Arc::new(Switch(std::sync::atomic::AtomicBool::new(false)));
    let session = start_gated(&url, dir.path(), None, Some(gate.clone()));
    // Build a conversation with a few turns.
    for text in ["one", "two", "three", "four", "five"] {
        let mut events = session.subscribe();
        session.send(text.into()).unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
                .await
                .expect("an event within 20 s")
                .unwrap()
                .event;
            if matches!(
                event,
                SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
            ) {
                break;
            }
        }
    }
    gate.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let before = fake.requests.lock().unwrap().len();
    let outcome = session.compact().await;
    assert!(outcome.is_err(), "the budget stopped the summary call");
    assert_eq!(fake.summaries(), 0);
    assert_eq!(fake.requests.lock().unwrap().len(), before);
}

#[tokio::test]
async fn a_compaction_summary_counts_as_a_model_call() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake), true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    let session = start(&url, dir.path(), Some(0.85));
    let events = turn(&session, "read a.txt until you are done").await;
    assert_eq!(fake.summaries(), 1);
    // The summary call's usage was recorded like any other call's, for budgets to count.
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::ModelCallCompleted { model, usage, .. }
            if model == "a:model" && usage.prompt_tokens == 700 && usage.completion_tokens == 40
    )));
}
