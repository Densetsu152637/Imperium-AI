//! `session.export`: a session with a subagent is exported as Markdown and JSON, while running
//! and after it is archived (read back from its journal), with or without the subagent's log.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::SessionExportResult;
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};

/// Root: `delegate` spawns a subagent, a spawn result awaits it, its report ends the turn.
/// Subagent: answers `sub: <message>`.
async fn serve() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let subagent = messages[0]["content"]
                .as_str()
                .is_some_and(|system| system.starts_with("You are an Imperium subagent"));
            let last = messages.last().cloned().unwrap_or_default();
            let said = last["content"].as_str().unwrap_or_default().to_string();
            let tool = |id: &str, name: &str, arguments: Value| {
                json!({"tool_calls": [{"index": 0, "id": id,
                    "function": {"name": name, "arguments": arguments.to_string()}}]})
            };
            let delta = if subagent {
                json!({"content": format!("sub: {said}")})
            } else if said == "delegate" {
                tool("spawn", "spawn_agent", json!({"brief": "look around"}))
            } else if said.starts_with("started ") {
                tool("await", "await_tasks", json!({}))
            } else {
                json!({"content": "all done"})
            };
            let chunk = json!({"choices": [{"delta": delta}]});
            (
                [("content-type", "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn daemon(home: &std::path::Path, url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{url}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false"
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

async fn export(
    connection: &mut Connection,
    session_id: &str,
    format: &str,
    include_subagents: Option<bool>,
) -> Result<SessionExportResult, i32> {
    let mut params = json!({"session_id": session_id, "format": format});
    if let Some(include) = include_subagents {
        params["include_subagents"] = json!(include);
    }
    let frame = json!({"jsonrpc": "2.0", "id": 2, "method": "session.export", "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response: Response = match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(30), deferred)
            .await
            .expect("an export within 30 s"),
        (None, None) => panic!("no reply"),
    };
    match response.outcome {
        Outcome::Result(value) => Ok(serde_json::from_value(value).unwrap()),
        Outcome::Error(error) => Err(error.code),
    }
}

#[tokio::test]
async fn a_session_with_a_subagent_exports_running_and_archived() {
    let url = serve().await;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url);
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut events = session.subscribe();
    let mut connection = Connection::new(Arc::clone(&state));
    let handshake = json!({"jsonrpc": "2.0", "id": 1, "method": "system.handshake",
        "params": {"client": {"name": "test", "version": "0"},
                   "protocol": {"major": 0, "minor": 2}}});
    let _ = connection.handle_text(&handshake.to_string());

    session.send("delegate".into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
            .await
            .expect("an event within 15 s")
            .unwrap();
        if event.agent_id == "root" && matches!(event.event, SessionEvent::TurnCompleted { .. }) {
            break;
        }
    }

    let check_markdown = |text: &str| {
        assert!(text.contains("delegate"), "{text}");
        assert!(text.contains("Tool: spawn_agent"), "{text}");
        assert!(text.contains("Subagent t1"), "{text}");
        assert!(text.contains("sub: look around"), "{text}");
        assert!(text.contains("all done"), "{text}");
    };
    let check_json = |text: &str, with_log: bool| {
        let doc: Value = serde_json::from_str(text).unwrap();
        assert_eq!(doc["session_id"], id.as_str());
        let sub = &doc["root"]["subagents"][0];
        assert_eq!(sub["agent_id"], "t1");
        assert_eq!(sub["task"], "look around");
        assert_eq!(sub["report"], "sub: look around");
        assert_eq!(!sub["entries"].as_array().unwrap().is_empty(), with_log);
        assert!(
            doc["root"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["type"] == "tool" && entry["tool"] == "spawn_agent")
        );
    };

    // Running.
    let markdown = export(&mut connection, &id, "markdown", None)
        .await
        .unwrap();
    assert!(markdown.filename.ends_with(".md"));
    check_markdown(&markdown.content);
    let json_export = export(&mut connection, &id, "json", None).await.unwrap();
    assert!(json_export.filename.ends_with(".json"));
    check_json(&json_export.content, true);
    let short = export(&mut connection, &id, "json", Some(false))
        .await
        .unwrap();
    check_json(&short.content, false);

    // Archived: read back from the journal.
    state.archive(&id).unwrap();
    assert!(state.session(&id).is_none());
    let archived_md = export(&mut connection, &id, "markdown", None)
        .await
        .unwrap();
    check_markdown(&archived_md.content);
    let archived_json = export(&mut connection, &id, "json", None).await.unwrap();
    check_json(&archived_json.content, true);

    // No such session.
    assert_eq!(
        export(&mut connection, "nope", "markdown", None)
            .await
            .unwrap_err(),
        codes::INVALID_PARAMS
    );
}
