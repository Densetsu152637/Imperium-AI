//! `session.browser_control` and `session.browser_act`: the user takes over the session's
//! browser, drives it, and gives it back; every client hears of it in the session's events. Only
//! the desktop app may: a paired device, a mod's page, a plain token holder (an agent's command
//! on the socket) and any caller of a server are refused before anything is looked at.

use std::sync::Arc;
use std::time::Duration;

use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes};
use imperium_protocol::methods::{BrowserActResult, BrowserControlResult};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};

fn daemon(home: &std::path::Path, browser: bool) -> Arc<DaemonState> {
    daemon_with(home, browser, "")
}

fn daemon_with(home: &std::path::Path, browser: bool, extra: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n\
         [web]\nenabled = true\nbrowser = {browser}\nheadless = true\n{extra}"
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

/// The desktop app's connection: the only one that may take the browser over.
fn connect(state: &Arc<DaemonState>) -> Connection {
    shake(Connection::new(Arc::clone(state)).as_desktop_app_for_tests())
}

fn shake(mut connection: Connection) -> Connection {
    let handshake = json!({"jsonrpc": "2.0", "id": 1, "method": "system.handshake",
        "params": {"client": {"name": "test", "version": "0"},
                   "protocol": {"major": 0, "minor": 2}}});
    let _ = connection.handle_text(&handshake.to_string());
    connection
}

/// The error a refused call answers with: its code and its data.
async fn refusal(connection: &mut Connection, method: &str, params: Value) -> (i32, Value) {
    let frame = json!({"jsonrpc": "2.0", "id": 3, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response: Response = match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(30), deferred)
            .await
            .expect("a reply within 30 s"),
        (None, None) => panic!("no reply"),
    };
    match response.outcome {
        Outcome::Result(value) => panic!("{method} was answered: {value}"),
        Outcome::Error(error) => (error.code, error.data.unwrap_or(Value::Null)),
    }
}

/// The takeover calls a refused caller might send: take the browser, reload it, navigate it.
fn takeover_calls(session_id: &str) -> [(&'static str, Value); 3] {
    [
        (
            "session.browser_control",
            json!({"session_id": session_id, "user": true}),
        ),
        (
            "session.browser_act",
            json!({"session_id": session_id, "action": {"type": "reload"}}),
        ),
        (
            "session.browser_act",
            json!({"session_id": session_id,
                   "action": {"type": "navigate", "url": "https://example.com/"}}),
        ),
    ]
}

#[tokio::test]
async fn only_the_desktop_app_may_take_the_browser_over_or_drive_it() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), false);
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let mut events = session.subscribe();
    // A plain token holder: an agent's `run_command` on the socket, a hook, a plugin's server, a
    // mod's page over loopback TCP. None of them is the installed desktop app.
    let mut plain = shake(Connection::new(Arc::clone(&state)));
    // A paired device, even one whose scopes listed the methods.
    let mut device = shake(Connection::restricted(
        Arc::clone(&state),
        vec!["session.browser_control", "session.browser_act"],
    ));
    for connection in [&mut plain, &mut device] {
        for (method, params) in takeover_calls(session.id()) {
            // Refused before the session is looked at: this session has no browser, which the
            // desktop app would be told (`INVALID_PARAMS`, below).
            assert_eq!(
                refusal(connection, method, params).await.0,
                codes::FORBIDDEN,
                "{method}"
            );
        }
        for (method, params) in takeover_calls("nope") {
            assert_eq!(
                refusal(connection, method, params).await.0,
                codes::FORBIDDEN,
                "{method}"
            );
        }
    }
    // Nobody took the browser over.
    let changed = tokio::time::timeout(Duration::from_millis(200), async {
        while let Ok(event) = events.recv().await {
            if matches!(event.event, SessionEvent::BrowserControl { .. }) {
                return true;
            }
        }
        false
    })
    .await;
    assert_ne!(
        changed,
        Ok(true),
        "a refused caller changed who drives the browser"
    );
}

/// On a server (`daemon.server`) nobody takes the browser over, not even the desktop app: both
/// calls answer `-32010` with the feature's name before the caller is looked at.
#[tokio::test]
async fn on_a_server_nobody_takes_the_browser_over() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon_with(home.path(), false, "[daemon]\nserver = true");
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let mut app = connect(&state);
    let mut plain = shake(Connection::new(Arc::clone(&state)));
    for connection in [&mut app, &mut plain] {
        for (method, params) in takeover_calls(session.id()) {
            assert_eq!(
                refusal(connection, method, params).await,
                (
                    codes::UNAVAILABLE_ON_SERVER,
                    json!({"feature": imperium_protocol::jsonrpc::server_features::BROWSER_CONTROL})
                ),
                "{method}"
            );
        }
    }
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
