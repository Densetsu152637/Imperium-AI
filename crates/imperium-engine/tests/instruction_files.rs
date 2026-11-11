//! Instruction files met while exploring: an agent whose file tools touch a nested directory
//! gets the `AGENTS.md` and `CLAUDE.md` files of that directory and its parents, once each,
//! outermost first. Nothing is loaded when the workflow is turned off.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_config::{Enforce, RoutingSettings};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;
use imperium_workflow::ContextIndex;
use serde_json::{Value, json};

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn call(id: usize, name: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("call{id}"),
        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
    )
}

/// A model that explores `pkg/deep`, reads a file there, lists the parent, then answers; it
/// keeps the last request it was sent.
async fn explorer(last: Arc<Mutex<Option<Value>>>) -> String {
    let steps = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let steps = Arc::clone(&steps);
            let last = Arc::clone(&last);
            async move {
                *last.lock().unwrap() = Some(body);
                let step = steps.fetch_add(1, Ordering::SeqCst);
                let body = match step {
                    0 => call(step, "list_dir", json!({"path": "pkg/deep"})),
                    1 => call(step, "read_file", json!({"path": "pkg/deep/notes.txt"})),
                    2 => call(step, "list_dir", json!({"path": "pkg"})),
                    _ => sse(json!({"choices":[{"delta":{"content":"done"}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn model(url: &str) -> ResolvedModel {
    ResolvedModel {
        spec: "cloud:root".into(),
        endpoint: Endpoint {
            base_url: url.into(),
            api_key: None,
        },
        model: "root".into(),
        pricing: None,
        local: false,
        context_window: None,
        kind: "openai".into(),
        reasoning: None,
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("pkg/deep")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "Project rules.\n").unwrap();
    std::fs::write(root.join("pkg/AGENTS.md"), "Package rules.\n").unwrap();
    std::fs::write(
        root.join("pkg/CLAUDE.md"),
        "@AGENTS.md\nPackage Claude rules.\n",
    )
    .unwrap();
    std::fs::write(root.join("pkg/deep/AGENTS.md"), "Deep rules.\n").unwrap();
    std::fs::write(root.join("pkg/deep/CLAUDE.md"), "Deep Claude rules.\n").unwrap();
    std::fs::write(root.join("pkg/deep/notes.txt"), "notes\n").unwrap();
    dir
}

/// Runs the explorer in `project` and returns the last request it saw and the references the
/// session reported loading.
async fn explore(project: &tempfile::TempDir, workflow: bool) -> (Value, Vec<Vec<String>>) {
    let last = Arc::new(Mutex::new(None));
    let url = explorer(Arc::clone(&last)).await;
    let context = workflow.then(|| Arc::new(ContextIndex::new(Vec::new(), project.path())));
    let session = Session::spawn(SessionConfig {
        id: "instructions".into(),
        workspace: Workspace::new(project.path()).unwrap(),
        model: Some(model(&url)),
        catalog: Vec::new(),
        clients: Default::default(),
        routing: RoutingSettings::default(),
        local: None,
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context,
        enforce: Enforce::Off,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 8,
        background: None,
        checkpoints: None,
        auto_compact: false,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: project.path().join(".journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("Look around pkg/deep".into()).unwrap();
    let mut loaded = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::ContextLoaded { references } => loaded.push(references),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    let request = last.lock().unwrap().take().unwrap();
    (request, loaded)
}

/// The text of every user message in `request`.
fn user_messages(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .filter_map(|message| message["content"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn exploring_a_nested_directory_loads_its_instruction_files_and_its_parents_once() {
    let dir = project();
    let (request, loaded) = explore(&dir, true).await;
    let users = user_messages(&request);
    let instructions: Vec<&String> = users
        .iter()
        .filter(|text| text.starts_with("<workflow-instructions>"))
        .collect();
    // One message for the first touch; the later reads and the parent listing add nothing.
    assert_eq!(instructions.len(), 1, "{users:#?}");
    let text = instructions[0];
    let position = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} missing from {text}"))
    };
    let order = [
        "pkg/AGENTS.md\"",
        "pkg/CLAUDE.md\"",
        "pkg/deep/AGENTS.md\"",
        "pkg/deep/CLAUDE.md\"",
    ]
    .map(position);
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "outermost first, nearest last: {order:?}"
    );
    for rule in ["Package rules.", "Deep rules.", "Deep Claude rules."] {
        assert_eq!(text.matches(rule).count(), 1, "{rule} once");
    }
    // The project folder's own files are the manifest's to serve, not served again here.
    assert!(!text.contains("Project rules."));
    // pkg/CLAUDE.md imports AGENTS.md, which was served just before it: not repeated.
    assert!(text.contains("Package Claude rules."));
    assert!(!text.contains("<imported-file"));
    // The journal records them, once each.
    let references: Vec<&String> = loaded.iter().flatten().collect();
    assert_eq!(references.len(), 4, "{references:?}");
    assert!(
        references
            .iter()
            .all(|reference| reference.starts_with("instructions:"))
    );
}

#[tokio::test]
async fn nothing_is_loaded_when_the_workflow_is_off() {
    let dir = project();
    let (request, loaded) = explore(&dir, false).await;
    assert!(
        user_messages(&request)
            .iter()
            .all(|text| !text.contains("<workflow-instructions>"))
    );
    assert!(loaded.is_empty());
}
