use imperium_cli::client::{Client, ClientError, read_endpoint};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperium_protocol::methods::{Peer, Ping, PingParams};
use imperiumd::AccessToken;
use tokio::net::TcpListener;

fn me() -> Peer {
    Peer {
        name: "test".into(),
        version: "0".into(),
    }
}

async fn start_daemon() -> DaemonEndpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token = AccessToken::generate().unwrap();
    let endpoint = DaemonEndpoint {
        url: format!("ws://{}/rpc", listener.local_addr().unwrap()),
        token: token.as_str().to_string(),
        pid: std::process::id(),
        protocol: PROTOCOL_VERSION,
    };
    tokio::spawn(imperiumd::serve(listener, token, std::future::pending()));
    endpoint
}

#[tokio::test]
async fn ping_round_trips_through_the_daemon() {
    let endpoint = start_daemon().await;
    let mut client = Client::connect(&endpoint, me()).await.unwrap();
    assert_eq!(client.daemon().daemon.name, "imperiumd");
    assert_eq!(client.daemon().protocol, PROTOCOL_VERSION);
    client.call::<Ping>(PingParams {}).await.unwrap();
    client.call::<Ping>(PingParams {}).await.unwrap();
}

#[tokio::test]
async fn a_wrong_token_fails_to_connect() {
    let mut endpoint = start_daemon().await;
    endpoint.token = "not-the-token".into();
    let error = Client::connect(&endpoint, me()).await.unwrap_err();
    assert!(matches!(error, ClientError::Connect(_)), "{error}");
}

#[test]
fn a_missing_endpoint_file_means_the_daemon_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let error = read_endpoint(&dir.path().join("daemon.json")).unwrap_err();
    assert!(matches!(error, ClientError::DaemonNotRunning(_)), "{error}");
}
