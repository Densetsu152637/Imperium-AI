//! `events.subscribe` and `events.unsubscribe`: a connection chooses which notification topics
//! it receives, on top of what it may receive anyway.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use imperium_core::{AccessToken, Connection, DaemonState};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::jsonrpc::{Notification, Outcome, Response};
use imperium_protocol::methods::{MODEL_BENCH_PROGRESS, RESOURCES_CHANGED};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn daemon() -> Arc<DaemonState> {
    let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
        .settings()
        .unwrap();
    DaemonState::new(
        imperium_config::Paths::under(std::path::Path::new("unused")),
        settings,
        None,
    )
}

async fn connect(daemon: &Arc<DaemonState>) -> Socket {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut request = format!("ws://{}/rpc", listener.local_addr().unwrap())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", "Bearer secret".parse().unwrap());
    tokio::spawn(imperium_core::serve(
        listener,
        AccessToken::from("secret".to_string()),
        Arc::clone(daemon),
        std::future::pending(),
    ));
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    call(
        &mut socket,
        0,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": PROTOCOL_VERSION}),
    )
    .await;
    socket
}

/// Sends a request and returns its result, skipping notifications that arrive first.
async fn call(socket: &mut Socket, id: u64, method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        let value = next(socket).await;
        if value["id"] == id {
            return value;
        }
    }
}

async fn next(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("a message within 10 s")
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

/// Asserts that no notification arrives for a moment.
async fn nothing_arrives(socket: &mut Socket) {
    let waited = tokio::time::timeout(Duration::from_millis(300), socket.next()).await;
    assert!(waited.is_err(), "unexpected message: {waited:?}");
}

#[tokio::test]
async fn a_connection_receives_only_the_topics_it_subscribed_to() {
    let daemon = daemon();
    let mut socket = connect(&daemon).await;

    // By default every announcement arrives.
    daemon.announce("session.updated", json!({"n": 1}));
    assert_eq!(next(&mut socket).await["method"], "session.updated");

    // The first subscription narrows to the topics named.
    let reply = call(
        &mut socket,
        1,
        "events.subscribe",
        json!({"topics": ["model.residency"]}),
    )
    .await;
    assert_eq!(reply["result"]["topics"], json!(["model.residency"]));
    daemon.announce("session.updated", json!({"n": 2}));
    daemon.announce("model.residency", json!({"n": 3}));
    let arrived = next(&mut socket).await;
    assert_eq!(arrived["method"], "model.residency", "{arrived}");
    assert_eq!(arrived["params"]["n"], 3);

    // A prefix adds a family; unsubscribing a topic inside it mutes only that one.
    call(
        &mut socket,
        2,
        "events.subscribe",
        json!({"topics": ["session.*"]}),
    )
    .await;
    let reply = call(
        &mut socket,
        3,
        "events.unsubscribe",
        json!({"topics": ["session.updated"]}),
    )
    .await;
    assert_eq!(reply["result"]["muted"], json!(["session.updated"]));
    daemon.announce("session.updated", json!({"n": 4}));
    daemon.announce("session.created", json!({"n": 5}));
    assert_eq!(next(&mut socket).await["params"]["n"], 5);
    nothing_arrives(&mut socket).await;

    // `*` returns to everything.
    let reply = call(&mut socket, 4, "events.subscribe", json!({"topics": ["*"]})).await;
    assert_eq!(reply["result"]["topics"], Value::Null);
    daemon.announce("session.updated", json!({"n": 6}));
    assert_eq!(next(&mut socket).await["params"]["n"], 6);
}

#[tokio::test]
async fn one_connections_choice_does_not_touch_another() {
    let daemon = daemon();
    let mut narrow = connect(&daemon).await;
    let mut wide = connect(&daemon).await;
    call(
        &mut narrow,
        1,
        "events.subscribe",
        json!({"topics": ["model.residency"]}),
    )
    .await;
    daemon.announce("session.updated", json!({}));
    assert_eq!(next(&mut wide).await["method"], "session.updated");
    nothing_arrives(&mut narrow).await;
}

#[tokio::test]
async fn bad_topics_are_refused() {
    let daemon = daemon();
    let mut socket = connect(&daemon).await;
    let reply = call(
        &mut socket,
        1,
        "events.subscribe",
        json!({"topics": ["Not A Topic"]}),
    )
    .await;
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
    daemon.announce("session.updated", json!({}));
    assert_eq!(next(&mut socket).await["method"], "session.updated");
}

fn handshake(connection: &mut Connection) {
    let reply = connection.handle_text(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "system.handshake", "params": {
            "client": {"name": "test", "version": "0"}, "protocol": PROTOCOL_VERSION}})
        .to_string(),
    );
    assert!(matches!(
        reply.response.unwrap().outcome,
        Outcome::Result(_)
    ));
}

fn subscribe(connection: &mut Connection, topics: &[&str]) -> Response {
    connection
        .handle_text(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "events.subscribe",
                "params": {"topics": topics}})
            .to_string(),
        )
        .response
        .unwrap()
}

fn notice(method: &str) -> Notification {
    Notification::new(method, json!({}))
}

#[tokio::test]
async fn a_subscription_never_widens_what_a_connection_may_receive() {
    let daemon = daemon();

    // A restricted connection (a paired phone's kind) never gets benchmark progress, whatever
    // it subscribes to. Phones themselves may not subscribe; give this one the method to prove
    // the filter is layered, not trusted.
    let mut phone = Connection::restricted(Arc::clone(&daemon), vec!["events.subscribe"]);
    handshake(&mut phone);
    assert!(!phone.receives(&notice(MODEL_BENCH_PROGRESS)));
    assert!(matches!(
        subscribe(&mut phone, &[MODEL_BENCH_PROGRESS, "*"]).outcome,
        Outcome::Result(_)
    ));
    assert!(!phone.receives(&notice(MODEL_BENCH_PROGRESS)));
    assert!(phone.receives(&notice("session.updated")));

    // Resource snapshots still need `system.resources_subscribe`.
    let mut full = Connection::new(Arc::clone(&daemon));
    handshake(&mut full);
    assert!(matches!(
        subscribe(&mut full, &[RESOURCES_CHANGED]).outcome,
        Outcome::Result(_)
    ));
    assert!(!full.receives(&notice(RESOURCES_CHANGED)));
    assert!(!full.receives(&notice("session.updated")), "narrowed");
    // The unrestricted connection does get benchmark progress once it names it.
    subscribe(&mut full, &[MODEL_BENCH_PROGRESS]);
    assert!(full.receives(&notice(MODEL_BENCH_PROGRESS)));
}

#[tokio::test]
async fn a_restricted_connection_cannot_call_the_subscription_methods_unless_granted() {
    let daemon = daemon();
    let mut phone = Connection::restricted(Arc::clone(&daemon), vec!["session.list"]);
    handshake(&mut phone);
    let reply = subscribe(&mut phone, &["session.event"]);
    assert!(matches!(reply.outcome, Outcome::Error(error) if error.code == -32005));
}
