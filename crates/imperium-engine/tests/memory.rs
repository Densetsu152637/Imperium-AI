//! Phase 5 exit criterion: a memory saved in one project's session is recalled in a new
//! session of that project.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_config::{MemorySettings, MemoryWrite, WritePolicy};
use imperium_engine::{ResolvedModel, Session, SessionConfig, SessionMemory};
use imperium_memory::Store;
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// The first session's model saves a memory; every later call answers and records the last
/// user message it was sent.
async fn model(last_user: Arc<Mutex<String>>) -> String {
    let step = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let step = Arc::clone(&step);
            let last_user = Arc::clone(&last_user);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                if let Some(user) = messages.iter().rev().find(|m| m["role"] == "user") {
                    *last_user.lock().unwrap() = user["content"].as_str().unwrap_or_default().into();
                }
                let body = if step.fetch_add(1, Ordering::SeqCst) == 0 {
                    let arguments = json!({
                        "name": "release-checklist",
                        "description": "How releases are cut in this project",
                        "type": "project",
                        "scope": "project",
                        "body": "Releases are tagged vX.Y.Z from main after the CI gate passes."
                    });
                    sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"m",
                        "function":{"name":"memory_remember","arguments":arguments.to_string()}}]}}]}))
                } else {
                    sse(json!({"choices":[{"delta":{"content":"ok"}}]}))
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

fn start(id: &str, url: &str, data: &std::path::Path, project: &std::path::Path) -> Session {
    Session::spawn(SessionConfig {
        id: id.into(),
        workspace: Workspace::new(project).unwrap(),
        model: Some(ResolvedModel {
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
            store: Store::new(data, Some(project)),
            settings: MemorySettings {
                write: MemoryWrite {
                    project: WritePolicy::Ask,
                    ..MemoryWrite::default()
                },
                ..MemorySettings::default()
            },
        }),
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 5,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: data.join(format!("{id}.jsonl")),
    })
    .unwrap()
}

/// Runs one turn, approving any permission request, and returns its events.
async fn turn(session: &Session, text: &str) -> Vec<SessionEvent> {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if let SessionEvent::PermissionRequested { request_id, .. } = &event {
            session
                .respond(*request_id, PermissionDecision::Allow)
                .unwrap();
        }
        let done = matches!(event, SessionEvent::TurnCompleted { .. });
        if let SessionEvent::TurnFailed { message } = &event {
            panic!("{message}");
        }
        seen.push(event);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn plan_mode_refuses_to_save_a_memory() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let url = model(Arc::new(Mutex::new(String::new()))).await;
    let session = start("planned", &url, data.path(), project.path());
    session.set_permission_mode(imperium_api::PermissionMode::Plan);
    let events = turn(&session, "Remember how we cut releases.").await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionEvent::PermissionRequested { .. })),
        "plan mode refuses without asking"
    );
    assert!(events.iter().any(|e| matches!(e,
        SessionEvent::ToolFinished { ok: false, output, .. } if output.contains("plan mode"))));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionEvent::MemorySaved { .. }))
    );
    assert!(!project.path().join(".imperium/memory").exists());
}

#[tokio::test]
async fn a_memory_saved_in_one_session_is_recalled_in_the_next() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let last_user = Arc::new(Mutex::new(String::new()));
    let url = model(Arc::clone(&last_user)).await;

    let first = start("first", &url, data.path(), project.path());
    let events = turn(&first, "Remember how we cut releases.").await;
    assert!(events.iter().any(|e| matches!(e,
        SessionEvent::PermissionRequested { tool, .. } if tool == "memory_remember")));
    assert!(events.iter().any(|e| matches!(e,
        SessionEvent::MemorySaved { name, scope } if name == "release-checklist" && scope == "project")));
    assert!(
        project
            .path()
            .join(".imperium/memory/release-checklist.md")
            .is_file()
    );

    let second = start("second", &url, data.path(), project.path());
    let events = turn(&second, "Cut a release for the new version please").await;
    assert!(events.iter().any(|e| matches!(e,
        SessionEvent::MemoryUsed { names } if names == &["release-checklist"])));
    let sent = last_user.lock().unwrap().clone();
    assert!(sent.starts_with("<relevant-memories>"), "{sent}");
    assert!(sent.contains("tagged vX.Y.Z from main"), "{sent}");
    assert!(
        sent.ends_with("Cut a release for the new version please"),
        "{sent}"
    );
}
