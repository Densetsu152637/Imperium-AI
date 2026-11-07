use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{ResolvedModel, Session, SessionConfig};
use imperium_models::{Endpoint, Pricing};
use imperium_protocol::session::{AgentEvent, PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use tokio::sync::broadcast;

/// A fake endpoint that replays one scripted SSE body per request.
async fn scripted_endpoint(bodies: Vec<String>) -> Endpoint {
    let calls = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(bodies);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            let body = bodies[index.min(bodies.len() - 1)].clone();
            async move { ([("content-type", "text/event-stream")], body) }
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

async fn next(events: &mut broadcast::Receiver<AgentEvent>) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("an event within 10 s")
        .unwrap()
        .event
}

fn start(endpoint: Endpoint, dir: &std::path::Path) -> Session {
    Session::spawn(SessionConfig {
        id: "test".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: ResolvedModel {
            spec: "fake:model".into(),
            endpoint,
            model: "model".into(),
            pricing: Some(Pricing {
                input_usd_per_mtok: 1.0,
                output_usd_per_mtok: 2.0,
                cached_input_usd_per_mtok: None,
            }),
        },
        catalog: Vec::new(),
        max_steps: 5,
        journal_path: dir.join(".journal/test.jsonl"),
    })
    .unwrap()
}

#[tokio::test]
async fn an_approved_write_changes_the_file_and_the_turn_reports_cost() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("write_file", r#"{"path":"hello.txt","content":"hi"}"#),
        text_body("Created hello.txt."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();
    session.send("create hello.txt".into()).unwrap();

    assert!(matches!(
        next(&mut events).await,
        SessionEvent::TurnStarted { .. }
    ));
    let SessionEvent::ModelCallCompleted {
        usage,
        cost_usd,
        tokens_per_sec,
        ..
    } = next(&mut events).await
    else {
        panic!("expected per-call telemetry");
    };
    assert_eq!(usage.prompt_tokens, 100);
    // 100 × $1 + 20 × $2, per million tokens.
    assert!((cost_usd.unwrap() - 0.00014).abs() < 1e-12);
    assert!(tokens_per_sec.is_some());
    let SessionEvent::PermissionRequested {
        request_id, tool, ..
    } = next(&mut events).await
    else {
        panic!("expected a permission request");
    };
    assert_eq!(tool, "write_file");
    session
        .respond(request_id, PermissionDecision::Allow)
        .unwrap();

    assert!(matches!(
        next(&mut events).await,
        SessionEvent::ToolStarted { .. }
    ));
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::ToolFinished { ok: true, .. }
    ));
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::TextDelta { .. }
    ));
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::ModelCallCompleted { .. }
    ));
    let SessionEvent::TurnCompleted {
        usage,
        cost_usd,
        steps,
    } = next(&mut events).await
    else {
        panic!("expected the turn to complete");
    };
    assert_eq!(steps, 2);
    assert_eq!(usage.prompt_tokens, 250);
    assert_eq!(usage.completion_tokens, 30);
    // 250 × $1 + 30 × $2, per million tokens.
    assert!((cost_usd.unwrap() - 0.00031).abs() < 1e-12);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
        "hi"
    );
    let journal = std::fs::read_to_string(dir.path().join(".journal/test.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 8);
    let (history, _) = session.subscribe_with_history();
    assert_eq!(history.len(), 8);
    assert!(matches!(history[0].event, SessionEvent::TurnStarted { .. }));
    assert!(history.iter().all(|event| event.agent_id == "root"));
    assert!(journal.contains(r#""type":"turn_completed""#));
}

#[tokio::test]
async fn a_denied_command_does_not_run() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body(
            "run_command",
            r#"{"command":"echo should-not-run > ran.txt"}"#,
        ),
        text_body("Understood."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();
    session.send("run it".into()).unwrap();

    next(&mut events).await;
    next(&mut events).await;
    let SessionEvent::PermissionRequested { request_id, .. } = next(&mut events).await else {
        panic!("expected a permission request");
    };
    session
        .respond(request_id, PermissionDecision::Deny)
        .unwrap();
    let SessionEvent::ToolFinished { ok, output, .. } = next(&mut events).await else {
        panic!("expected the denied tool to finish");
    };
    assert!(!ok);
    assert!(output.contains("denied"));
    assert!(!dir.path().join("ran.txt").exists());
}

/// A fake endpoint that tells the root agent and subagents apart by their system prompt.
async fn delegating_endpoint(
    root_calls: Arc<AtomicUsize>,
    sub_calls: Arc<AtomicUsize>,
) -> Endpoint {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let root_calls = Arc::clone(&root_calls);
            let sub_calls = Arc::clone(&sub_calls);
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                let body = if system.contains("subagent") {
                    let n = sub_calls.fetch_add(1, Ordering::SeqCst);
                    text_body(&format!("report {n}"))
                } else {
                    match root_calls.fetch_add(1, Ordering::SeqCst) {
                        0 => {
                            let calls = serde_json::json!({"choices":[{"delta":{"tool_calls":[
                                {"index":0,"id":"a","function":{"name":"spawn_agent","arguments":"{\"brief\":\"check docs\"}"}},
                                {"index":1,"id":"b","function":{"name":"spawn_agent","arguments":"{\"brief\":\"check tests\",\"tier\":\"basic\"}"}}
                            ]}}]});
                            format!("data: {calls}

data: [DONE]

")
                        }
                        1 => tool_call_body("await_tasks", "{}"),
                        _ => text_body("Both checks are done."),
                    }
                };
                ([("content-type", "text/event-stream")], body)
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

#[tokio::test]
async fn the_root_delegates_waits_without_model_calls_and_collects_reports() {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let sub_calls = Arc::new(AtomicUsize::new(0));
    let endpoint = delegating_endpoint(Arc::clone(&root_calls), Arc::clone(&sub_calls)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();
    session.send("check docs and tests".into()).unwrap();

    let mut spawned = Vec::new();
    let mut finished = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::TaskSpawned { task_id, .. } => spawned.push(task_id),
            SessionEvent::TaskFinished { task_id, ok, .. } => {
                assert!(ok);
                assert_eq!(event.agent_id, task_id);
                finished.push(task_id);
            }
            SessionEvent::TurnCompleted { steps, .. } if event.agent_id == "root" => {
                assert_eq!(steps, 3);
                break;
            }
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            _ => {}
        }
    }
    assert_eq!(spawned, ["t1", "t2"]);
    finished.sort();
    assert_eq!(finished, ["t1", "t2"]);
    // Spawn, await, and summary: waiting for the subagents cost the root no model calls.
    assert_eq!(root_calls.load(Ordering::SeqCst), 3);
    assert_eq!(sub_calls.load(Ordering::SeqCst), 2);
}
