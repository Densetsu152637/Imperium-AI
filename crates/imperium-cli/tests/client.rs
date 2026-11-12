use std::sync::Arc;

use axum::Router;
use axum::routing::post;
use imperium_cli::client::{Client, ClientError, read_endpoint};
use imperium_config::{Config, Layer, Paths, Source};
use imperium_core::{AccessToken, DaemonState};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperium_protocol::methods::{
    Peer, Ping, PingParams, SessionCreate, SessionCreateParams, SessionSend, SessionSendParams,
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
async fn a_wrong_token_fails_to_connect() {
    let data = tempfile::tempdir().unwrap();
    let mut endpoint = start_daemon(daemon_with("", data.path())).await;
    endpoint.token = "not-the-token".into();
    let error = Client::connect(&endpoint, me()).await.unwrap_err();
    assert!(matches!(error, ClientError::Connect(_)), "{error}");
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
