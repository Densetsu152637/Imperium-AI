use futures_util::{SinkExt, StreamExt};
use imperium_core::AccessToken;
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::jsonrpc::{Outcome, Response};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

async fn start_server(token: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/rpc", listener.local_addr().unwrap());
    let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
        .settings()
        .unwrap();
    let daemon = imperium_core::DaemonState::new(
        imperium_config::Paths::under(std::path::Path::new("unused")),
        settings,
        None,
    );
    tokio::spawn(imperium_core::serve(
        listener,
        AccessToken::from(token.to_string()),
        daemon,
        std::future::pending(),
    ));
    url
}

#[tokio::test]
async fn an_authorised_client_can_handshake_and_ping() {
    let url = start_server("secret").await;
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", "Bearer secret".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();

    let calls = [
        json!({"jsonrpc":"2.0","id":1,"method":"system.handshake","params":{
            "client":{"name":"test","version":"0"},"protocol":PROTOCOL_VERSION}}),
        json!({"jsonrpc":"2.0","id":2,"method":"system.ping"}),
    ];
    for call in calls {
        socket
            .send(Message::Text(call.to_string().into()))
            .await
            .unwrap();
        let reply = socket.next().await.unwrap().unwrap();
        let response: Response = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        assert!(
            matches!(response.outcome, Outcome::Result(_)),
            "{response:?}"
        );
    }
}

#[tokio::test]
async fn a_browser_client_can_send_the_token_as_a_subprotocol() {
    let url = start_server("secret").await;
    let mut request = url.clone().into_client_request().unwrap();
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        "imperium.v0, imperium.token.secret".parse().unwrap(),
    );
    let (_, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "imperium.v0");

    let mut wrong = url.into_client_request().unwrap();
    wrong.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        "imperium.v0, imperium.token.wrong".parse().unwrap(),
    );
    match tokio_tungstenite::connect_async(wrong).await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected 401, got {other:?}"),
    }
}

#[tokio::test]
async fn a_client_without_the_token_is_refused() {
    let url = start_server("secret").await;
    for header in [None, Some("Bearer wrong")] {
        let mut request = url.clone().into_client_request().unwrap();
        if let Some(header) = header {
            request
                .headers_mut()
                .insert("Authorization", header.parse().unwrap());
        }
        match tokio_tungstenite::connect_async(request).await {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 401),
            other => panic!("expected 401, got {other:?}"),
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Connects with the token and handshakes.
async fn client(url: &str) -> Socket {
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", "Bearer secret".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let hello = json!({"jsonrpc":"2.0","id":0,"method":"system.handshake","params":{
        "client":{"name":"test","version":"0"},"protocol":PROTOCOL_VERSION}});
    socket
        .send(Message::Text(hello.to_string().into()))
        .await
        .unwrap();
    socket.next().await.unwrap().unwrap();
    socket
}

/// The next frame as JSON.
async fn next_json(socket: &mut Socket) -> serde_json::Value {
    let frame = socket.next().await.unwrap().unwrap();
    serde_json::from_str(frame.to_text().unwrap()).unwrap()
}

fn call(id: u32, method: &str, params: serde_json::Value) -> Message {
    Message::Text(
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
            .to_string()
            .into(),
    )
}

#[tokio::test]
async fn resource_snapshots_reach_only_subscribed_connections() {
    let url = start_server("secret").await;
    let mut watcher = client(&url).await;
    let mut bystander = client(&url).await;

    watcher
        .send(call(1, "system.resources", json!({})))
        .await
        .unwrap();
    let snapshot = next_json(&mut watcher).await;
    assert!(
        snapshot["result"]["memory"]["ram_total_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        snapshot["result"]["local_models"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    watcher
        .send(call(2, "system.resources_subscribe", json!({"on": true})))
        .await
        .unwrap();
    // The reply and the first snapshot, in either order.
    let mut seen = Vec::new();
    while seen.len() < 2 {
        let message = next_json(&mut watcher).await;
        seen.push(
            message["method"]
                .as_str()
                .map_or_else(|| format!("reply {}", message["id"]), str::to_string),
        );
    }
    assert!(
        seen.contains(&"system.resources_changed".to_string()),
        "{seen:?}"
    );
    assert!(seen.contains(&"reply 2".to_string()), "{seen:?}");

    // The bystander's next frame is the reply to its own call, not a snapshot.
    bystander
        .send(call(3, "models.unload", json!({"model": "none"})))
        .await
        .unwrap();
    let reply = next_json(&mut bystander).await;
    assert_eq!(reply["id"], 3, "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not loaded")
    );
}
