//! Phase 11 exit criterion: a paired phone approves a queued action. The phone app is served,
//! pairing exchanges a one-time code for a token, and the phone's connection is limited to its
//! scopes.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use imperium_protocol::PROTOCOL_VERSION;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(socket: &mut Socket, id: u64, method: &str, params: Value) {
    let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .unwrap();
}

/// The next message that is either the response to `id` or a notification named `method`.
async fn next(socket: &mut Socket, id: Option<u64>, method: Option<&str>) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a message within 10 s")
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if id.is_some_and(|id| value["id"] == id) || method.is_some_and(|m| value["method"] == m) {
            return value;
        }
    }
}

#[tokio::test]
async fn a_paired_phone_approves_a_queued_action() {
    let home = tempfile::tempdir().unwrap();
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            "[phone]\nbind = \"127.0.0.1:0\"".parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    let daemon =
        imperium_core::DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
    let address = daemon.start_phone().await.unwrap().unwrap();
    let base = format!("http://{address}");

    // The phone app is served.
    let page = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("imperium.phone.token"));

    // Pairing: a wrong code is refused, the right one gives a token once.
    let http = reqwest::Client::new();
    let wrong = http
        .post(format!("{base}/pair"))
        .json(&json!({"code": "12345678", "name": "Phone"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 403);
    let code = daemon.devices().new_code().code;
    let paired: Value = http
        .post(format!("{base}/pair"))
        .json(&json!({"code": code, "name": "Pixel 9"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = paired["token"].as_str().unwrap().to_string();
    assert_eq!(daemon.devices().list()[0].name, "Pixel 9");

    // An unpaired token cannot connect.
    let mut bad = format!("ws://{address}/rpc").into_client_request().unwrap();
    bad.headers_mut()
        .insert("Authorization", "Bearer nope".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(bad).await.is_err());

    let mut request = format!("ws://{address}/rpc").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut phone, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    send(
        &mut phone,
        1,
        "system.handshake",
        json!({"client": {"name": "phone", "version": "0"}, "protocol": PROTOCOL_VERSION}),
    )
    .await;
    assert!(next(&mut phone, Some(1), None).await["result"].is_object());

    // Phones cannot start sessions.
    send(&mut phone, 2, "session.create", json!({"project_dir": "."})).await;
    let refused = next(&mut phone, Some(2), None).await;
    assert_eq!(refused["error"]["code"], -32005, "{refused}");

    // Unattended work queues an approval; the phone hears about it and approves it.
    let answer =
        daemon
            .approvals()
            .request(&daemon, "push", "Push the fix for owner/repo#7".into());
    let requested = next(&mut phone, None, Some("approval.requested")).await;
    let id = requested["params"]["id"].as_u64().unwrap();
    send(
        &mut phone,
        3,
        "approvals.respond",
        json!({"id": id, "approve": true}),
    )
    .await;
    assert_eq!(
        next(&mut phone, Some(3), None).await["result"]["found"],
        true
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .unwrap()
            .unwrap()
    );

    // Revoked phones cannot reconnect.
    let device = daemon.devices().list()[0].id.clone();
    assert!(daemon.devices().revoke(&device).unwrap());
    let mut again = format!("ws://{address}/rpc").into_client_request().unwrap();
    again
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    assert!(tokio_tungstenite::connect_async(again).await.is_err());
}
