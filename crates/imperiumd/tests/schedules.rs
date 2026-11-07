//! Phase 6 exit criterion (first half): a schedule runs in the daemon with no window open and
//! every connected client is notified when it completes.

use std::time::Duration;

use axum::Router;
use axum::routing::post;
use futures_util::{SinkExt, StreamExt};
use imperium_protocol::PROTOCOL_VERSION;
use imperiumd::AccessToken;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// A model that answers every request with a short report.
async fn fake_model() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let chunk = json!({"choices":[{"delta":{"content":"Triaged 3 issues."}}]});
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

#[tokio::test]
async fn schedules_run_without_a_window_and_notify_clients() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_path = project.path().display().to_string().replace('\\', "/");
    let echo = "echo nightly audit done";
    let config = format!(
        r#"
        [models]
        default = "fake:model"
        [endpoints.fake]
        base_url = "{model}"
        [memory]
        enabled = false

        [[schedules]]
        id = "triage"
        trigger = {{ every = "1s" }}
        action = {{ project = "{project_path}", prompt = "Triage new issues." }}

        [[schedules]]
        id = "audit"
        trigger = {{ every = "1s" }}
        action = {{ project = "{project_path}", command = "{echo}" }}

        [[schedules]]
        id = "later"
        trigger = {{ cron = "0 3 * * *" }}
        action = {{ project = "{project_path}", command = "{echo}" }}
        "#,
        model = fake_model().await
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
    let daemon =
        imperiumd::DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/rpc", listener.local_addr().unwrap());
    tokio::spawn(imperiumd::serve(
        listener,
        AccessToken::from("secret".to_string()),
        std::sync::Arc::clone(&daemon),
        std::future::pending(),
    ));

    // A client connects, as the desktop or phone app would, then the scheduler starts.
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", "Bearer secret".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let handshake = json!({"jsonrpc":"2.0","id":1,"method":"system.handshake","params":{
        "client":{"name":"test","version":"0"},"protocol":PROTOCOL_VERSION}});
    socket
        .send(Message::Text(handshake.to_string().into()))
        .await
        .unwrap();
    socket.next().await.unwrap().unwrap();
    daemon.start_scheduler();

    let mut completed: Vec<(String, Value)> = Vec::new();
    while completed.len() < 2 {
        let message = tokio::time::timeout(Duration::from_secs(20), socket.next())
            .await
            .expect("a schedule event within 20 s")
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value["method"] != "schedule.event" {
            continue;
        }
        let params = value["params"].clone();
        let id = params["schedule_id"].as_str().unwrap().to_string();
        assert_ne!(params["status"], "failed", "{params}");
        if params["status"] == "completed" && !completed.iter().any(|(seen, _)| *seen == id) {
            completed.push((id, params));
        }
    }
    let triage = &completed.iter().find(|(id, _)| id == "triage").unwrap().1;
    assert_eq!(triage["summary"], "Triaged 3 issues.");
    let session_id = triage["session_id"].as_str().unwrap();
    assert!(daemon.session(session_id).is_some());
    let audit = &completed.iter().find(|(id, _)| id == "audit").unwrap().1;
    assert!(
        audit["summary"]
            .as_str()
            .unwrap()
            .contains("nightly audit done")
    );

    // The listing shows each schedule's history and next run; the run survives a reload.
    let list = json!({"jsonrpc":"2.0","id":2,"method":"schedules.list"});
    socket
        .send(Message::Text(list.to_string().into()))
        .await
        .unwrap();
    let rows = loop {
        let message = socket.next().await.unwrap().unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value["id"] == 2 {
            break value["result"]["schedules"].as_array().unwrap().clone();
        }
    };
    let later = rows.iter().find(|row| row["id"] == "later").unwrap();
    assert!(later["last_run"].is_null() && later["next_run"].is_string());
    assert_eq!(
        rows.iter().find(|row| row["id"] == "audit").unwrap()["last_ok"],
        true
    );
    assert!(home.path().join("data/schedules/history.json").is_file());
}
