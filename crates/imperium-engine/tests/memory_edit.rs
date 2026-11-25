//! `memory_edit` and `memory_forget` change saved memories in place under the same write policy
//! and plan-mode rule as `memory_remember`, and nothing they remove is erased.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_config::{MemorySettings, MemoryWrite, WritePolicy};
use imperium_engine::{ResolvedModel, Session, SessionConfig, SessionMemory};
use imperium_memory::environment::EnvironmentMemory;
use imperium_memory::{NewEntry, Scope, Store};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// A model that makes the given tool calls one per step, then answers; keeps every request.
async fn model(steps: Vec<(&'static str, Value)>, requests: Arc<Mutex<Vec<Value>>>) -> String {
    let steps = Arc::new(steps);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let steps = Arc::clone(&steps);
            let requests = Arc::clone(&requests);
            async move {
                let done = body["messages"]
                    .as_array()
                    .map_or(0, |m| m.iter().filter(|m| m["role"] == "tool").count());
                requests.lock().unwrap().push(body);
                let reply = match steps.get(done) {
                    Some((name, arguments)) => sse(json!({"choices":[{"delta":{"tool_calls":
                        [{"index":0,"id":format!("call-{done}"),
                          "function":{"name":name,"arguments":arguments.to_string()}}]}}]})),
                    None => sse(json!({"choices":[{"delta":{"content":"ok"}}]})),
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
    environment: Option<&std::path::Path>,
    policy: WritePolicy,
) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace: Workspace::new(dir).unwrap(),
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
            store: Store::new(&dir.join("data")),
            settings: MemorySettings {
                write: MemoryWrite {
                    global: policy,
                    environment: policy,
                },
                ..MemorySettings::default()
            },
            environment: environment.map(|file| EnvironmentMemory::new(file.to_path_buf())),
        }),
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 8,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness: Default::default(),
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
        journal_path: dir.join(".sessions/s.jsonl"),
    })
    .unwrap()
}

/// Runs one turn, answering permission requests with `decision`.
async fn turn(session: &Session, decision: PermissionDecision) -> Vec<SessionEvent> {
    let mut events = session.subscribe();
    session.send("Tidy the memory.".into()).unwrap();
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if let SessionEvent::PermissionRequested { request_id, .. } = &event {
            session.respond(*request_id, decision).unwrap();
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

fn save(data: &std::path::Path, name: &str, body: &str) {
    Store::new(&data.join("data"))
        .save(NewEntry {
            name: name.into(),
            description: "Tool choices".into(),
            kind: "feedback".into(),
            scope: Scope::Global,
            body: body.into(),
            source: "test".into(),
        })
        .unwrap();
}

fn finished(events: &[SessionEvent], tool: &str) -> Vec<(bool, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolFinished {
                tool: Some(name),
                ok,
                output,
                ..
            } if name == tool => Some((*ok, output.clone())),
            _ => None,
        })
        .collect()
}

fn global(dir: &std::path::Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(format!("data/memory/global/{name}.md"))).unwrap_or_default()
}

#[tokio::test]
async fn memory_edit_replaces_a_unique_string_and_refuses_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), "tools", "Use pnpm.\nUse vitest.");
    let steps = vec![
        (
            "memory_edit",
            json!({"name": "tools", "old_string": "Use ", "new_string": "Run "}),
        ),
        (
            "memory_edit",
            json!({"name": "tools", "old_string": "yarn", "new_string": "bun"}),
        ),
        (
            "memory_edit",
            json!({"name": "tools", "old_string": "pnpm", "new_string": "bun"}),
        ),
    ];
    let url = model(steps, Default::default()).await;
    let session = start(&url, dir.path(), None, WritePolicy::Auto);
    let events = turn(&session, PermissionDecision::Deny).await;
    let results = finished(&events, "memory_edit");
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(
        !results[0].0 && results[0].1.contains("2 times"),
        "{results:?}"
    );
    assert!(
        !results[1].0 && results[1].1.contains("not found"),
        "{results:?}"
    );
    assert!(results[2].0, "{results:?}");
    let text = global(dir.path(), "tools");
    assert!(text.contains("Use bun.\nUse vitest."), "{text}");
    assert!(text.contains("description: Tool choices"), "{text}");
    assert!(events.iter().any(|e| matches!(e,
        SessionEvent::MemorySaved { name, scope } if name == "tools" && scope == "global")));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionEvent::PermissionRequested { .. }))
    );
}

#[tokio::test]
async fn memory_forget_moves_the_entry_aside_and_updates_the_index() {
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), "old-fact", "Obsolete.");
    save(dir.path(), "keeper", "Still true.");
    let url = model(
        vec![("memory_forget", json!({"name": "old-fact"}))],
        Default::default(),
    )
    .await;
    let session = start(&url, dir.path(), None, WritePolicy::Auto);
    let events = turn(&session, PermissionDecision::Deny).await;
    assert!(finished(&events, "memory_forget")[0].0);
    assert!(events.iter().any(|e| matches!(e,
        SessionEvent::Notice { message } if message.contains("Forgot global memory old-fact"))));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionEvent::MemorySaved { .. }))
    );
    assert!(!dir.path().join("data/memory/global/old-fact.md").exists());
    let kept: Vec<_> = std::fs::read_dir(dir.path().join("data/memory/trash"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(kept.len(), 1);
    assert!(
        std::fs::read_to_string(kept[0].path())
            .unwrap()
            .contains("Obsolete.")
    );
    let index = std::fs::read_to_string(dir.path().join("data/memory/global/INDEX.md")).unwrap();
    assert!(
        index.contains("keeper") && !index.contains("old-fact"),
        "{index}"
    );
}

#[tokio::test]
async fn ask_policy_asks_first_and_a_refusal_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), "tools", "Use pnpm.");
    let steps = vec![
        (
            "memory_edit",
            json!({"name": "tools", "old_string": "pnpm", "new_string": "bun"}),
        ),
        ("memory_forget", json!({"name": "tools"})),
    ];
    let url = model(steps, Default::default()).await;
    let session = start(&url, dir.path(), None, WritePolicy::Ask);
    let events = turn(&session, PermissionDecision::Deny).await;
    let asked: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::PermissionRequested { tool, .. } => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(asked, ["memory_edit", "memory_forget"]);
    assert!(finished(&events, "memory_edit")[0].1.contains("declined"));
    assert!(finished(&events, "memory_forget")[0].1.contains("declined"));
    assert!(global(dir.path(), "tools").contains("Use pnpm."));
    assert!(!dir.path().join("data/memory/trash").exists());

    // Approved, they go through.
    let session = start(&url, dir.path(), None, WritePolicy::Ask);
    let events = turn(&session, PermissionDecision::Allow).await;
    assert!(finished(&events, "memory_edit")[0].0);
    assert!(finished(&events, "memory_forget")[0].0);
    assert!(!dir.path().join("data/memory/global/tools.md").exists());
}

#[tokio::test]
async fn off_policy_and_plan_mode_refuse() {
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), "tools", "Use pnpm.");
    let steps = vec![
        (
            "memory_edit",
            json!({"name": "tools", "old_string": "pnpm", "new_string": "bun"}),
        ),
        ("memory_forget", json!({"name": "tools"})),
    ];
    let url = model(steps, Default::default()).await;
    let session = start(&url, dir.path(), None, WritePolicy::Off);
    let events = turn(&session, PermissionDecision::Allow).await;
    for tool in ["memory_edit", "memory_forget"] {
        let result = &finished(&events, tool)[0];
        assert!(!result.0 && result.1.contains("turned off"), "{result:?}");
    }
    assert!(global(dir.path(), "tools").contains("Use pnpm."));

    let session = start(&url, dir.path(), None, WritePolicy::Auto);
    session.set_permission_mode(imperium_api::PermissionMode::Plan);
    let events = turn(&session, PermissionDecision::Allow).await;
    for tool in ["memory_edit", "memory_forget"] {
        let result = &finished(&events, tool)[0];
        assert!(!result.0 && result.1.contains("plan mode"), "{result:?}");
    }
    assert!(global(dir.path(), "tools").contains("Use pnpm."));
}

#[tokio::test]
async fn environment_memory_is_edited_and_forgotten_keeping_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("env/Memory.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "# Notes\n\nhand written\n\n## db\n_Staging database (project)_\n\nReset every Monday.\n\n\
         ## ci\n_CI notes (project)_\n\nRuns on Linux.\n",
    )
    .unwrap();
    let steps = vec![
        (
            "memory_edit",
            json!({"name": "db", "old_string": "Monday", "new_string": "Sunday"}),
        ),
        (
            "memory_forget",
            json!({"name": "ci", "scope": "environment"}),
        ),
    ];
    let requests = Arc::new(Mutex::new(Vec::new()));
    let url = model(steps, Arc::clone(&requests)).await;
    let session = start(&url, dir.path(), Some(&file), WritePolicy::Auto);
    let events = turn(&session, PermissionDecision::Deny).await;
    assert!(finished(&events, "memory_edit")[0].0);
    assert!(finished(&events, "memory_forget")[0].0);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("hand written"), "{text}");
    assert!(text.contains("_Staging database (project)_"), "{text}");
    assert!(text.contains("Reset every Sunday."), "{text}");
    assert!(!text.contains("## ci"), "{text}");
    let archive = std::fs::read_to_string(dir.path().join("env/Memory.archive.md")).unwrap();
    assert!(
        archive.contains("## ci") && archive.contains("Runs on Linux."),
        "{archive}"
    );

    // The root is offered both tools.
    let first = requests.lock().unwrap()[0].clone();
    let names: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    for tool in ["memory_edit", "memory_forget"] {
        assert!(names.contains(&tool), "{names:?}");
    }
}
