//! Mid-turn compaction that is skipped as not worth it (the system prompt and tools alone take
//! most of the trigger) says so once for the agent, not on every check.

use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;
use serde_json::{Value, json};

const WINDOW: u64 = 20_000;
/// Over half the window, which is where mid-turn compaction starts at `context.auto_compact_at`
/// 0.5: the instructions alone take most of it, so summarising could not help.
const REPORTED_PROMPT: u64 = 12_000;

fn stream(chunk: Value) -> String {
    let usage =
        json!({"choices": [], "usage": {"prompt_tokens": REPORTED_PROMPT, "completion_tokens": 3}});
    format!("data: {chunk}\n\ndata: {usage}\n\ndata: [DONE]\n\n")
}

/// Reads `a.txt` when the last message is not a tool result, else answers.
async fn serve() -> String {
    let chat = post(|Json(body): Json<Value>| async move {
        let last_is_tool = body["messages"]
            .as_array()
            .and_then(|messages| messages.last())
            .is_some_and(|message| message["role"] == "tool");
        let chunk = if last_is_tool {
            json!({"choices": [{"delta": {"content": "It says alpha."}}]})
        } else {
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call-1",
                "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}}]}}]})
        };
        ([("content-type", "text/event-stream")], stream(chunk))
    });
    let app = Router::new().route("/v1/chat/completions", chat);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base_url
}

fn start(dir: &std::path::Path, base_url: String) -> Session {
    Session::spawn(SessionConfig {
        id: "notice".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "hosted:big".into(),
            endpoint: Endpoint {
                base_url,
                api_key: None,
            },
            model: "q".into(),
            pricing: None,
            local: false,
            context_window: Some(WINDOW),
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
        // Roughly 9,000 tokens by the engine's estimate (one per short word).
        instructions: Some("alpha ".repeat(9_000)),
        personality: None,
        max_steps: 5,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: Some(0.5),
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".journal/notice.jsonl"),
    })
    .unwrap()
}

/// Sends a message and returns the notices the root agent gave until its turn completed.
async fn run_turn(session: &Session, text: &str) -> Vec<String> {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut notices = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::Notice { message } => notices.push(message),
            SessionEvent::TurnCompleted { .. } => return notices,
            SessionEvent::TurnFailed { message } => panic!("turn failed: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_skipped_mid_turn_compaction_is_noticed_once() {
    let base_url = serve().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha beta gamma\n").unwrap();
    let session = start(dir.path(), base_url);
    let skipped = |notices: &[String]| {
        notices
            .iter()
            .filter(|message| message.contains("compacting it would not help"))
            .count()
    };

    let first = run_turn(&session, "please read a.txt").await;
    assert_eq!(skipped(&first), 1, "{first:?}");
    // After a skip the check backs off until the conversation has grown by four messages and a
    // reply and a result (the `compact_mark`), so the second turn is too short to check again.
    // The third is long enough: it runs the same check, finds the same thing, and the
    // once-per-agent flag keeps it quiet. (Without the flag, this assertion fails.)
    let second = run_turn(&session, "and read it again").await;
    assert_eq!(skipped(&second), 0, "{second:?}");
    let third = run_turn(&session, "and once more").await;
    assert_eq!(skipped(&third), 0, "{third:?}");
}
