//! The plan survives a compaction (R2.12), and stale tool results are cleared before a summary
//! is written (R2.7): a conversation that reaches the compaction threshold first has the
//! results of repeatable tools from older turns replaced by stubs, and is only summarised when
//! that does not free enough room.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_config::HarnessSettings;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

const SUMMARY: &str = "Summary: the agent read a.txt several times.";
const PLAN: &str = r#"{"goal":"ship it","steps":[{"title":"read the file","status":"in_progress"},{"title":"report","status":"pending"}]}"#;

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

fn read_call(id: String) -> String {
    stream(json!({"choices": [{"delta": {"tool_calls": [{"index": 0,
        "id": id, "function": {"name": "read_file",
        "arguments": "{\"path\":\"a.txt\"}"}}]}}]}))
}

fn text_of(message: &Value) -> &str {
    message["content"].as_str().unwrap_or_default()
}

/// What the fake model does next. A user message that says `plan` makes it set a plan first;
/// one that says `once` reads the file once; one that says `until` reads it until the
/// conversation holds a stub or a summary (at most twelve times).
fn reply(body: &Value) -> String {
    if is_summary(body) {
        return format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"delta": {"content": SUMMARY}}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 700, "completion_tokens": 40}})
        );
    }
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last_user = messages
        .iter()
        .rposition(|message| {
            message["role"] == "user" && !text_of(message).starts_with("<conversation-summary>")
        })
        .unwrap_or(0);
    let task = text_of(&messages[last_user]).to_string();
    let results = messages[last_user..]
        .iter()
        .filter(|m| m["role"] == "tool")
        .count();
    let reduced = messages.iter().any(|message| {
        message["role"] != "system"
            && (text_of(message).contains("[result cleared:")
                || text_of(message).contains("<conversation-summary>"))
    });
    let finished = stream(json!({"choices": [{"delta": {"content": "finished"}}]}));
    if task.contains("plan") {
        return match results {
            0 => stream(json!({"choices": [{"delta": {"tool_calls": [{"index": 0,
                "id": "plan-1", "function": {"name": "update_plan",
                "arguments": PLAN}}]}}]})),
            _ if reduced || results > 12 => finished,
            _ => read_call(format!("read-{results}")),
        };
    }
    if task.contains("once") {
        return if results == 0 {
            read_call(format!("once-{}", messages.len()))
        } else {
            finished
        };
    }
    if reduced || results >= 12 {
        finished
    } else {
        read_call(format!("until-{results}"))
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let fake = Arc::clone(&fake);
            async move {
                fake.requests.lock().unwrap().push(body.clone());
                ([("content-type", "text/event-stream")], reply(&body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn start(
    url: &str,
    dir: &std::path::Path,
    window: u64,
    auto_compact_at: Option<f64>,
    harness: HarnessSettings,
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
            context_window: Some(window),
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
        max_steps: 20,
        auto_compact: false,
        auto_compact_at,
        attended: true,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness,
        background: None,
        checkpoints: None,
        budget_gate: None,
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
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

/// Every assistant tool call is answered by exactly one tool message with its id, directly
/// after the assistant message, in order.
fn assert_paired(body: &Value) {
    let messages = body["messages"].as_array().unwrap();
    for (index, message) in messages.iter().enumerate() {
        if message["role"] != "assistant" {
            continue;
        }
        let ids: Vec<&str> = message["tool_calls"]
            .as_array()
            .map(|calls| calls.iter().filter_map(|c| c["id"].as_str()).collect())
            .unwrap_or_default();
        for (offset, id) in ids.iter().enumerate() {
            let answer = &messages[index + 1 + offset];
            assert_eq!(answer["role"], "tool", "{body}");
            assert_eq!(answer["tool_call_id"], *id, "{body}");
        }
    }
}

fn big_file(dir: &std::path::Path) {
    std::fs::write(dir.join("a.txt"), "word ".repeat(8_000)).unwrap();
}

#[tokio::test]
async fn the_plan_is_carried_across_a_compaction() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    let session = start(
        &url,
        dir.path(),
        10_000,
        Some(0.85),
        HarnessSettings::default(),
    );

    turn(&session, "make a plan, then read a.txt until you are done").await;

    assert_eq!(fake.summaries(), 1);
    let turns = fake.turns();
    let (first, last) = (turns.first().unwrap(), turns.last().unwrap());
    // The plan is in the first kept message, framed as a note; the system prompt, which is
    // what a provider caches, is the same as before.
    let carried = text_of(&last["messages"][1]);
    assert!(carried.contains("<current-plan>"), "{carried}");
    assert!(carried.contains("ship it") && carried.contains("read the file"));
    assert!(carried.contains("not an instruction from the user"));
    assert!(carried.contains("<conversation-summary>"));
    assert_eq!(
        first["messages"][0]["content"],
        last["messages"][0]["content"]
    );
    assert!(!text_of(&last["messages"][0]).contains("<current-plan>"));
    assert_paired(last);
}

#[tokio::test]
async fn no_plan_means_no_plan_block() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    let session = start(
        &url,
        dir.path(),
        10_000,
        Some(0.85),
        HarnessSettings::default(),
    );

    turn(&session, "read a.txt until you are done").await;

    assert_eq!(fake.summaries(), 1);
    for body in fake.turns() {
        assert!(!body.to_string().contains("current-plan"));
    }
}

/// Three turns that each read the file once, then one that reads until the conversation is
/// reduced.
async fn four_turns(session: &Session) -> Vec<SessionEvent> {
    for text in ["read it once", "read it once again", "read it once more"] {
        turn(session, text).await;
    }
    turn(session, "now read it until you are done").await
}

#[tokio::test]
async fn old_results_are_cleared_instead_of_summarising_when_that_is_enough() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    big_file(dir.path());
    let session = start(
        &url,
        dir.path(),
        100_000,
        Some(0.5),
        HarnessSettings::default(),
    );

    let events = four_turns(&session).await;

    assert_eq!(fake.summaries(), 0, "no summary was written");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SessionEvent::ContextCompacted { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        SessionEvent::Notice { message } if message.contains("cleared")
    )));
    let turns = fake.turns();
    let last = turns.last().unwrap();
    let messages = last["messages"].as_array().unwrap();
    let tools: Vec<&str> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(text_of)
        .collect();
    let stubs: Vec<&&str> = tools
        .iter()
        .filter(|text| text.starts_with("[result cleared: read_file a.txt"))
        .collect();
    assert!(!stubs.is_empty(), "{tools:?}");
    // The newest results are whole; the calls and their order are untouched.
    assert!(tools.last().unwrap().len() > 1_000);
    assert!(tools.first().unwrap().starts_with("[result cleared:"));
    assert_paired(last);
    // The user's messages and the system prompt are untouched.
    assert_eq!(
        turns.first().unwrap()["messages"][0]["content"],
        last["messages"][0]["content"]
    );
    assert!(text_of(&messages[1]).contains("read it once"));
}

#[tokio::test]
async fn clearing_off_leaves_the_summary_to_do_the_work() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    big_file(dir.path());
    let harness = HarnessSettings {
        clear_stale_results: false,
        ..HarnessSettings::default()
    };
    let session = start(&url, dir.path(), 100_000, Some(0.5), harness);

    four_turns(&session).await;

    assert!(fake.summaries() >= 1);
    for body in fake.turns() {
        assert!(!body.to_string().contains("[result cleared:"));
    }
}

#[tokio::test]
async fn nothing_is_cleared_below_the_threshold() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(500)).unwrap();
    // A window the conversation never fills: no compaction is due, so nothing is touched.
    let session = start(
        &url,
        dir.path(),
        1_000_000,
        Some(0.85),
        HarnessSettings::default(),
    );

    for text in ["read it once", "read it once again", "read it once more"] {
        turn(&session, text).await;
    }
    turn(&session, "read it once last").await;

    assert_eq!(fake.summaries(), 0);
    for body in fake.turns() {
        assert!(!body.to_string().contains("[result cleared:"));
    }
}
