use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;

fn tool_call_body(name: &str, arguments: &str) -> String {
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":"call-1","function":{"name":name,"arguments":arguments}}]}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20}}"#
    )
}

fn text_body(text: &str) -> String {
    let chunk = serde_json::json!({"choices":[{"delta":{"content":text}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":10}}"#
    )
}

/// Every request the fake server received: whether a subagent sent it, and its body.
type Requests = Arc<Mutex<Vec<(bool, String)>>>;

/// A fake endpoint that tells the root agent and subagents apart by their system prompt, runs
/// the root through `delegate_chunks` and `await_tasks`, and records every request.
async fn endpoint(requests: Requests) -> Endpoint {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let requests = Arc::clone(&requests);
            let root_calls = Arc::clone(&root_calls);
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                let subagent = system.contains("subagent");
                requests.lock().unwrap().push((subagent, body.to_string()));
                let reply = if subagent {
                    text_body("chunk report")
                } else {
                    match root_calls.fetch_add(1, Ordering::SeqCst) {
                        0 => {
                            // The handle in the first request is the artefact to split.
                            let id = body.to_string();
                            let id = id
                                .split("stored as artefact ")
                                .nth(1)
                                .and_then(|rest| rest.split(' ').next())
                                .unwrap_or("missing")
                                .to_string();
                            let arguments = serde_json::json!({
                                "artefact_id": id,
                                "chunk_tokens": 100_000,
                                "brief": "list the rows you were given",
                                "reduce": "merge the lists"
                            });
                            tool_call_body("delegate_chunks", &arguments.to_string())
                        }
                        1 => tool_call_body("await_tasks", "{}"),
                        _ => text_body("The input is summarised."),
                    }
                };
                ([("content-type", "text/event-stream")], reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    Endpoint {
        base_url,
        api_key: None,
    }
}

fn start(endpoint: Endpoint, dir: &std::path::Path) -> Session {
    Session::spawn(SessionConfig {
        id: "test".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint,
            model: "model".into(),
            local: false,
            context_window: Some(128_000),
            kind: "openai".into(),
            reasoning: None,
            pricing: None,
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
        max_steps: 5,
        background: None,
        checkpoints: None,
        auto_compact: false,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".journal/test.jsonl"),
    })
    .unwrap()
}

/// About 300,000 tokens (four characters each) in paragraphs of ten rows.
fn large_input() -> String {
    let mut text = String::new();
    for row in 0..30_000 {
        text.push_str(&format!("NEEDLE row {row:06} lorem ipsum dolor sit\n"));
        if row % 10 == 9 {
            text.push('\n');
        }
    }
    text
}

#[tokio::test]
async fn delegate_chunks_keeps_a_300k_token_input_out_of_the_roots_context() {
    let requests: Requests = Arc::default();
    let endpoint = endpoint(Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();
    let input = large_input();
    assert!(input.len() / 4 >= 300_000);
    session.send(input).unwrap();

    let mut spawned = Vec::new();
    let mut started_output = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .expect("an event within 60 s")
            .unwrap();
        match event.event {
            SessionEvent::TaskSpawned { task_id, .. } => spawned.push(task_id),
            SessionEvent::ToolFinished { output, .. }
                if output.starts_with("started") && started_output.is_empty() =>
            {
                started_output = output;
            }
            SessionEvent::TurnCompleted { steps, .. } if event.agent_id == "root" => {
                assert_eq!(steps, 3);
                break;
            }
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            _ => {}
        }
    }

    let requests = requests.lock().unwrap();
    let root: Vec<_> = requests.iter().filter(|(sub, _)| !sub).collect();
    let chunk_agents: Vec<_> = requests
        .iter()
        .filter(|(sub, body)| *sub && body.contains("NEEDLE"))
        .collect();
    // The root made three model calls, and none of them carried the input beyond the handle's
    // outline (its first three lines, as the input has no headings).
    assert_eq!(root.len(), 3);
    assert!(
        root.iter()
            .all(|(_, body)| body.matches("NEEDLE").count() <= 3)
    );
    assert!(
        root.iter()
            .all(|(_, body)| !body.contains("NEEDLE row 000003"))
    );
    assert!(root.iter().all(|(_, body)| body.len() < 20_000));
    assert!(root[0].1.contains("stored as artefact a"));
    // Every chunk went to its own subagent, and together they read the whole input.
    assert!(chunk_agents.len() >= 3);
    for row in ["000000", "015000", "029999"] {
        assert!(
            chunk_agents
                .iter()
                .any(|(_, body)| body.contains(&format!("NEEDLE row {row}"))),
            "row {row} reached no subagent"
        );
    }
    // The reduce subagent saw the chunk reports, not the chunks.
    let reduce = requests
        .iter()
        .find(|(sub, body)| *sub && body.contains("merge the lists"))
        .expect("a reduce request");
    assert!(reduce.1.contains("chunk report") && !reduce.1.contains("NEEDLE"));
    // Chunk agents plus the reduce agent were spawned, and the root was told about them.
    assert_eq!(spawned.len(), chunk_agents.len() + 1);
    assert!(started_output.contains(&format!("t{}", spawned.len())));
}
