//! `session.agents` and `session.agent_resume`: a finished subagent is listed as idle and
//! resumable, takes a follow-up from the user, keeps its state in the session's folder, and that
//! state is deleted with the session.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::{SessionAgentResumeResult, SessionAgentsResult};
use imperium_protocol::session::{AgentEvent, AgentStatus, SessionEvent};
use serde_json::{Value, json};
use tokio::sync::broadcast;

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
                json!({"content": "ok"})
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

fn call(connection: &mut Connection, method: &str, params: Value) -> Response {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    connection
        .handle_text(&frame.to_string())
        .response
        .expect("an immediate reply")
}

fn result<T: serde::de::DeserializeOwned>(response: Response) -> T {
    match response.outcome {
        Outcome::Result(value) => serde_json::from_value(value).unwrap(),
        Outcome::Error(error) => panic!("{error:?}"),
    }
}

fn error_code(response: Response) -> i32 {
    match response.outcome {
        Outcome::Error(error) => error.code,
        Outcome::Result(value) => panic!("expected an error, got {value}"),
    }
}

async fn until(
    events: &mut broadcast::Receiver<AgentEvent>,
    agent: &str,
    wanted: impl Fn(&SessionEvent) -> bool,
) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
            .await
            .expect("an event within 15 s")
            .unwrap();
        if event.agent_id == agent && wanted(&event.event) {
            return;
        }
    }
}

#[tokio::test]
async fn subagents_are_listed_resumed_and_deleted_with_the_session() {
    let url = serve().await;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &url);
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let session_id = session.id().to_string();
    let mut events = session.subscribe();
    let mut connection = Connection::new(Arc::clone(&state));
    let handshake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": {"major": 0, "minor": 2}});
    let _: Value = result(call(&mut connection, "system.handshake", handshake));

    session.send("delegate".into()).unwrap();
    until(&mut events, "root", |event| {
        matches!(event, SessionEvent::TurnCompleted { .. })
    })
    .await;

    let listed: SessionAgentsResult = result(call(
        &mut connection,
        "session.agents",
        json!({"session_id": session_id}),
    ));
    let ids: Vec<&str> = listed
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, ["root", "t1"]);
    assert_eq!(listed.agents[1].status, AgentStatus::Idle);
    assert!(listed.agents[1].resumable);

    let resume = |agent: &str, text: &str| json!({"session_id": session_id, "agent_id": agent, "text": text});
    let resumed: SessionAgentResumeResult = result(call(
        &mut connection,
        "session.agent_resume",
        resume("t1", "and the tests?"),
    ));
    assert_eq!(
        (resumed.agent_id.as_str(), resumed.model.as_str()),
        ("t1", "fake:m")
    );
    until(&mut events, "t1", |event| {
        matches!(event, SessionEvent::TaskFinished { ok: true, report, .. } if report == "sub: and the tests?")
    })
    .await;

    for (agent, text) in [("root", "hi"), ("t7", "hi"), ("t1", "")] {
        let refused = call(&mut connection, "session.agent_resume", resume(agent, text));
        assert_eq!(error_code(refused), codes::INVALID_PARAMS, "{agent}");
    }

    let kept = imperium_config::Paths::under(home.path())
        .data_dir()
        .join("sessions")
        .join(format!("{session_id}.agents"));
    assert!(kept.join("t1.json").is_file());
    let _: Value = result(call(
        &mut connection,
        "session.delete",
        json!({"session_id": session_id}),
    ));
    assert!(!kept.exists());
}
