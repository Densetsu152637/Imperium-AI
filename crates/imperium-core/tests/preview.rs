//! A session's preview server (`preview_open` with a command) runs in the session's background
//! processes, so the daemon ending the session ends it too, even while something still holds
//! the session.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{DaemonState, SessionOptions};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};

/// Calls `preview_open` with the command and port the user's message names, then ends.
async fn serve(command: String, port: u16) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let command = command.clone();
            async move {
                let tools = body["messages"]
                    .as_array()
                    .map_or(0, |m| m.iter().filter(|m| m["role"] == "tool").count());
                let delta = if tools == 0 {
                    json!({"tool_calls": [{"index": 0, "id": "p", "function": {
                        "name": "preview_open",
                        "arguments": json!({"command": command, "port": port}).to_string()}}]})
                } else {
                    json!({"content": "Previewing."})
                };
                let chunk = json!({"choices": [{"delta": delta}]});
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// Not a test of its own: the test below runs this binary again as a stand-in dev server, in a
/// folder that names the port. Elsewhere it does nothing.
#[tokio::test]
async fn stand_in_dev_server() {
    let Ok(port) = std::fs::read_to_string("imperium-preview-port") else {
        return;
    };
    let port: u16 = port.trim().parse().unwrap();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let _ = listener.accept().await;
        }
    })
    .await;
}

async fn listening(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

#[tokio::test]
async fn ending_a_session_stops_its_preview_server() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("imperium-preview-port"),
        port.to_string(),
    )
    .unwrap();
    let command = format!(
        "\"{}\" --exact stand_in_dev_server --nocapture",
        std::env::current_exe().unwrap().display()
    );
    let url = serve(command, port).await;
    let home = tempfile::tempdir().unwrap();
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
    let state: Arc<DaemonState> =
        DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
    let (session, _) = state
        .create_session_with(
            &project.path().display().to_string(),
            None,
            SessionOptions {
                pre_approved: vec!["run_command".into()],
                ..SessionOptions::default()
            },
        )
        .unwrap();
    let id = session.id().to_string();
    let mut events = session.subscribe();
    session.send("Show the app".into()).unwrap();
    let mut previewed = None;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .expect("an event within 60 s")
            .unwrap();
        match event.event {
            SessionEvent::PreviewChanged { url, .. } => previewed = url,
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(previewed, Some(format!("http://localhost:{port}/")));
    assert!(listening(port).await);

    // The daemon ends the session while this test still holds it.
    state.forget_session(&id);
    let mut stopped = false;
    for _ in 0..80 {
        if !listening(port).await {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(stopped, "the preview server outlived its session");
    drop(session);
}
