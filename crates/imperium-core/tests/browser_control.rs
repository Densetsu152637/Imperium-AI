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
        Outcome::Error(error) => {
            // The tests compare codes; keep the reason for the failure output.
            eprintln!("{method} failed with {}: {}", error.code, error.message);
            Err(error.code)
        }
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
    if imperium_web::browser::browser_for_tests().is_none() {
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
    assert_eq!(done.view.tab.as_deref(), Some("1"));
    assert_eq!(done.view.tabs.len(), 1);

    // Tabs: the user opens one, switches back, and closes it again.
    let mut act_on = async |action: serde_json::Value| {
        let params = json!({"session_id": id, "action": action});
        call(&mut connection, "session.browser_act", params).await
    };
    let opened = act_on(json!({"type": "new_tab", "url": page}))
        .await
        .unwrap();
    let opened: BrowserActResult = serde_json::from_value(opened).unwrap();
    assert_eq!(opened.view.tab.as_deref(), Some("2"));
    assert_eq!(opened.view.tabs.len(), 2);
    assert!(opened.view.tabs[1].active && !opened.view.tabs[0].active);
    let back = act_on(json!({"type": "switch_tab", "tab": "1"}))
        .await
        .unwrap();
    let back: BrowserActResult = serde_json::from_value(back).unwrap();
    assert_eq!(back.view.tab.as_deref(), Some("1"));
    assert_eq!(
        act_on(json!({"type": "switch_tab", "tab": "9"})).await,
        Err(codes::INVALID_PARAMS)
    );
    let closed = act_on(json!({"type": "close_tab", "tab": "2"}))
        .await
        .unwrap();
    let closed: BrowserActResult = serde_json::from_value(closed).unwrap();
    assert_eq!(closed.view.tabs.len(), 1);
    assert_eq!(
        act_on(json!({"type": "close_tab", "tab": "1"})).await,
        Err(codes::INVALID_PARAMS),
        "the last tab stays"
    );

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
