//! `session.tools`, `session.run_tool` and `session.tool_result`: the daemon side of the MCP
//! server bridge. A call runs inside the session through the checks a model's own call goes
//! through, so a write asks the user and a tool the bridge does not run is never started. No model is called (nothing listens on the
//! model endpoint).

use std::sync::Arc;
use std::time::Duration;

use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};

fn daemon(home: &std::path::Path, mode: &str, base_url: &str) -> Arc<DaemonState> {
    let config = format!(
        r#"
[models]
default = "a:m"

[endpoints.a]
base_url = "{base_url}"

[permissions]
mode = "{mode}"

[workflow]
enabled = false

[memory]
enabled = false
"#
    );
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    DaemonState::new(imperium_config::Paths::under(home), settings, None)
}

/// Nothing listens here; no model call is made.
const NO_MODEL: &str = "http://127.0.0.1:9/v1";

/// A model that answers after five seconds, so a turn stays running meanwhile.
async fn slow_model() -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            (
                [("content-type", "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"Done.\"}}]}\n\ndata: [DONE]\n\n",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn call(connection: &mut Connection, method: &str, params: Value) -> Result<Value, i32> {
    let frame = json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response: Response = match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(30), deferred)
            .await
            .expect("a reply within 30 s"),
        (None, None) => panic!("no reply"),
    };
    match response.outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => {
            eprintln!("{method} failed with {}: {}", error.code, error.message);
            Err(error.code)
        }
    }
}

fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    let handshake = json!({"jsonrpc": "2.0", "id": 1, "method": "system.handshake",
        "params": {"client": {"name": "test", "version": "0"},
                   "protocol": {"major": 0, "minor": 41}}});
    let _ = connection.handle_text(&handshake.to_string());
    connection
}

/// Starts a bridged call and waits for its result.
async fn run(
    connection: &mut Connection,
    session_id: &str,
    tool: &str,
    arguments: Value,
    allow_write: bool,
) -> Result<Value, i32> {
    let started = call(
        connection,
        "session.run_tool",
        json!({"session_id": session_id, "tool": tool, "arguments": arguments,
               "allow_write": allow_write}),
    )
    .await?;
    let call_id = started["call_id"].as_str().unwrap().to_string();
    collect(connection, session_id, &call_id).await
}

async fn collect(
    connection: &mut Connection,
    session_id: &str,
    call_id: &str,
) -> Result<Value, i32> {
    for _ in 0..40 {
        let result = call(
            connection,
            "session.tool_result",
            json!({"session_id": session_id, "call_id": call_id, "wait_ms": 1000}),
        )
        .await?;
        if result["done"] == true {
            return Ok(result);
        }
    }
    panic!("the call did not finish");
}

#[tokio::test]
async fn it_lists_the_tools_the_bridge_runs_read_tools_first() {
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "ask", NO_MODEL);
    let mut connection = connect(&state);
    let listed = call(&mut connection, "session.tools", json!({}))
        .await
        .unwrap();
    let tools = listed["tools"].as_array().unwrap();
    let find = |name: &str| tools.iter().find(|tool| tool["name"] == name);
    for read in ["read_file", "list_dir", "glob", "grep", "web_fetch"] {
        assert_eq!(find(read).unwrap()["writes"], false, "{read}");
    }
    for write in ["write_file", "edit_file", "run_command"] {
        assert_eq!(find(write).unwrap()["writes"], true, "{write}");
    }
    assert!(find("read_file").unwrap()["parameters"]["properties"]["path"].is_object());
    // Delegation and questions to the user are not tools of the bridge.
    for other in ["spawn_agent", "ask_user", "start_session"] {
        assert!(find(other).is_none(), "{other}");
    }
    let first_write = tools
        .iter()
        .position(|tool| tool["writes"] == true)
        .unwrap();
    assert!(
        tools[first_write..]
            .iter()
            .all(|tool| tool["writes"] == true)
    );
}

#[tokio::test]
async fn reads_run_and_a_result_is_collected_once() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("notes.txt"),
        "first line\nsecond line\n",
    )
    .unwrap();
    let state = daemon(home.path(), "ask", NO_MODEL);
    let (session, _events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut connection = connect(&state);

    let read = run(
        &mut connection,
        &id,
        "read_file",
        json!({"path": "notes.txt"}),
        false,
    )
    .await
    .unwrap();
    assert_eq!(read["ok"], true);
    assert!(read["output"].as_str().unwrap().contains("second line"));

    // A result is handed over once.
    let started = call(
        &mut connection,
        "session.run_tool",
        json!({"session_id": id, "tool": "list_dir", "arguments": {"path": "."}}),
    )
    .await
    .unwrap();
    let call_id = started["call_id"].as_str().unwrap().to_string();
    let listing = collect(&mut connection, &id, &call_id).await.unwrap();
    assert!(listing["output"].as_str().unwrap().contains("notes.txt"));
    assert_eq!(
        call(
            &mut connection,
            "session.tool_result",
            json!({"session_id": id, "call_id": call_id})
        )
        .await,
        Err(codes::INVALID_PARAMS)
    );

    // The call is in the session's journal like any other.
    let (history, _) = session.subscribe_with_history();
    assert!(history.iter().any(|event| matches!(
        &event.event,
        SessionEvent::ToolFinished { tool: Some(tool), ok: true, .. } if tool == "read_file"
    )));
}

#[tokio::test]
async fn writes_need_the_flag_and_then_ask_the_user() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "ask", NO_MODEL);
    let (session, mut events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut connection = connect(&state);
    let target = project.path().join("out.txt");

    // Without the flag a write tool is not started, and neither is anything the bridge does not
    // run.
    for tool in [
        "write_file",
        "run_command",
        "spawn_agent",
        "ask_user",
        "nope",
    ] {
        assert_eq!(
            call(
                &mut connection,
                "session.run_tool",
                json!({"session_id": id, "tool": tool,
                       "arguments": {"path": "out.txt", "content": "x", "command": "echo"}}),
            )
            .await,
            Err(codes::INVALID_PARAMS),
            "{tool}"
        );
    }
    assert_eq!(
        call(
            &mut connection,
            "session.run_tool",
            json!({"session_id": id, "tool": "read_file", "arguments": "not an object"}),
        )
        .await,
        Err(codes::INVALID_PARAMS)
    );
    assert_eq!(
        call(
            &mut connection,
            "session.run_tool",
            json!({"session_id": "nope", "tool": "read_file", "arguments": {}}),
        )
        .await,
        Err(codes::SESSION_NOT_FOUND)
    );
    assert!(!target.exists());

    // With it, the call asks: nothing is written until the user allows it.
    let write = json!({"path": "out.txt", "content": "from the bridge"});
    for (decision, written) in [("deny", false), ("allow", true)] {
        let started = call(
            &mut connection,
            "session.run_tool",
            json!({"session_id": id, "tool": "write_file", "arguments": write,
                   "allow_write": true}),
        )
        .await
        .unwrap();
        let call_id = started["call_id"].as_str().unwrap().to_string();
        let request_id = loop {
            let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
                .await
                .expect("a permission request")
                .unwrap();
            if let SessionEvent::PermissionRequested {
                request_id, tool, ..
            } = event.event
            {
                assert_eq!(tool, "write_file");
                break request_id;
            }
        };
        // Still waiting for the answer.
        let waiting = call(
            &mut connection,
            "session.tool_result",
            json!({"session_id": id, "call_id": call_id, "wait_ms": 200}),
        )
        .await
        .unwrap();
        assert_eq!(waiting["done"], false);
        assert!(!target.exists());
        call(
            &mut connection,
            "session.respond",
            json!({"session_id": id, "request_id": request_id, "decision": decision}),
        )
        .await
        .unwrap();
        let result = collect(&mut connection, &id, &call_id).await.unwrap();
        assert_eq!(result["ok"], written, "{decision}: {result}");
        assert_eq!(target.exists(), written, "{decision}");
        if !written {
            assert!(result["output"].as_str().unwrap().contains("denied"));
        }
    }
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "from the bridge");
}

#[tokio::test]
async fn an_agents_profile_refuses_bridged_calls_like_its_own() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.txt"), "kept").unwrap();
    let state = daemon(home.path(), "full", NO_MODEL);
    let mut connection = connect(&state);
    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Work", "folders": [project.path()]}),
    )
    .await
    .unwrap();
    let agent = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": env["id"], "name": "Locked",
               "permissions": {"local_files_only": true}}),
    )
    .await
    .unwrap();
    let id = agent["session_id"].as_str().unwrap().to_string();

    // Local files stay readable; the web and commands are refused (and no command runs, even
    // in full mode with writes allowed).
    let read = run(
        &mut connection,
        &id,
        "read_file",
        json!({"path": "notes.txt"}),
        true,
    )
    .await
    .unwrap();
    assert_eq!(read["ok"], true, "{read}");
    for (tool, arguments) in [
        ("web_fetch", json!({"url": "http://127.0.0.1:9/"})),
        ("run_command", json!({"command": "echo hi > ran.txt"})),
    ] {
        let refused = run(&mut connection, &id, tool, arguments, true)
            .await
            .unwrap();
        assert_eq!(refused["ok"], false, "{tool}: {refused}");
        // (Whether the web tools exist depends on the web settings; the commands' refusal is the
        // profile's.)
        if tool == "run_command" {
            assert!(
                refused["output"]
                    .as_str()
                    .unwrap()
                    .contains("local files only"),
                "{tool}: {refused}"
            );
        }
    }
    assert!(!project.path().join("ran.txt").exists());
}

#[tokio::test]
async fn a_call_made_while_a_turn_runs_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "ask", &slow_model().await);
    let (session, _events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut connection = connect(&state);
    session.send("work slowly".into()).unwrap();
    let busy = run(
        &mut connection,
        &id,
        "list_dir",
        json!({"path": "."}),
        false,
    )
    .await
    .unwrap();
    assert_eq!(busy["ok"], false, "{busy}");
    assert!(busy["output"].as_str().unwrap().contains("running a turn"));
}

#[tokio::test]
async fn the_safety_floor_still_asks_in_full_mode() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".git")).unwrap();
    let state = daemon(home.path(), "full", NO_MODEL);
    let (session, mut events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut connection = connect(&state);
    let target = project.path().join(".git").join("hook.txt");
    let started = call(
        &mut connection,
        "session.run_tool",
        json!({"session_id": id, "tool": "write_file", "allow_write": true,
               "arguments": {"path": ".git/hook.txt", "content": "x"}}),
    )
    .await
    .unwrap();
    let call_id = started["call_id"].as_str().unwrap().to_string();
    let (request_id, summary) = loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("the floor asks even in full mode")
            .unwrap();
        if let SessionEvent::PermissionRequested {
            request_id,
            summary,
            ..
        } = event.event
        {
            break (request_id, summary);
        }
    };
    // The request says it comes from the bridge: the user's to answer.
    assert!(summary.starts_with("[MCP bridge] "), "{summary}");
    assert!(!target.exists());
    call(
        &mut connection,
        "session.respond",
        json!({"session_id": id, "request_id": request_id, "decision": "deny"}),
    )
    .await
    .unwrap();
    let result = collect(&mut connection, &id, &call_id).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(!target.exists());
}

#[tokio::test]
async fn stopping_the_session_ends_a_call_waiting_for_an_answer() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "ask", NO_MODEL);
    let (session, mut events) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut connection = connect(&state);
    let started = call(
        &mut connection,
        "session.run_tool",
        json!({"session_id": id, "tool": "write_file", "allow_write": true,
               "arguments": {"path": "out.txt", "content": "x"}}),
    )
    .await
    .unwrap();
    let call_id = started["call_id"].as_str().unwrap().to_string();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("a permission request")
            .unwrap();
        if matches!(event.event, SessionEvent::PermissionRequested { .. }) {
            break;
        }
    }
    call(
        &mut connection,
        "session.control",
        json!({"session_id": id, "action": "stop"}),
    )
    .await
    .unwrap();
    let result = collect(&mut connection, &id, &call_id).await.unwrap();
    assert_eq!(result["ok"], false, "{result}");
    assert!(!project.path().join("out.txt").exists());
}
