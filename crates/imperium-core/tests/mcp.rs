//! An agent calls an MCP server's tools: an allowed tool runs without asking, another asks
//! first, and a server that cannot start is reported as a notice.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{ResolvedModel, Session, SessionConfig};
use imperium_mcp::{Launch, ServerSpec};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// An MCP server with `lookup` (allowed in the test) and `delete` (needs approval).
async fn mcp_server(calls: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/mcp",
        post(move |axum::Json(request): axum::Json<Value>| {
            let calls = Arc::clone(&calls);
            async move {
                let Some(id) = request.get("id").cloned() else {
                    return axum::Json(json!({}));
                };
                let result = match request["method"].as_str() {
                    Some("initialize") => json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
                    Some("tools/list") => json!({"tools": [
                        {"name": "lookup", "description": "Look up a word", "inputSchema": {"type": "object"}},
                        {"name": "delete", "description": "Delete a word", "inputSchema": {"type": "object"}}]}),
                    _ => {
                        calls.fetch_add(1, Ordering::SeqCst);
                        let name = request["params"]["name"].as_str().unwrap_or_default();
                        json!({"content": [{"type": "text", "text": format!("{name} ok")}]})
                    }
                };
                axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            }
        }),
    );
    format!("{}/mcp", serve(app).await)
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// A model that calls lookup, then delete, then answers; it records the tools it was offered.
async fn model(offered: Arc<std::sync::Mutex<Vec<String>>>) -> String {
    let step = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let step = Arc::clone(&step);
            let offered = Arc::clone(&offered);
            async move {
                let names = body["tools"].as_array().into_iter().flatten();
                *offered.lock().unwrap() = names
                    .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                    .collect();
                let call = |name: &str| {
                    sse(
                        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
                        "function":{"name":name,"arguments":"{\"word\":\"x\"}"}}]}}]}),
                    )
                };
                let body = match step.fetch_add(1, Ordering::SeqCst) {
                    0 => call("mcp__words__lookup"),
                    1 => call("mcp__words__delete"),
                    _ => sse(json!({"choices":[{"delta":{"content":"done"}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    format!("{}/v1", serve(app).await)
}

#[tokio::test]
async fn agents_call_mcp_tools_with_approval_rules() {
    let mcp_calls = Arc::new(AtomicUsize::new(0));
    let offered = Arc::new(std::sync::Mutex::new(Vec::new()));
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "mcp".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: model(Arc::clone(&offered)).await,
                api_key: None,
            },
            model: "model".into(),
            pricing: None,
            local: false,
        },
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        local: None,
        mcp: vec![
            ServerSpec {
                name: "words".into(),
                launch: Launch::Http {
                    url: mcp_server(Arc::clone(&mcp_calls)).await,
                    headers: Vec::new(),
                },
                allow: vec!["lookup".into()],
                timeout: Duration::from_secs(5),
            },
            ServerSpec {
                name: "missing".into(),
                launch: Launch::Stdio {
                    command: "imperium-no-such-server".into(),
                    args: Vec::new(),
                    env: Vec::new(),
                    cwd: None,
                },
                allow: Vec::new(),
                timeout: Duration::from_secs(5),
            },
        ],
        notices: vec!["hello from the daemon".into()],
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        instructions: None,
        max_steps: 5,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("use the words server".into()).unwrap();
    let mut notices = Vec::new();
    let mut finished = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::Notice { message } => notices.push(message),
            SessionEvent::PermissionRequested {
                request_id, tool, ..
            } => {
                assert_eq!(tool, "mcp__words__delete");
                session
                    .respond(request_id, PermissionDecision::Allow)
                    .unwrap();
            }
            SessionEvent::ToolFinished { ok, output, .. } => finished.push((ok, output)),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0], "hello from the daemon");
    assert!(notices[1].contains("missing"), "{}", notices[1]);
    assert_eq!(
        finished,
        [
            (true, "lookup ok".to_string()),
            (true, "delete ok".to_string())
        ]
    );
    assert_eq!(mcp_calls.load(Ordering::SeqCst), 2);
    let offered = offered.lock().unwrap().clone();
    assert!(
        offered.contains(&"mcp__words__lookup".to_string()),
        "{offered:?}"
    );
}
