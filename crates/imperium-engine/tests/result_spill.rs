//! A tool result too large for the conversation is saved to a file in the session's results
//! folder: the model sees a preview and the path, every agent gets this, the file is readable
//! with `read_file`, it survives a restart, and a smaller result (or a session with the
//! setting off) is left alone.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;
use serde_json::{Value, json};

#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<Value>>,
}

fn stream(delta: Value) -> String {
    let chunk = json!({"choices": [{"delta": delta}],
                       "usage": {"prompt_tokens": 10, "completion_tokens": 2}});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn text(content: &str) -> String {
    stream(json!({"content": content}))
}

fn call(id: &str, name: &str, arguments: Value) -> String {
    stream(json!({"tool_calls": [{"index": 0, "id": id,
        "function": {"name": name, "arguments": arguments.to_string()}}]}))
}

fn is_subagent(body: &Value) -> bool {
    body["messages"][0]["content"]
        .as_str()
        .is_some_and(|system| system.starts_with("You are an Imperium subagent"))
}

/// The path a saved-result notice names.
fn saved_path(notice: &str) -> String {
    let rest = notice.split("saved to ").nth(1).expect("a saved-to path");
    rest.split("; read ranges").next().unwrap().to_string()
}

/// Root: `search` greps the big file (800 matching lines), `search small` the small one, and
/// when a result says where it was saved reads lines 400 to 402 of that file; `delegate`
/// spawns a subagent whose brief is `search`. Subagent: `search` greps the big file and then
/// reports what it was told. Anything else ends in `done`.
async fn answer(body: &Value) -> String {
    let last = body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .cloned()
        .unwrap_or_default();
    let said = last["content"].as_str().unwrap_or_default().to_string();
    let role = last["role"].as_str().unwrap_or_default();
    let big = json!({"pattern": "needle", "glob": "big.txt", "max_results": 1000});
    if is_subagent(body) {
        return match (role, said.as_str()) {
            ("user", "search") => call("grep", "grep", big),
            ("tool", said) if said.contains("saved to ") => text("subagent saw a saved result"),
            _ => text("subagent done"),
        };
    }
    match (role, said.as_str()) {
        ("user", "search") => call("grep", "grep", big),
        ("user", "search small") => call(
            "grep",
            "grep",
            json!({"pattern": "needle", "glob": "small.txt"}),
        ),
        ("user", "delegate") => call("spawn", "spawn_agent", json!({"brief": "search"})),
        ("tool", said) if said.contains("saved to ") => call(
            "read",
            "read_file",
            json!({"path": saved_path(said), "offset": 400, "limit": 3}),
        ),
        ("tool", said) if said.starts_with("started ") => call("await", "await_tasks", json!({})),
        _ => text("done"),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let fake = Arc::clone(&fake);
            async move {
                fake.requests.lock().unwrap().push(body.clone());
                ([("content-type", "text/event-stream")], answer(&body).await)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// The workspace is `dir`; the journal, and so the results folder, is in `data`: a folder of
/// its own, outside the workspace, as the daemon's data folder is.
fn start(
    url: &str,
    dir: &Path,
    data: &Path,
    resume: bool,
    harness: imperium_config::HarnessSettings,
) -> Session {
    start_in(url, Workspace::new(dir).unwrap(), data, resume, harness)
}

fn start_in(
    url: &str,
    workspace: Workspace,
    data: &Path,
    resume: bool,
    harness: imperium_config::HarnessSettings,
) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace,
        model: Some(ResolvedModel {
            spec: "fake:m".into(),
            endpoint: Endpoint {
                base_url: url.into(),
                api_key: None,
            },
            model: "m".into(),
            pricing: None,
            local: false,
            context_window: Some(128_000),
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
        resume,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 6,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness: imperium_config::HarnessSettings {
            dereference: imperium_config::HarnessMode::Off,
            state: imperium_config::HarnessMode::Off,
            hints: imperium_config::HarnessMode::Off,
            clarify: imperium_config::HarnessMode::Off,
            ..harness
        },
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
        journal_path: data.join("sessions/s.jsonl"),
    })
    .unwrap()
}

async fn turn(session: &Session, message: &str) {
    let mut events = session.subscribe();
    session.send(message.into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.recv())
            .await
            .expect("an event within 30 s")
            .unwrap();
        match event.event {
            SessionEvent::TurnCompleted { .. } if event.agent_id == "root" => return,
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            _ => {}
        }
    }
}

/// Every distinct tool message the root's (or a subagent's) model was sent, in order.
fn tool_messages(fake: &Fake, subagent: bool) -> Vec<String> {
    let requests = fake.requests.lock().unwrap();
    let mut seen: Vec<String> = Vec::new();
    for body in requests.iter().filter(|body| is_subagent(body) == subagent) {
        for message in body["messages"].as_array().unwrap() {
            if message["role"] == "tool" {
                let content = message["content"].as_str().unwrap_or_default().to_string();
                if !seen.contains(&content) {
                    seen.push(content);
                }
            }
        }
    }
    seen
}

fn write_files(dir: &Path) {
    let big: String = (1..=800)
        .map(|n| format!("needle line {n:04} {}\n", "x".repeat(60)))
        .collect();
    std::fs::write(dir.join("big.txt"), big).unwrap();
    std::fs::write(dir.join("small.txt"), "needle one\nneedle two\n").unwrap();
}

fn results_files(data: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(data.join("sessions/s.results"))
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_large_result_is_saved_with_a_preview_and_the_path_and_read_back_by_range() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write_files(dir.path());
    let session = start(&url, dir.path(), data.path(), false, Default::default());
    turn(&session, "search").await;

    let tools = tool_messages(&fake, false);
    assert_eq!(tools.len(), 2, "{tools:?}");
    // The conversation holds the head and the tail of the output, the path, and how to read it.
    let notice = &tools[0];
    assert!(notice.contains("Full output saved to "), "{notice}");
    assert!(notice.contains("read_file") && notice.contains("grep"));
    assert!(notice.contains("needle line 0001"), "{notice}");
    assert!(notice.contains("needle line 0800"), "{notice}");
    assert!(!notice.contains("needle line 0400"), "{notice}");
    assert!(notice.len() < 8_000, "{}", notice.len());
    // The file holds the whole result and sits in the session's results folder.
    let path = PathBuf::from(saved_path(notice));
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("needle line 0400") && saved.len() > 40_000);
    assert_eq!(results_files(data.path()).len(), 1);
    // The agent read a range of it with `read_file`, which is granted and not saved again.
    assert!(tools[1].contains("needle line 04"), "{}", tools[1]);
    assert!(!tools[1].contains("Full output saved to"), "{}", tools[1]);
    assert_eq!(results_files(data.path()).len(), 1);
}

#[tokio::test]
async fn a_result_under_the_threshold_stays_in_the_conversation() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write_files(dir.path());
    let session = start(&url, dir.path(), data.path(), false, Default::default());
    turn(&session, "search small").await;

    let tools = tool_messages(&fake, false);
    assert_eq!(tools.len(), 1, "{tools:?}");
    assert!(tools[0].contains("needle one") && tools[0].contains("needle two"));
    assert!(!tools[0].contains("saved to"), "{}", tools[0]);
    assert!(results_files(data.path()).is_empty());
}

#[tokio::test]
async fn the_setting_turns_saving_off() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write_files(dir.path());
    let harness = imperium_config::HarnessSettings {
        result_spill: false,
        ..Default::default()
    };
    let session = start(&url, dir.path(), data.path(), false, harness);
    turn(&session, "search").await;

    let tools = tool_messages(&fake, false);
    assert_eq!(tools.len(), 1, "{tools:?}");
    assert!(tools[0].contains("needle line 0400") && !tools[0].contains("saved to"));
    assert!(results_files(data.path()).is_empty());
}

#[tokio::test]
async fn a_subagents_large_result_is_saved_too() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write_files(dir.path());
    let session = start(&url, dir.path(), data.path(), false, Default::default());
    turn(&session, "delegate").await;

    let tools = tool_messages(&fake, true);
    assert!(
        tools
            .iter()
            .any(|tool| tool.contains("Full output saved to ")
                && tool.contains("needle line 0001")
                && !tool.contains("needle line 0400")),
        "{tools:?}"
    );
    assert_eq!(results_files(data.path()).len(), 1);
}

#[tokio::test]
async fn the_path_in_the_conversation_stays_valid_after_a_restart() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write_files(dir.path());
    let first = start(&url, dir.path(), data.path(), false, Default::default());
    turn(&first, "search").await;
    let path = PathBuf::from(saved_path(&tool_messages(&fake, false)[0]));
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let resumed = start(&url, dir.path(), data.path(), true, Default::default());
    turn(&resumed, "anything else?").await;
    // The model reads the earlier conversation with the same notice, and the file is there.
    let last = fake.requests.lock().unwrap().last().cloned().unwrap();
    let sent = last["messages"].to_string();
    assert!(sent.contains("Full output saved to "), "{sent}");
    assert!(path.is_file());
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("needle line 0400")
    );
    assert_eq!(results_files(data.path()).len(), 1, "no second copy");
}

#[tokio::test]
async fn the_system_prompt_is_the_same_with_saving_on_and_off() {
    let mut prompts = Vec::new();
    for on in [true, false] {
        let fake = Arc::new(Fake::default());
        let url = serve(Arc::clone(&fake)).await;
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let harness = imperium_config::HarnessSettings {
            result_spill: on,
            ..Default::default()
        };
        let session = start(&url, dir.path(), data.path(), false, harness);
        turn(&session, "search small").await;
        let first = fake.requests.lock().unwrap()[0].clone();
        prompts.push((first["messages"][0].clone(), first["tools"].clone()));
    }
    assert_eq!(prompts[0], prompts[1]);
}

#[tokio::test]
async fn a_session_limited_to_local_files_reads_its_results_back() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    write_files(dir.path());
    let locked =
        Workspace::new(dir.path())
            .unwrap()
            .with_profile(imperium_policy::AgentProfile::new(
                imperium_policy::ProfileRules {
                    local_files_only: true,
                    ..Default::default()
                },
            ));
    let session = start_in(&url, locked, data.path(), false, Default::default());
    turn(&session, "search").await;

    let tools = tool_messages(&fake, false);
    assert_eq!(tools.len(), 2, "{tools:?}");
    assert!(tools[0].contains("Full output saved to "), "{}", tools[0]);
    assert!(tools[1].contains("needle line 04"), "{}", tools[1]);
}
