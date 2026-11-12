use std::sync::Arc;

use axum::Router;
use axum::routing::post;
use imperium_cli::client::{Client, ClientError, read_endpoint};
use imperium_config::{Config, Layer, Paths, Source};
use imperium_core::{AccessToken, DaemonState};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperium_protocol::methods::{
    EventsSubscribe, EventsSubscribeParams, Peer, Ping, PingParams, SessionCreate,
    SessionCreateParams, SessionSend, SessionSendParams,
};
use imperium_protocol::session::{SESSION_EVENT, SessionEvent, SessionEventParams};
use tokio::net::TcpListener;

fn me() -> Peer {
    Peer {
        name: "test".into(),
        version: "0".into(),
    }
}

async fn start_daemon(daemon: Arc<DaemonState>) -> DaemonEndpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token = AccessToken::generate().unwrap();
    let endpoint = DaemonEndpoint {
        url: format!("ws://{}/rpc", listener.local_addr().unwrap()),
        token: token.as_str().to_string(),
        pid: std::process::id(),
        socket: None,
        protocol: PROTOCOL_VERSION,
    };
    tokio::spawn(imperium_core::serve(
        listener,
        token,
        daemon,
        std::future::pending(),
    ));
    endpoint
}

fn daemon_with(config: &str, data: &std::path::Path) -> Arc<DaemonState> {
    let settings = Config::from_layers([
        Layer::defaults(),
        Layer::new(Source::User("test".into()), config.parse().unwrap()),
    ])
    .settings()
    .unwrap();
    DaemonState::new(Paths::under(data), settings, None)
}

#[tokio::test]
async fn ping_round_trips_through_the_daemon() {
    let data = tempfile::tempdir().unwrap();
    let endpoint = start_daemon(daemon_with("", data.path())).await;
    let mut client = Client::connect(&endpoint, me()).await.unwrap();
    assert_eq!(client.daemon().daemon.name, "imperium-core");
    assert_eq!(client.daemon().protocol, PROTOCOL_VERSION);
    client.call::<Ping>(PingParams {}).await.unwrap();
}

#[tokio::test]
async fn a_chat_turn_streams_events_to_the_client() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                [("content-type", "text/event-stream")],
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Hi there\"}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n",
                    "data: [DONE]\n\n",
                ),
            )
        }),
    );
    let model_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", model_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let config =
        format!("[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n");
    let endpoint = start_daemon(daemon_with(&config, data.path())).await;
    let mut client = Client::connect(&endpoint, me()).await.unwrap();

    let session = client
        .call::<SessionCreate>(SessionCreateParams {
            environment_id: None,
            project_dir: Some(project.path().to_string_lossy().into_owned()),
            model: None,
            ..SessionCreateParams::default()
        })
        .await
        .unwrap();
    assert_eq!(session.model.as_deref(), Some("fake:m"));
    client
        .call::<SessionSend>(SessionSendParams {
            session_id: session.session_id.clone(),
            text: "hello".into(),
            mode: None,
            agent_id: None,
            confirm_budget: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();

    let mut text = String::new();
    loop {
        let notification = client.next_notification().await.unwrap();
        // Daemon-wide notices (such as session.created) arrive too.
        if notification.method != SESSION_EVENT {
            continue;
        }
        let params: SessionEventParams = serde_json::from_value(notification.params).unwrap();
        assert_eq!(params.session_id, session.session_id);
        match params.agent_event.event {
            SessionEvent::TextDelta { text: delta } => text.push_str(&delta),
            SessionEvent::TurnCompleted { usage, .. } => {
                assert_eq!(usage.prompt_tokens, 9);
                break;
            }
            SessionEvent::TurnFailed { message } => panic!("turn failed: {message}"),
            _ => {}
        }
    }
    assert_eq!(text, "Hi there");
    let journal = data
        .path()
        .join("data/sessions")
        .join(format!("{}.jsonl", session.session_id));
    assert!(journal.exists());
}

#[tokio::test]
async fn a_topic_choice_stops_and_restores_session_events() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                [("content-type", "text/event-stream")],
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Hi there\"}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n",
                    "data: [DONE]\n\n",
                ),
            )
        }),
    );
    let model_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", model_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let config =
        format!("[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n");
    let endpoint = start_daemon(daemon_with(&config, data.path())).await;
    let mut client = Client::connect(&endpoint, me()).await.unwrap();
    let session = client
        .call::<SessionCreate>(SessionCreateParams {
            environment_id: None,
            project_dir: Some(project.path().to_string_lossy().into_owned()),
            model: None,
            ..SessionCreateParams::default()
        })
        .await
        .unwrap();
    let send = |text: &str| SessionSendParams {
        session_id: session.session_id.clone(),
        text: text.into(),
        mode: None,
        agent_id: None,
        confirm_budget: None,
        attachments: Vec::new(),
    };

    // Subscribed to the session, then narrowed to another topic: its events stop.
    client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            topics: vec!["model.*".into()],
        })
        .await
        .unwrap();
    client.call::<SessionSend>(send("first")).await.unwrap();
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(1500), async {
        loop {
            let notification = client.next_notification().await.unwrap();
            if notification.method == SESSION_EVENT {
                return notification;
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "a session event arrived: {quiet:?}");

    // Subscribing to the topic brings them back.
    client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            topics: vec!["session.event".into()],
        })
        .await
        .unwrap();
    client.call::<SessionSend>(send("second")).await.unwrap();
    loop {
        let notification = client.next_notification().await.unwrap();
        if notification.method != SESSION_EVENT {
            continue;
        }
        let params: SessionEventParams = serde_json::from_value(notification.params).unwrap();
        if matches!(params.agent_event.event, SessionEvent::TurnCompleted { .. }) {
            break;
        }
    }
}

#[tokio::test]
async fn a_wrong_token_fails_to_connect() {
    let data = tempfile::tempdir().unwrap();
    let mut endpoint = start_daemon(daemon_with("", data.path())).await;
    endpoint.token = "not-the-token".into();
    let error = Client::connect(&endpoint, me()).await.unwrap_err();
    assert!(matches!(error, ClientError::Connect(_)), "{error}");
}

/// Serves the daemon on its local socket only, returning an endpoint with no TCP URL.
async fn start_socket_daemon(data: &std::path::Path) -> DaemonEndpoint {
    let listener = imperium_ipc::Listener::bind(data).unwrap();
    let token = AccessToken::generate().unwrap();
    let endpoint = DaemonEndpoint {
        url: String::new(),
        token: token.as_str().to_string(),
        pid: std::process::id(),
        socket: Some(listener.address()),
        protocol: PROTOCOL_VERSION,
    };
    tokio::spawn(imperium_core::serve_ipc(
        listener,
        token,
        daemon_with("", data),
        std::future::pending(),
    ));
    endpoint
}

#[tokio::test]
async fn the_client_connects_over_the_local_socket() {
    let data = tempfile::tempdir().unwrap();
    let endpoint = start_socket_daemon(data.path()).await;
    let mut client = Client::connect(&endpoint, me()).await.unwrap();
    assert_eq!(client.daemon().daemon.name, "imperium-core");
    client.call::<Ping>(PingParams {}).await.unwrap();

    // The token is checked there too.
    let wrong = DaemonEndpoint {
        token: "wrong".into(),
        ..endpoint.clone()
    };
    assert!(matches!(
        Client::connect(&wrong, me()).await,
        Err(ClientError::Connect(_))
    ));
}

#[cfg(windows)]
#[tokio::test]
async fn the_client_will_not_send_the_token_to_a_pipe_of_another_process() {
    let data = tempfile::tempdir().unwrap();
    let mut endpoint = start_socket_daemon(data.path()).await;
    endpoint.pid = endpoint.pid.wrapping_add(4);
    assert!(Client::connect(&endpoint, me()).await.is_err());
}

#[tokio::test]
async fn an_unreachable_socket_falls_back_to_the_loopback_url() {
    let data = tempfile::tempdir().unwrap();
    let mut endpoint = start_daemon(daemon_with("", data.path())).await;
    endpoint.socket = Some(
        data.path()
            .join("missing.sock")
            .to_string_lossy()
            .into_owned(),
    );
    let mut client = Client::connect(&endpoint, me()).await.unwrap();
    client.call::<Ping>(PingParams {}).await.unwrap();

    // With no URL to fall back on, the socket's own error is returned.
    endpoint.url = String::new();
    assert!(Client::connect(&endpoint, me()).await.is_err());
}

#[test]
fn a_missing_endpoint_file_means_the_daemon_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let error = read_endpoint(&dir.path().join("daemon.json")).unwrap_err();
    assert!(
        matches!(
            error,
            imperium_protocol::discovery::EndpointError::NotRunning(_)
        ),
        "{error}"
    );
}
