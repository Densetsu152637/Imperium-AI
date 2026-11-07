//! The client against fake MCP servers: one over an in-memory stream, one over HTTP.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::HeaderMap;
use axum::routing::post;
use imperium_mcp::{Client, Launch, McpError, McpServers, ServerSpec};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A minimal MCP server: two pages of tools, `echo` returns its text, `fail` reports an error.
fn answer(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request["method"].as_str()?;
    let result = match method {
        "initialize" => json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                               "serverInfo": {"name": "fake", "version": "1"}}),
        "tools/list" if request["params"]["cursor"].is_null() => json!({
            "tools": [{"name": "echo", "description": "Echo text",
                       "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}],
            "nextCursor": "2"}),
        "tools/list" => json!({"tools": [{"name": "fail", "description": "Always fails"}]}),
        "tools/call" if request["params"]["name"] == "echo" => json!({
            "content": [{"type": "text", "text": request["params"]["arguments"]["text"]}]}),
        "tools/call" => json!({"content": [{"type": "text", "text": "it broke"}], "isError": true}),
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}}),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

#[tokio::test]
async fn the_stream_transport_matches_responses_and_answers_server_pings() {
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_side);
    let (server_read, mut server_write) = tokio::io::split(server_side);
    let pinged = Arc::new(AtomicUsize::new(0));
    let saw_ping_reply = Arc::clone(&pinged);
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        // Before answering anything, ask the client for a ping.
        server_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"s1\",\"method\":\"ping\"}\nnot json\n")
            .await
            .unwrap();
        while let Ok(Some(line)) = lines.next_line().await {
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == "s1" && message.get("result").is_some() {
                saw_ping_reply.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            if let Some(reply) = answer(&message) {
                let line = format!("{reply}\n");
                server_write.write_all(line.as_bytes()).await.unwrap();
            }
        }
    });
    let client = Client::over_stream(client_read, client_write, Duration::from_secs(5));
    let (a, b) = tokio::join!(
        client.request(
            "tools/call",
            json!({"name": "echo", "arguments": {"text": "one"}})
        ),
        client.request(
            "tools/call",
            json!({"name": "echo", "arguments": {"text": "two"}})
        ),
    );
    assert_eq!(a.unwrap()["content"][0]["text"], "one");
    assert_eq!(b.unwrap()["content"][0]["text"], "two");
    let missing = client.request("resources/list", json!({})).await;
    assert!(matches!(
        missing,
        Err(McpError::Server { code: -32601, .. })
    ));
    assert_eq!(pinged.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_silent_server_times_out() {
    let (client_side, _server_side) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(client_side);
    let client = Client::over_stream(read, write, Duration::from_millis(100));
    let result = client.request("initialize", json!({})).await;
    assert_eq!(result, Err(McpError::Timeout(0)));
}

/// Serves MCP over HTTP: JSON for most answers, SSE for tool calls, with a session id.
async fn http_server() -> String {
    let app = Router::new().route(
        "/mcp",
        post(
            |headers: HeaderMap, axum::Json(request): axum::Json<Value>| async move {
                let session = headers
                    .get("mcp-session-id")
                    .map(|v| v.to_str().unwrap().to_string());
                if request["method"] != "initialize" && session.as_deref() != Some("abc") {
                    return (
                        [("content-type", "text/plain")],
                        "missing session".to_string(),
                    )
                        .into_response_with(400);
                }
                let Some(reply) = answer(&request) else {
                    return ([("content-type", "application/json")], String::new())
                        .into_response_with(202);
                };
                if request["method"] == "tools/call" {
                    let progress =
                        json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {}});
                    let body = format!(
                        "event: message\ndata: {progress}\n\nevent: message\ndata: {reply}\n\n"
                    );
                    return ([("content-type", "text/event-stream")], body).into_response_with(200);
                }
                ([("content-type", "application/json")], reply.to_string()).into_response_with(200)
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

trait WithStatus {
    fn into_response_with(self, status: u16) -> axum::response::Response;
}

impl WithStatus for ([(&'static str, &'static str); 1], String) {
    fn into_response_with(self, status: u16) -> axum::response::Response {
        use axum::response::IntoResponse;
        let mut response = self.into_response();
        *response.status_mut() = axum::http::StatusCode::from_u16(status).unwrap();
        if status == 200 {
            response
                .headers_mut()
                .insert("mcp-session-id", "abc".parse().unwrap());
        }
        response
    }
}

#[tokio::test]
async fn http_servers_list_tools_across_pages_and_call_them() {
    let spec = ServerSpec {
        name: "fake".into(),
        launch: Launch::Http {
            url: http_server().await,
            headers: vec![("x-test".into(), "1".into())],
        },
        allow: vec!["echo".into()],
        timeout: Duration::from_secs(5),
    };
    let broken = ServerSpec {
        name: "broken".into(),
        launch: Launch::Stdio {
            command: "imperium-no-such-mcp-server".into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
        },
        allow: Vec::new(),
        timeout: Duration::from_secs(5),
    };
    let (servers, failed) = McpServers::connect(vec![spec, broken]).await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].server, "broken");
    let names: Vec<_> = servers
        .tools()
        .map(|t| (t.qualified.as_str(), t.allowed))
        .collect();
    assert_eq!(
        names,
        [("mcp__fake__echo", true), ("mcp__fake__fail", false)]
    );
    assert_eq!(
        servers.tool_specs()[1].parameters,
        json!({"type": "object"})
    );
    assert_eq!(
        servers.call("mcp__fake__echo", r#"{"text":"hi"}"#).await,
        Ok("hi".to_string())
    );
    assert_eq!(
        servers.call("mcp__fake__fail", "").await,
        Err("it broke".to_string())
    );
    assert!(servers.call("mcp__fake__nope", "{}").await.is_err());
}
