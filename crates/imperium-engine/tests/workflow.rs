//! Phase 5 exit criterion: agents on different models (a hosted one and a local one), given the
//! same role and task, are served and load the same workflow files. Also checks the
//! required-before-action gate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_config::{Enforce, Location, RoutingSettings, Tier};
use imperium_engine::{CatalogModel, ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{ContextItem, PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use imperium_workflow::{ContextIndex, Pack};
use serde_json::{Value, json};

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn call(name: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
    )
}

/// Required references listed in the manifest block of the latest user message.
fn required_refs(body: &Value) -> Vec<String> {
    let user = body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string();
    user.lines()
        .filter_map(|line| line.strip_prefix("- "))
        .filter_map(|line| line.split_once(" (required"))
        .map(|(reference, _)| reference.to_string())
        .collect()
}

/// A subagent model: tries to write first (refused while context is unloaded), then loads its
/// required context, writes, and reports.
async fn subagent_model(calls: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let calls = Arc::clone(&calls);
            async move {
                let tool_results = body["messages"]
                    .as_array()
                    .map_or(0, |m| m.iter().filter(|m| m["role"] == "tool").count());
                calls.fetch_add(1, Ordering::SeqCst);
                let write = json!({"path": "out.txt", "content": "x"});
                let body = match tool_results {
                    0 => call("write_file", write),
                    1 => call("context_load", json!({"references": required_refs(&body)})),
                    2 => call("write_file", write),
                    _ => sse(json!({"choices":[{"delta":{"content":"report"}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    serve(app).await
}

/// The root delegates the same brief with the same role to both models, then waits.
async fn root_model() -> String {
    let step = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let step = Arc::clone(&step);
            async move {
                let brief = |model: &str| {
                    json!({"brief": "Debug why the parser crashes", "role": "leaf", "model": model})
                };
                let body = match step.fetch_add(1, Ordering::SeqCst) {
                    0 => call("spawn_agent", brief("hosted")),
                    1 => call("spawn_agent", brief("local")),
                    2 => call("await_tasks", json!({})),
                    _ => sse(json!({"choices":[{"delta":{"content":"done"}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    serve(app).await
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn model(spec: &str, url: &str) -> ResolvedModel {
    ResolvedModel {
        spec: spec.into(),
        endpoint: Endpoint {
            base_url: url.into(),
            api_key: None,
        },
        model: spec.into(),
        pricing: None,
        local: false,
        context_window: None,
        kind: "openai".into(),
        reasoning: None,
    }
}

#[tokio::test]
async fn hosted_and_local_agents_with_the_same_role_and_task_load_the_same_files() {
    let pack = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(pack.path().join("workflows")).unwrap();
    std::fs::write(
        pack.path().join("AGENTS.md"),
        "- Bounded delegated execution: [Leaf](workflows/AGENTS.leaf.md).\n\
         - Incorrect behavior or incidents: [Debugging](workflows/AGENTS.debugging.md).\n\
         - Visual inspection: [Vision](workflows/AGENTS.vision.md).\n",
    )
    .unwrap();
    for file in ["AGENTS.leaf.md", "AGENTS.debugging.md", "AGENTS.vision.md"] {
        std::fs::write(
            pack.path().join("workflows").join(file),
            format!("# {file}\n"),
        )
        .unwrap();
    }
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("AGENTS.md"), "Project rules.\n").unwrap();

    let hosted_calls = Arc::new(AtomicUsize::new(0));
    let local_calls = Arc::new(AtomicUsize::new(0));
    let hosted_url = subagent_model(Arc::clone(&hosted_calls)).await;
    let local_url = subagent_model(Arc::clone(&local_calls)).await;
    let catalog = vec![
        CatalogModel {
            id: "hosted".into(),
            model: model("cloud:frontier", &hosted_url),
            tier: Tier::Frontier,
            location: Location::Cloud,
            tags: Vec::new(),
            named_only: false,
        },
        CatalogModel {
            id: "local".into(),
            model: model("llamacpp:small", &local_url),
            tier: Tier::Basic,
            location: Location::Local,
            tags: Vec::new(),
            named_only: false,
        },
    ];
    let context = ContextIndex::new(vec![Pack::load(pack.path()).unwrap()], project.path());
    let session = Session::spawn(SessionConfig {
        id: "wf".into(),
        workspace: Workspace::new(project.path()).unwrap(),
        model: Some(model("cloud:root", &root_model().await)),
        catalog,
        clients: Default::default(),
        routing: RoutingSettings::default(),
        local: None,
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context: Some(Arc::new(context)),
        enforce: Enforce::RequiredBeforeAction,
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
    session.send("Fix the parser crash".into()).unwrap();

    let manifests: Mutex<Vec<(String, String, Vec<ContextItem>)>> = Mutex::new(Vec::new());
    let mut loaded: Vec<(String, Vec<String>)> = Vec::new();
    let mut refused = 0;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        let agent = event.agent_id.clone();
        match event.event {
            SessionEvent::ContextManifest { role, items, .. } => {
                manifests.lock().unwrap().push((agent, role, items));
            }
            SessionEvent::ContextLoaded { references } => loaded.push((agent, references)),
            SessionEvent::ToolFinished {
                ok: false, output, ..
            } if output.contains("load the required context first") => {
                refused += 1;
            }
            SessionEvent::PermissionRequested { request_id, .. } => {
                session
                    .respond(request_id, PermissionDecision::Allow)
                    .unwrap();
            }
            SessionEvent::TurnCompleted { .. } if agent == "root" => break,
            SessionEvent::TurnFailed { message } => panic!("{agent}: {message}"),
            _ => {}
        }
    }

    let manifests = manifests.into_inner().unwrap();
    let root = manifests
        .iter()
        .find(|(agent, ..)| agent == "root")
        .unwrap();
    assert_eq!(root.1, "root");
    let subagents: Vec<_> = manifests
        .iter()
        .filter(|(agent, ..)| agent != "root")
        .collect();
    assert_eq!(subagents.len(), 2);
    assert_eq!(subagents[0].1, "leaf");
    assert_eq!(
        subagents[0].2, subagents[1].2,
        "same role and task, same manifest"
    );
    let refs: Vec<_> = subagents[0]
        .2
        .iter()
        .map(|item| item.reference.as_str())
        .collect();
    let pack_name = pack
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        refs,
        [
            "project:AGENTS.md".to_string(),
            format!("{pack_name}:AGENTS.md"),
            format!("{pack_name}:workflows/AGENTS.leaf.md"),
            format!("{pack_name}:workflows/AGENTS.debugging.md"),
        ]
    );
    // Both subagents loaded exactly those files, after being refused a write without them.
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].1, loaded[1].1);
    assert_eq!(loaded[0].1, refs);
    assert_eq!(refused, 2);
    assert!(hosted_calls.load(Ordering::SeqCst) >= 3);
    assert!(local_calls.load(Ordering::SeqCst) >= 3);
    assert!(project.path().join("out.txt").is_file());
}
