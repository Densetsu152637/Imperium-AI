//! A session is titled by its model after its first exchange; the user can rename it, and both
//! survive a restart. A generated title never replaces the user's.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Answers a title request with a labelled, quoted title, and anything else with "Done.".
async fn fake_model() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let asks_title = body["messages"][0]["content"]
                .as_str()
                .is_some_and(|text| text.contains("You name chat sessions"));
            let text = if asks_title {
                "Title: \"Fixing the login bug.\""
            } else {
                "Done."
            };
            let chunk = json!({"choices":[{"delta":{"content": text}}]});
            (
                [("content-type", "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn daemon(home: &std::path::Path, base_url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
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

fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let Some(Response { outcome, .. }) = handled.response else {
        panic!("no immediate reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {error:?}"),
    }
}

async fn control(connection: &mut Connection, params: Value) -> Outcome {
    let frame = json!({"jsonrpc": "2.0", "id": 9, "method": "session.control", "params": params});
    let handled = connection.handle_text(&frame.to_string());
    handled.deferred.expect("a deferred reply").await.outcome
}

fn handshake(connection: &mut Connection) {
    call(
        connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
}

fn title(connection: &mut Connection) -> String {
    let listed = call(connection, 3, "session.list", json!({}));
    listed["sessions"][0]["title"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn sessions_are_titled_after_the_first_exchange_and_can_be_renamed() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let url = fake_model().await;
    let state = daemon(home.path(), &url);
    let mut connection = Connection::new(Arc::clone(&state));
    handshake(&mut connection);
    let created = call(
        &mut connection,
        2,
        "session.create",
        json!({"project_dir": project.path()}),
    );
    let session_id = created["session_id"].as_str().unwrap().to_string();
    let mut announcements = state.announcements();
    let session = state.session(&session_id).unwrap();
    let mut events = session.subscribe();
    session
        .send("the login page rejects every password, please look".into())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::TurnCompleted { .. } => break,
                SessionEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");
    let titled = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let notice = announcements.recv().await.unwrap();
            if notice.method == "session.updated" && notice.params["title"] != "" {
                break notice.params;
            }
        }
    })
    .await
    .expect("the generated title is announced");
    assert_eq!(titled["title"], "Fixing the login bug");
    assert_eq!(title(&mut connection), "Fixing the login bug");

    // The user's name replaces it, and a generated title never replaces the user's.
    let renamed = control(
        &mut connection,
        json!({"session_id": session_id, "action": "rename", "title": "  Login\nfix "}),
    )
    .await;
    assert!(matches!(renamed, Outcome::Result(_)), "{renamed:?}");
    assert_eq!(title(&mut connection), "Login fix");
    assert!(!state.set_title(&session_id, "Generated", false).unwrap());
    assert_eq!(title(&mut connection), "Login fix");
    let missing = control(
        &mut connection,
        json!({"session_id": session_id, "action": "rename"}),
    )
    .await;
    assert!(matches!(missing, Outcome::Error(_)), "{missing:?}");

    // After a restart the user's title is kept.
    drop(events);
    drop(session);
    drop(announcements);
    drop(connection);
    drop(state);
    let state = daemon(home.path(), &url);
    assert_eq!(state.resume_sessions(), 1);
    let mut connection = Connection::new(Arc::clone(&state));
    handshake(&mut connection);
    assert_eq!(title(&mut connection), "Login fix");
    assert!(!state.set_title(&session_id, "Generated", false).unwrap());

    // Clearing it shows the first message's words until the next exchange titles it again.
    let cleared = control(
        &mut connection,
        json!({"session_id": session_id, "action": "rename", "title": ""}),
    )
    .await;
    assert!(matches!(cleared, Outcome::Result(_)), "{cleared:?}");
    assert_eq!(
        title(&mut connection),
        "The login page rejects every password"
    );
}
