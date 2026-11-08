//! `session.send` modes and `session.queue`: a message sent during a turn is queued with an
//! id, a steering one joins the running turn, queued messages can be listed, edited, and
//! removed, and bad targets or actions are refused.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::{SessionQueueResult, SessionSendResult};
use imperium_protocol::session::{AgentEvent, SessionEvent};
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// Answers `slow` after a pause and anything else at once.
async fn serve() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let said = body["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .and_then(|message| message["content"].as_str())
                .unwrap_or_default()
                .to_string();
            if said == "slow" {
                tokio::time::sleep(Duration::from_millis(1_000)).await;
            }
            let chunk = json!({"choices": [{"delta": {"content": "ok"}}]});
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
         [workflow]\nenabled = false"
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

async fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Response {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(20), deferred)
            .await
            .expect("an answer within 20 s"),
        (None, None) => panic!("no reply"),
    }
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
    wanted: impl Fn(&SessionEvent) -> bool,
) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
            .await
            .expect("an event within 15 s")
            .unwrap();
        if event.agent_id == "root" && wanted(&event.event) {
            return;
        }
    }
}

#[tokio::test]
async fn send_modes_and_queue_actions() {
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
    let _: Value = result(call(&mut connection, 1, "system.handshake", handshake).await);

    let send = |text: &str, mode: Option<&str>, agent: Option<&str>| {
        let mut params = json!({"session_id": session_id, "text": text});
        if let Some(mode) = mode {
            params["mode"] = json!(mode);
        }
        if let Some(agent) = agent {
            params["agent_id"] = json!(agent);
        }
        params
    };
    let queue = |action: &str, id: Option<&str>, text: Option<&str>| json!({"session_id": session_id, "action": action, "id": id, "text": text});

    // An idle session starts the turn; a plain send has no mode (today's clients).
    let started: SessionSendResult =
        result(call(&mut connection, 2, "session.send", send("slow", None, None)).await);
    assert_eq!(started.id, "q1");
    until(&mut events, |event| {
        matches!(event, SessionEvent::TurnStarted { .. })
    })
    .await;

    // Busy: one queued message, one steering message, one to remove.
    let queued: SessionSendResult = result(
        call(
            &mut connection,
            3,
            "session.send",
            send("next", Some("queue"), None),
        )
        .await,
    );
    let steering: SessionSendResult = result(
        call(
            &mut connection,
            4,
            "session.send",
            send("mind the tests", Some("steer"), Some("root")),
        )
        .await,
    );
    let doomed: SessionSendResult = result(
        call(
            &mut connection,
            5,
            "session.send",
            send("never", None, None),
        )
        .await,
    );
    until(
        &mut events,
        |event| matches!(event, SessionEvent::MessageQueued { id, .. } if *id == doomed.id),
    )
    .await;

    let listed: SessionQueueResult = result(
        call(
            &mut connection,
            6,
            "session.queue",
            queue("list", None, None),
        )
        .await,
    );
    let summary: Vec<(String, String, bool)> = listed
        .messages
        .into_iter()
        .map(|message| (message.id, message.text, message.steer))
        .collect();
    assert_eq!(
        summary,
        [
            (queued.id.clone(), "next".into(), false),
            (steering.id.clone(), "mind the tests".into(), true),
            (doomed.id.clone(), "never".into(), false),
        ]
    );
    let edited: SessionQueueResult = result(
        call(
            &mut connection,
            7,
            "session.queue",
            queue("edit", Some(&queued.id), Some("next, edited")),
        )
        .await,
    );
    assert_eq!(edited.messages[0].text, "next, edited");
    let removed: SessionQueueResult = result(
        call(
            &mut connection,
            8,
            "session.queue",
            queue("remove", Some(&doomed.id), None),
        )
        .await,
    );
    assert_eq!(removed.messages.len(), 2);

    // Refusals: an unknown message, a missing id, an unknown action, an unknown agent, and
    // an empty message.
    for (id, method, params) in [
        (9, "session.queue", queue("remove", Some("q99"), None)),
        (10, "session.queue", queue("edit", None, Some("x"))),
        (11, "session.queue", queue("shuffle", None, None)),
        (12, "session.send", send("hi", Some("steer"), Some("t9"))),
        (13, "session.send", send("  ", None, None)),
    ] {
        assert_eq!(
            error_code(call(&mut connection, id, method, params).await),
            codes::INVALID_PARAMS,
            "{method} {id}"
        );
    }
    // An unknown mode is invalid params too.
    let bad_mode = call(
        &mut connection,
        14,
        "session.send",
        send("hi", Some("interrupt"), None),
    )
    .await;
    assert_eq!(error_code(bad_mode), codes::INVALID_PARAMS);

    // The steering message joined the running turn; the queued one ran after it.
    until(&mut events, |event| {
        matches!(event, SessionEvent::TurnCompleted { .. })
    })
    .await;
    until(&mut events, |event| {
        matches!(event, SessionEvent::TurnCompleted { .. })
    })
    .await;
    let (history, _) = session.subscribe_with_history();
    let root: Vec<&SessionEvent> = history
        .iter()
        .filter(|event| event.agent_id == "root")
        .map(|event| &event.event)
        .collect();
    assert!(root.contains(&&SessionEvent::Steered {
        id: steering.id.clone(),
        text: "mind the tests".into(),
    }));
    let turns: Vec<&String> = root
        .iter()
        .filter_map(|event| match event {
            SessionEvent::TurnStarted { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(turns, ["slow", "next, edited"]);
    let empty: SessionQueueResult = result(
        call(
            &mut connection,
            15,
            "session.queue",
            queue("list", None, None),
        )
        .await,
    );
    assert!(empty.messages.is_empty());
}
