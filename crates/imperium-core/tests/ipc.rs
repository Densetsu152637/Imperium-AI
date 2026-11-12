//! The same protocol over the local socket (a Unix domain socket, or a Windows named pipe),
//! with the same token check.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use imperium_core::AccessToken;
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::jsonrpc::{Outcome, Response};
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{WebSocketStream, client_async};

type Socket = WebSocketStream<imperium_ipc::Stream>;

struct Daemon {
    address: String,
    state: Arc<imperium_core::DaemonState>,
    _data: tempfile::TempDir,
}

async fn start(token: &str) -> Daemon {
    let data = tempfile::tempdir().unwrap();
    let listener = imperium_ipc::Listener::bind(data.path()).unwrap();
    let address = listener.address();
    let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
        .settings()
        .unwrap();
    let state =
        imperium_core::DaemonState::new(imperium_config::Paths::under(data.path()), settings, None);
    tokio::spawn(imperium_core::serve_ipc(
        listener,
        AccessToken::from(token.to_string()),
        Arc::clone(&state),
        std::future::pending(),
    ));
    Daemon {
        address,
        state,
        _data: data,
    }
}

async fn upgrade(address: &str, authorization: Option<&str>) -> Result<Socket, tungstenite::Error> {
    let stream = imperium_ipc::connect(address).await.unwrap();
    let mut request = "ws://localhost/rpc".into_client_request().unwrap();
    if let Some(authorization) = authorization {
        request
            .headers_mut()
            .insert("Authorization", authorization.parse().unwrap());
    }
    client_async(request, stream)
        .await
        .map(|(socket, _)| socket)
}

async fn reply(socket: &mut Socket, call: serde_json::Value) -> Response {
    socket
        .send(Message::Text(call.to_string().into()))
        .await
        .unwrap();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a reply within 10 s")
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        if value.get("id").is_some() {
            return serde_json::from_value(value).unwrap();
        }
    }
}

#[tokio::test]
async fn an_authorised_client_speaks_the_protocol_over_the_socket() {
    let daemon = start("secret").await;
    let mut socket = upgrade(&daemon.address, Some("Bearer secret"))
        .await
        .unwrap();
    for (id, call) in [
        (
            1,
            json!({"jsonrpc":"2.0","id":1,"method":"system.handshake","params":{
                "client":{"name":"test","version":"0"},"protocol":PROTOCOL_VERSION}}),
        ),
        (2, json!({"jsonrpc":"2.0","id":2,"method":"system.ping"})),
    ] {
        let response = reply(&mut socket, call).await;
        assert!(
            matches!(response.outcome, Outcome::Result(_)),
            "call {id}: {response:?}"
        );
    }
}

#[tokio::test]
async fn a_local_connection_still_needs_the_token() {
    let daemon = start("secret").await;
    for header in [None, Some("Bearer wrong"), Some("secret")] {
        match upgrade(&daemon.address, header).await {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 401),
            other => panic!("expected 401 for {header:?}, got {:?}", other.map(|_| ())),
        }
    }
}

#[tokio::test]
async fn subscriptions_and_announcements_work_over_the_socket() {
    let daemon = start("secret").await;
    let mut socket = upgrade(&daemon.address, Some("Bearer secret"))
        .await
        .unwrap();
    reply(
        &mut socket,
        json!({"jsonrpc":"2.0","id":1,"method":"system.handshake","params":{
            "client":{"name":"test","version":"0"},"protocol":PROTOCOL_VERSION}}),
    )
    .await;
    let selected = reply(
        &mut socket,
        json!({"jsonrpc":"2.0","id":2,"method":"events.subscribe","params":{
            "topics":["model.residency"]}}),
    )
    .await;
    assert!(matches!(selected.outcome, Outcome::Result(_)));

    daemon.state.announce("session.updated", json!({"n": 1}));
    daemon.state.announce("model.residency", json!({"n": 2}));
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("a notification within 10 s")
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
    assert_eq!(value["method"], "model.residency", "{value}");
}
