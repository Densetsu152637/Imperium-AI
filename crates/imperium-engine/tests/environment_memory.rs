//! An environment's `Memory.md` is part of its sessions' instructions: it is there in the first
//! model call and, byte for byte, in the calls after the conversation was compacted mid-turn
//! (compaction never touches the system prompt); agents write to it with the `environment` scope
//! of `memory_remember`, which keeps what the user wrote by hand.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_config::{MemorySettings, WritePolicy};
use imperium_engine::{ResolvedModel, Session, SessionConfig, SessionMemory};
use imperium_memory::Store;
use imperium_memory::environment::EnvironmentMemory;
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

const SUMMARY: &str = "Summary: the agent read a.txt several times.";
const NOTE: &str = "The staging database is reset every Monday.";

fn is_summary(body: &Value) -> bool {
    body["messages"][0]["content"]
        .as_str()
        .is_some_and(|system| system.starts_with("You summarise"))
}

fn stream(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Reads a.txt until its conversation holds a summary, then finishes; keeps every request.
async fn reading_model(requests: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let requests = Arc::clone(&requests);
            async move {
                requests.lock().unwrap().push(body.clone());
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let summarised = messages.iter().any(|message| {
                    message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("<conversation-summary>"))
                });
                let reads = messages.iter().filter(|m| m["role"] == "tool").count();
                let reply = if is_summary(&body) {
                    stream(json!({"choices": [{"delta": {"content": SUMMARY}}]}))
                } else if summarised || reads > 8 {
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

fn start(
    url: &str,
    dir: &std::path::Path,
    memory_file: &std::path::Path,
    auto_compact_at: Option<f64>,
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
            context_window: Some(8_000),
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
        memory: Some(SessionMemory {
            store: Store::new(&dir.join("data"), None),
            settings: MemorySettings::default(),
            environment: Some(EnvironmentMemory::new(memory_file.to_path_buf())),
        }),
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 14,
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
async fn environment_memory_is_in_every_call_including_after_compaction() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let url = reading_model(Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "word ".repeat(1_500)).unwrap();
    let memory_file = dir.path().join("env").join("Memory.md");
    std::fs::create_dir_all(memory_file.parent().unwrap()).unwrap();
    std::fs::write(&memory_file, format!("## staging\n{NOTE}\n")).unwrap();
    let session = start(&url, dir.path(), &memory_file, Some(0.85));

    let events = turn(&session, "read a.txt until you are done").await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, SessionEvent::ContextCompacted { .. })),
        "the conversation was compacted"
    );

    let requests = requests.lock().unwrap();
    let turns: Vec<&Value> = requests.iter().filter(|body| !is_summary(body)).collect();
    let first = turns.first().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap();
    let last = turns.last().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(
        first.contains("<environment-memory>") && first.contains(NOTE),
        "{first}"
    );
    assert!(
        first.contains("environment memory wins") || first.contains("follow it for work"),
        "the instructions say which memory wins"
    );
    assert_eq!(first, last, "compaction leaves the system prompt as it was");
    assert!(
        turns.last().unwrap()["messages"]
            .to_string()
            .contains("<conversation-summary>"),
        "the last call read the summary instead of the first reads"
    );
}

/// The model saves an environment memory on its first call, then finishes.
async fn saving_model() -> String {
    let step = Arc::new(Mutex::new(0usize));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(_body): axum::Json<Value>| {
            let step = Arc::clone(&step);
            async move {
                let mut step = step.lock().unwrap();
                *step += 1;
                let reply = if *step == 1 {
                    let arguments = json!({
                        "name": "staging-reset",
                        "description": "When staging is reset",
                        "type": "project",
                        "scope": "environment",
                        "body": NOTE
                    });
                    stream(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"m",
                        "function":{"name":"memory_remember","arguments":arguments.to_string()}}]}}]}))
                } else {
                    stream(json!({"choices":[{"delta":{"content":"saved"}}]}))
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

#[tokio::test]
async fn memory_remember_with_the_environment_scope_writes_a_section_and_keeps_the_rest() {
    let url = saving_model().await;
    let dir = tempfile::tempdir().unwrap();
    let memory_file = dir.path().join("env").join("Memory.md");
    std::fs::create_dir_all(memory_file.parent().unwrap()).unwrap();
    std::fs::write(&memory_file, "# Hand written\n\nKeep me.\n").unwrap();
    let session = start(&url, dir.path(), &memory_file, None);

    let events = turn(&session, "remember the staging reset").await;
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::MemorySaved { name, scope } if name == "staging-reset" && scope == "environment")));
    let text = std::fs::read_to_string(&memory_file).unwrap();
    assert!(text.starts_with("# Hand written\n\nKeep me."), "{text}");
    assert!(
        text.contains("## staging-reset") && text.contains(NOTE),
        "{text}"
    );
    // The default policy for the environment scope saves without asking.
    assert_eq!(
        MemorySettings::default().write.environment,
        WritePolicy::Auto
    );
}
