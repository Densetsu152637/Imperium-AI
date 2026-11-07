use futures_util::{SinkExt, StreamExt};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperiumd::AccessToken;
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
    let daemon = imperiumd::DaemonState::new(
        imperium_config::Paths::under(std::path::Path::new("unused")),
        settings,
    );
    tokio::spawn(imperiumd::serve(
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
