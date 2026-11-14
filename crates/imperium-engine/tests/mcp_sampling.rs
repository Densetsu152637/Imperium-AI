//! An MCP server's sampling request during a tool call: refused unless the server's config
//! allows it, then always asked of the user, then answered by the session's model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_mcp::{Launch, McpTools, ServerSpec};
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

/// An MCP server whose `work` tool asks for a sampled reply first; it keeps the client's
/// answers to that request.
async fn mcp_server(replies: Arc<Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/mcp",
        post(move |axum::Json(request): axum::Json<Value>| {
            let replies = Arc::clone(&replies);
            async move {
                let Some(id) = request.get("id").cloned() else {
                    return axum::http::StatusCode::ACCEPTED.into_response();
                };
                if request.get("method").is_none() {
                    replies.lock().unwrap().push(request);
                    return axum::http::StatusCode::ACCEPTED.into_response();
                }
                let result = match request["method"].as_str() {
                    Some("initialize") => {
                        json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}})
                    }
                    Some("tools/list") => json!({"tools": [
                        {"name": "work", "description": "Needs a model", "inputSchema": {"type": "object"}}]}),
                    _ => {
                        let ask = json!({"jsonrpc": "2.0", "id": "s1",
                            "method": "sampling/createMessage", "params": {
                                "messages": [{"role": "user",
                                    "content": {"type": "text", "text": "name a colour"}}],
                                "maxTokens": 20}});
                        let done = json!({"jsonrpc": "2.0", "id": id,
                            "result": {"content": [{"type": "text", "text": "worked"}]}});
                        let body = format!(
                            "event: message\ndata: {ask}\n\nevent: message\ndata: {done}\n\n"
                        );
                        return ([("content-type", "text/event-stream")], body).into_response();
                    }
                };
                axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
            }
        }),
    );
    format!("{}/mcp", serve(app).await)
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// The session's model: calls `work`, writes "teal" when asked without tools (the sampled
/// request), and answers once it has the tool's result. `samples` counts the sampled requests.
async fn model(samples: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let samples = Arc::clone(&samples);
            async move {
                let tools = body["tools"].as_array().map_or(0, Vec::len);
                let has_result = body["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|message| message["role"] == "tool");
                let text = if tools == 0 {
                    samples.fetch_add(1, Ordering::SeqCst);
                    sse(json!({"choices":[{"delta":{"content":"teal"}}]}))
                } else if has_result {
                    sse(json!({"choices":[{"delta":{"content":"done"}}]}))
                } else {
                    sse(
                        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
                        "function":{"name":"mcp__w__work","arguments":"{}"}}]}}]}),
                    )
                };
                ([("content-type", "text/event-stream")], text)
            }
        }),
    );
    format!("{}/v1", serve(app).await)
}

struct Outcome {
    approvals: Vec<(String, String)>,
    tool_output: String,
    replies: Vec<Value>,
    samples: usize,
}

/// Runs one turn that calls `work`, answering any approval with `decision`.
async fn run(sampling: bool, decision: PermissionDecision) -> Outcome {
    let samples = Arc::new(AtomicUsize::new(0));
    let replies = Arc::new(Mutex::new(Vec::new()));
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "sampling".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: model(Arc::clone(&samples)).await,
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
        tools: vec![Arc::new(McpTools::new(vec![ServerSpec {
            name: "w".into(),
            launch: Launch::Http {
                url: mcp_server(Arc::clone(&replies)).await,
                headers: Vec::new(),
            },
            // The tool itself runs unasked, so only the sampling request can ask.
            allow: vec!["*".into()],
            timeout: Duration::from_secs(5),
            sampling,
        }]))],
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
        personality: None,
        max_steps: 5,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("use the w server".into()).unwrap();
    let mut approvals = Vec::new();
    let mut tool_output = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::PermissionRequested {
                request_id,
                tool,
                summary,
                ..
            } => {
                approvals.push((tool, summary));
                session.respond(request_id, decision).unwrap();
            }
            SessionEvent::ToolFinished { output, .. } => tool_output = output,
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    let replies = replies.lock().unwrap().clone();
    Outcome {
        approvals,
        tool_output,
        replies,
        samples: samples.load(Ordering::SeqCst),
    }
}

#[tokio::test]
async fn an_approved_request_is_written_by_the_session_model() {
    let outcome = run(true, PermissionDecision::Allow).await;
    assert_eq!(outcome.tool_output, "worked");
    assert_eq!(outcome.approvals.len(), 1);
    assert_eq!(outcome.approvals[0].0, "mcp_sampling");
    assert!(outcome.approvals[0].1.contains("MCP server w"));
    assert!(outcome.approvals[0].1.contains("name a colour"));
    assert_eq!(outcome.samples, 1);
    assert_eq!(outcome.replies.len(), 1);
    assert_eq!(outcome.replies[0]["result"]["content"]["text"], "teal");
    assert_eq!(outcome.replies[0]["result"]["model"], "fake:model");
}

#[tokio::test]
async fn a_declined_request_never_reaches_the_model() {
    let outcome = run(true, PermissionDecision::Deny).await;
    assert_eq!(outcome.approvals.len(), 1);
    assert_eq!(outcome.samples, 0);
    assert_eq!(outcome.replies[0]["error"]["code"], -1);
    assert_eq!(outcome.tool_output, "worked");
}

#[tokio::test]
async fn sampling_is_refused_without_asking_unless_the_server_is_configured() {
    let outcome = run(false, PermissionDecision::Allow).await;
    assert!(outcome.approvals.is_empty(), "{:?}", outcome.approvals);
    assert_eq!(outcome.samples, 0);
    assert_eq!(outcome.replies[0]["error"]["code"], -32601);
    assert_eq!(outcome.tool_output, "worked");
}
