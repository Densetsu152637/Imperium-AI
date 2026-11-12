//! `session.browser_control` and `session.browser_act`: the user takes over the session's
//! browser, drives it, and gives it back; every client hears of it in the session's events.

use std::sync::Arc;
use std::time::Duration;

use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::{BrowserActResult, BrowserControlResult};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};

fn daemon(home: &std::path::Path, browser: bool) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n\
         [web]\nenabled = true\nbrowser = {browser}\nheadless = true"
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

async fn call(connection: &mut Connection, method: &str, params: Value) -> Result<Value, i32> {
    let frame = json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response: Response = match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(90), deferred)
            .await
            .expect("a reply within 90 s"),
        (None, None) => panic!("no reply"),
    };
    match response.outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => Err(error.code),
    }
}

fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    let handshake = json!({"jsonrpc": "2.0", "id": 1, "method": "system.handshake",
        "params": {"client": {"name": "test", "version": "0"},
                   "protocol": {"major": 0, "minor": 2}}});
    let _ = connection.handle_text(&handshake.to_string());
    connection
}

#[tokio::test]
async fn takeover_needs_a_session_with_a_browser() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), false);
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let mut connection = connect(&state);
    let take = json!({"session_id": session.id(), "user": true});
    assert_eq!(
        call(&mut connection, "session.browser_control", take).await,
        Err(codes::INVALID_PARAMS)
    );
    let missing = json!({"session_id": "nope", "user": true});
    assert_eq!(
        call(&mut connection, "session.browser_control", missing).await,
        Err(codes::SESSION_NOT_FOUND)
    );
    let act = json!({"session_id": session.id(), "action": {"type": "reload"}});
    assert_eq!(
        call(&mut connection, "session.browser_act", act).await,
        Err(codes::INVALID_PARAMS)
    );
}

/// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
#[tokio::test]
async fn the_user_takes_the_browser_over_drives_it_and_gives_it_back() {
    if imperium_web::browser::find_browser().is_none() {
        eprintln!("no Chrome, Chromium, or Edge found; skipping");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), true);
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    let mut events = session.subscribe();
    let mut connection = connect(&state);

    // Driving needs the takeover first.
    let act = json!({"session_id": id, "action": {"type": "reload"}});
    assert_eq!(
        call(&mut connection, "session.browser_act", act.clone()).await,
        Err(codes::INVALID_PARAMS)
    );

    let taken = call(
        &mut connection,
        "session.browser_control",
        json!({"session_id": id, "user": true}),
    )
    .await
    .unwrap();
    let taken: BrowserControlResult = serde_json::from_value(taken).unwrap();
    assert!(taken.view.is_some_and(|view| !view.screenshot.is_empty()));

    let app = axum::Router::new().route(
        "/",
        axum::routing::get(|| async {
            axum::response::Html("<html><head><title>Driven</title></head><body>hi</body></html>")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let page = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    // Only web pages can be opened, not data: or file: URLs.
    let local =
        json!({"session_id": id, "action": {"type": "navigate", "url": "file:///etc/passwd"}});
    assert_eq!(
        call(&mut connection, "session.browser_act", local).await,
        Err(codes::INVALID_PARAMS)
    );
    let navigate = json!({"session_id": id, "action": {"type": "navigate", "url": page}});
    let done = call(&mut connection, "session.browser_act", navigate)
        .await
        .unwrap();
    let done: BrowserActResult = serde_json::from_value(done).unwrap();
    assert_eq!(done.view.title, "Driven");

    let given = call(
        &mut connection,
        "session.browser_control",
        json!({"session_id": id, "user": false}),
    )
    .await
    .unwrap();
    assert_eq!(given, json!({"view": null}));
    assert_eq!(
        call(&mut connection, "session.browser_act", act).await,
        Err(codes::INVALID_PARAMS)
    );

    // Every client saw who drove, and the pages.
    let mut control = Vec::new();
    let mut titles = Vec::new();
    while let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(300), events.recv()).await
    {
        match event.event {
            SessionEvent::BrowserControl { user } => control.push(user),
            SessionEvent::BrowserView { title, .. } => titles.push(title),
            _ => {}
        }
    }
    assert_eq!(control, [true, false]);
    assert_eq!(titles.last().map(String::as_str), Some("Driven"));
}
