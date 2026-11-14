//! The Claude harness (`imperium mcp-serve`) driven over its MCP stdio protocol against a real
//! daemon whose model is a fake HTTP endpoint: orchestrating a task, smoke-testing a tool,
//! answering a permission request, and keeping the daemon's token out of every reply.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_cli::client::Client;
use imperium_cli::mcp_serve::serve;
use imperium_config::{Config, Layer, Paths, Source};
use imperium_core::{AccessToken, DaemonState};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperium_protocol::methods::Peer;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// A model that follows the script whose key the first user message contains: one tool call
/// per request, in order, then the reply "Done.".
async fn scripted_model(scripts: Vec<(&'static str, &'static str, Value)>) -> String {
    let scripts = Arc::new(scripts);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let scripts = Arc::clone(&scripts);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let done = messages.iter().filter(|m| m["role"] == "tool").count();
                let prompt = messages
                    .iter()
                    .rev()
                    .find(|m| m["role"] == "user")
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let call = scripts.iter().find(|(key, _, _)| prompt.contains(key));
                // One call per user message: only the first request after it makes the call.
                let after_last_user = messages
                    .iter()
                    .rev()
                    .take_while(|m| m["role"] != "user")
                    .filter(|m| m["role"] == "tool")
                    .count();
                let body = match call {
                    Some((_, name, arguments)) if after_last_user == 0 => {
                        sse(json!({"choices":[{"delta":{"tool_calls":[{
                            "index": 0, "id": format!("c{done}"),
                            "function": {"name": name, "arguments": arguments.to_string()}
                        }]}}]}))
                    }
                    _ => sse(json!({"choices":[{"delta":{"content":"Done."}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn start_daemon(daemon: Arc<DaemonState>) -> DaemonEndpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token = AccessToken::generate().unwrap();
    let endpoint = DaemonEndpoint {
        url: format!("ws://{}/rpc", listener.local_addr().unwrap()),
        token: token.as_str().to_string(),
        pid: std::process::id(),
        socket: None,
        protocol: PROTOCOL_VERSION,
    };
    tokio::spawn(imperium_core::serve(
        listener,
        token,
        daemon,
        std::future::pending(),
    ));
    endpoint
}

fn daemon_with(config: &str, data: &std::path::Path) -> Arc<DaemonState> {
    let settings = Config::from_layers([
        Layer::defaults(),
        Layer::new(Source::User("test".into()), config.parse().unwrap()),
    ])
    .settings()
    .unwrap();
    DaemonState::new(Paths::under(data), settings, None)
}

/// A harness connected to a daemon, spoken to as Claude Code would over stdio.
struct Harness {
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    next_id: u64,
    token: String,
}

impl Harness {
    async fn start(endpoint: &DaemonEndpoint) -> Harness {
        let client = Client::connect(
            endpoint,
            Peer {
                name: "imperium-cli".into(),
                version: "0".into(),
            },
        )
        .await
        .unwrap();
        let (mine, theirs) = tokio::io::duplex(1 << 20);
        let (theirs_read, theirs_write) = tokio::io::split(theirs);
        tokio::spawn(async move {
            serve(client, BufReader::new(theirs_read), theirs_write)
                .await
                .unwrap();
        });
        let (read, writer) = tokio::io::split(mine);
        Harness {
            writer,
            lines: BufReader::new(read).lines(),
            next_id: 1,
            token: endpoint.token.clone(),
        }
    }

    async fn send(&mut self, message: Value) {
        self.writer
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }

    /// One request and its reply (the message with that id).
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let line = tokio::time::timeout(Duration::from_secs(60), self.lines.next_line())
            .await
            .expect("a reply within 60 s")
            .unwrap()
            .expect("the harness is still running");
        assert!(
            !line.contains(&self.token),
            "the daemon token appeared in a reply"
        );
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        reply
    }

    /// A tool call: whether it failed, and its JSON result.
    async fn tool(&mut self, name: &str, arguments: Value) -> (bool, Value) {
        let reply = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"].as_bool().unwrap(),
            serde_json::from_str(text).unwrap(),
        )
    }

    async fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let (failed, value) = self.tool(name, arguments).await;
        assert!(!failed, "{name} failed: {value}");
        value
    }
}

const CONFIG: &str = "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{URL}\"\n\
    [permissions]\nmode = \"ask\"\n[workflow]\nenabled = false\n[memory]\nenabled = false\n";

async fn harness() -> (Harness, tempfile::TempDir) {
    let url = scripted_model(vec![
        ("read the notes", "read_file", json!({"path": "notes.txt"})),
        (
            "write the report",
            "write_file",
            json!({"path": "report.txt", "content": "written by the agent"}),
        ),
    ])
    .await;
    let data = tempfile::tempdir().unwrap();
    let endpoint = start_daemon(daemon_with(&CONFIG.replace("{URL}", &url), data.path())).await;
    let mut harness = Harness::start(&endpoint).await;
    let initialized = harness
        .request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        )
        .await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "imperium");
    harness
        .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    (harness, data)
}

#[tokio::test]
async fn it_lists_its_tools_and_the_daemons_environments_and_sessions() {
    let (mut harness, _data) = harness().await;

    let listed = harness.request("tools/list", json!({})).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for expected in [
        "list_environments",
        "list_sessions",
        "start_session",
        "send_message",
        "wait_for_turn",
        "get_tool_log",
        "get_transcript",
        "respond_permission",
        "stop_session",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }

    let pong = harness.request("ping", json!({})).await;
    assert!(pong["result"].is_object());
    let unknown = harness.request("nope/nope", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601);

    let environments = harness.ok("list_environments", json!({})).await;
    assert!(
        environments["environments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|env| env["id"] == "home")
    );
    let sessions = harness.ok("list_sessions", json!({})).await;
    assert_eq!(sessions["sessions"], json!([]));

    let (failed, error) = harness.tool("no_such_tool", json!({})).await;
    assert!(failed);
    assert!(error["error"].as_str().unwrap().contains("no_such_tool"));
}

#[tokio::test]
async fn a_task_is_handed_over_and_a_tool_smoke_tested() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.txt"), "remember the milk\n").unwrap();
    let (mut harness, _data) = harness().await;

    // Orchestrate: start a session on a project folder with a prompt and wait for the turn.
    let started = harness
        .ok(
            "start_session",
            json!({
                "project_dir": project.path().to_string_lossy(),
                "prompt": "please read the notes",
                "wait_timeout_s": 30,
            }),
        )
        .await;
    let session_id = started["session_id"].as_str().unwrap().to_string();
    assert_eq!(started["model"], "fake:m");
    let turn = &started["turn"];
    assert_eq!(turn["outcome"], "completed", "{turn}");
    assert_eq!(turn["assistant_text"], "Done.");

    // Smoke-test: the agent's read_file ran, and its result is in the tool log.
    let finished = turn["tool_log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["event"] == "tool_finished")
        .expect("a finished tool call");
    assert_eq!(finished["tool"], "read_file");
    assert_eq!(finished["ok"], true);
    assert!(
        finished["output"]
            .as_str()
            .unwrap()
            .contains("remember the milk")
    );
    let log = harness
        .ok("get_tool_log", json!({"session_id": session_id}))
        .await;
    assert!(
        log["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == "tool_started" && entry["tool"] == "read_file")
    );

    let transcript = harness
        .ok("get_transcript", json!({"session_id": session_id}))
        .await;
    assert!(
        transcript["content"]
            .as_str()
            .unwrap()
            .contains("please read the notes")
    );

    let listed = harness.ok("list_sessions", json!({})).await;
    assert_eq!(listed["sessions"][0]["session_id"], session_id.as_str());
    assert_eq!(listed["sessions"][0]["status"], "idle");
    let state = harness
        .ok("get_session", json!({"session_id": session_id}))
        .await;
    assert_eq!(state["latest_turn"], "completed");

    // A second message, and the wait for its turn only.
    let sent = harness
        .ok(
            "send_message",
            json!({"session_id": session_id, "text": "thanks"}),
        )
        .await;
    let waited = harness
        .ok(
            "wait_for_turn",
            json!({"session_id": session_id, "since": sent["cursor"], "timeout_s": 30}),
        )
        .await;
    assert_eq!(waited["outcome"], "completed");
}

#[tokio::test]
async fn a_permission_request_waits_for_an_explicit_answer() {
    let project = tempfile::tempdir().unwrap();
    let (mut harness, _data) = harness().await;
    let report = project.path().join("report.txt");

    let started = harness
        .ok(
            "start_session",
            json!({
                "project_dir": project.path().to_string_lossy(),
                "prompt": "please write the report",
            }),
        )
        .await;
    let session_id = started["session_id"].as_str().unwrap().to_string();
    let waited = harness
        .ok(
            "wait_for_turn",
            json!({"session_id": session_id, "since": started["cursor"], "timeout_s": 30}),
        )
        .await;
    assert_eq!(waited["outcome"], "permission_required", "{waited}");
    let request = &waited["pending_permissions"][0];
    assert_eq!(request["tool"], "write_file");
    assert!(!report.exists(), "nothing is written before an answer");

    // Nothing answers it on its own: a short wait still finds it pending.
    let still = harness
        .ok("get_session", json!({"session_id": session_id}))
        .await;
    assert_eq!(
        still["pending_permissions"][0]["request_id"],
        request["request_id"]
    );

    // A decision must be one of the three; a bad one is refused before the daemon is asked.
    let (failed, _) = harness
        .tool(
            "respond_permission",
            json!({"session_id": session_id, "request_id": request["request_id"], "decision": "yes"}),
        )
        .await;
    assert!(failed);

    // Denied: the agent carries on and the file is never written.
    harness
        .ok(
            "respond_permission",
            json!({"session_id": session_id, "request_id": request["request_id"], "decision": "deny"}),
        )
        .await;
    let waited = harness
        .ok(
            "wait_for_turn",
            json!({"session_id": session_id, "since": started["cursor"], "timeout_s": 30}),
        )
        .await;
    assert_eq!(waited["outcome"], "completed", "{waited}");
    assert!(!report.exists());

    // Allowed: the same request again, answered the other way.
    let sent = harness
        .ok(
            "send_message",
            json!({"session_id": session_id, "text": "please write the report again"}),
        )
        .await;
    let waited = harness
        .ok(
            "wait_for_turn",
            json!({"session_id": session_id, "since": sent["cursor"], "timeout_s": 30}),
        )
        .await;
    assert_eq!(waited["outcome"], "permission_required", "{waited}");
    harness
        .ok(
            "respond_permission",
            json!({
                "session_id": session_id,
                "request_id": waited["pending_permissions"][0]["request_id"],
                "decision": "allow",
            }),
        )
        .await;
    let waited = harness
        .ok(
            "wait_for_turn",
            json!({"session_id": session_id, "since": sent["cursor"], "timeout_s": 30}),
        )
        .await;
    assert_eq!(waited["outcome"], "completed", "{waited}");
    assert_eq!(
        std::fs::read_to_string(&report).unwrap(),
        "written by the agent"
    );

    let stopped = harness
        .ok("stop_session", json!({"session_id": session_id}))
        .await;
    assert_eq!(stopped["stopped"], true);
}

#[tokio::test]
async fn an_unfinished_turn_times_out_without_failing() {
    let project = tempfile::tempdir().unwrap();
    let (mut harness, _data) = harness().await;
    // A session that was never sent anything has no turn to wait for: the wait times out.
    let idle = harness
        .ok(
            "start_session",
            json!({"project_dir": project.path().to_string_lossy()}),
        )
        .await;
    let waited = harness
        .ok(
            "wait_for_turn",
            json!({"session_id": idle["session_id"], "timeout_s": 1}),
        )
        .await;
    assert_eq!(waited["outcome"], "timeout");
    assert_eq!(waited["status"], "idle");
    let (failed, error) = harness
        .tool("wait_for_turn", json!({"session_id": "no-such-session"}))
        .await;
    assert!(failed, "{error}");
}
