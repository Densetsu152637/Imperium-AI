//! The shared daemon lifetime (docs/02-architecture.md#shared-daemon-lifetime-implemented): a
//! daemon a client started exits when no connection holds a lease and no work keeps it up.
//!
//! These run an in-process daemon with a short grace period, and ask `shutdown_requested`
//! whether it decided to exit; the last test starts the real `imperium-core` binary.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_client::launch::{Launch, LeaseRequest, attach};
use imperium_client::{Client, ClientError};
use imperium_config::{Config, Layer, Paths, Source};
use imperium_core::{AccessToken, DaemonState};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperium_protocol::jsonrpc::codes;
use imperium_protocol::methods::{
    LeaseKind, LifetimeMode, ModelDownload, ModelDownloadParams, Peer, Ping, PingParams,
    SessionCreate, SessionCreateParams, SessionSend, SessionSendParams, Shutdown, ShutdownParams,
    SystemLifetime, SystemLifetimeParams, WorkKind,
};
use tokio::net::TcpListener;

const GRACE: Duration = Duration::from_millis(300);

fn me() -> Peer {
    Peer {
        name: "test".into(),
        version: "0".into(),
    }
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

async fn serve(daemon: &Arc<DaemonState>) -> DaemonEndpoint {
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
        Arc::clone(daemon),
        std::future::pending(),
    ));
    endpoint
}

/// A client that holds a lease for as long as it lives.
async fn leased(endpoint: &DaemonEndpoint, label: &str, pid: Option<u32>) -> Client {
    let mut client = Client::connect(endpoint, me()).await.unwrap();
    client.lease(LeaseKind::Cli, label, pid).await.unwrap();
    client
}

/// Whether the daemon decides to exit within `within`.
async fn exits(daemon: &DaemonState, within: Duration) -> bool {
    tokio::time::timeout(within, daemon.shutdown_requested())
        .await
        .is_ok()
}

async fn wait_for(mut condition: impl FnMut() -> bool) {
    for _ in 0..400 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the condition never held");
}

#[tokio::test]
async fn the_daemon_stays_while_any_client_holds_a_lease_and_exits_after_the_last() {
    let data = tempfile::tempdir().unwrap();
    let daemon = daemon_with("", data.path());
    daemon.start_lifetime(Some(GRACE));
    let endpoint = serve(&daemon).await;

    let first = leased(&endpoint, "imperium chat", None).await;
    let second = leased(&endpoint, "imperium session list", None).await;
    assert!(!exits(&daemon, GRACE * 3).await, "two leases hold it");

    drop(first);
    assert!(!exits(&daemon, GRACE * 3).await, "one lease is left");
    let mut remaining = second;
    remaining.call::<Ping>(PingParams {}).await.unwrap();

    drop(remaining);
    assert!(
        exits(&daemon, Duration::from_secs(10)).await,
        "no lease and no work: it exits"
    );
}

#[tokio::test]
async fn a_persistent_daemon_never_exits_for_want_of_clients() {
    let data = tempfile::tempdir().unwrap();
    let daemon = daemon_with("", data.path());
    daemon.start_lifetime(None);
    let endpoint = serve(&daemon).await;
    let client = leased(&endpoint, "imperium chat", None).await;
    drop(client);
    assert!(!exits(&daemon, Duration::from_millis(1500)).await);
}

#[tokio::test]
async fn a_lease_whose_process_is_gone_is_reaped_though_its_connection_lingers() {
    let data = tempfile::tempdir().unwrap();
    let daemon = daemon_with("", data.path());
    let alive = Arc::new(AtomicBool::new(true));
    let probe = Arc::clone(&alive);
    daemon.use_pid_probe(Arc::new(move |_| probe.load(Ordering::SeqCst)));
    daemon.start_lifetime(Some(GRACE));
    let endpoint = serve(&daemon).await;

    let mut ghost = leased(&endpoint, "imperium chat", Some(4242)).await;
    assert!(!exits(&daemon, GRACE * 3).await, "its process is alive");

    // The process dies; its socket does not close (a hang, or a connection a crash left).
    alive.store(false, Ordering::SeqCst);
    assert!(
        exits(&daemon, Duration::from_secs(10)).await,
        "the lease was reaped"
    );
    ghost.call::<Ping>(PingParams {}).await.unwrap();
}

#[tokio::test]
async fn a_client_is_turned_away_once_the_daemon_has_decided_to_exit() {
    let data = tempfile::tempdir().unwrap();
    let daemon = daemon_with("", data.path());
    daemon.start_lifetime(Some(GRACE));
    let endpoint = serve(&daemon).await;
    assert!(exits(&daemon, Duration::from_secs(10)).await);

    let mut late = Client::connect(&endpoint, me()).await.unwrap();
    match late.lease(LeaseKind::Cli, "late", None).await {
        Err(ClientError::Rpc(error)) => assert_eq!(error.code, codes::DAEMON_STOPPING),
        other => panic!("expected the daemon to refuse the lease, got {other:?}"),
    }
}

#[tokio::test]
async fn lifetime_reports_leases_and_chooses_persistence() {
    let data = tempfile::tempdir().unwrap();
    let daemon = daemon_with("", data.path());
    let endpoint = serve(&daemon).await;
    let mut client = leased(&endpoint, "imperium chat", Some(std::process::id())).await;

    let status = client
        .call::<SystemLifetime>(SystemLifetimeParams::default())
        .await
        .unwrap();
    assert_eq!(status.mode, LifetimeMode::Persistent);
    assert_eq!(status.leases.len(), 1);
    assert_eq!(status.leases[0].kind, LeaseKind::Cli);
    assert_eq!(status.leases[0].label, "imperium chat");
    assert_eq!(status.leases[0].pid, Some(std::process::id()));
    assert!(status.work.is_empty());
    assert_eq!((status.idle_ms, status.exits_in_ms), (None, None));

    let shared = client
        .call::<SystemLifetime>(SystemLifetimeParams {
            persistent: Some(false),
        })
        .await
        .unwrap();
    assert_eq!(shared.mode, LifetimeMode::Shared);
}

#[tokio::test]
async fn an_enabled_schedule_holds_the_daemon_and_a_disabled_one_does_not() {
    let project = tempfile::tempdir().unwrap();
    let project_path = project.path().display().to_string().replace('\\', "/");
    let config = |enabled: bool| {
        format!(
            r#"
            [[schedules]]
            id = "nightly"
            enabled = {enabled}
            trigger = {{ cron = "0 3 * * *" }}
            action = {{ project = "{project_path}", command = "echo nightly" }}
            "#
        )
    };

    let data = tempfile::tempdir().unwrap();
    let daemon = daemon_with(&config(true), data.path());
    daemon.start_scheduler();
    daemon.start_lifetime(Some(GRACE));
    let work = daemon.lifetime_status().work;
    assert_eq!(work.len(), 1, "{work:?}");
    assert_eq!(work[0].kind, WorkKind::Schedule);
    assert_eq!(work[0].detail, "nightly");
    assert!(
        !exits(&daemon, GRACE * 5).await,
        "a schedule with a run to come holds it with no client"
    );

    let data = tempfile::tempdir().unwrap();
    let idle = daemon_with(&config(false), data.path());
    idle.start_scheduler();
    idle.start_lifetime(Some(GRACE));
    assert!(idle.lifetime_status().work.is_empty());
    assert!(exits(&idle, Duration::from_secs(10)).await);
}

/// A model that answers after `delay`, so a turn is running for a while.
async fn slow_model(delay: Duration) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || async move {
            tokio::time::sleep(delay).await;
            (
                [("content-type", "text/event-stream")],
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Done.\"}}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n",
                    "data: [DONE]\n\n",
                ),
            )
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
async fn a_running_turn_holds_the_daemon_until_it_ends() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let base_url = slow_model(Duration::from_secs(3)).await;
    let config =
        format!("[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n");
    let daemon = daemon_with(&config, data.path());
    daemon.start_lifetime(Some(Duration::from_millis(500)));
    let endpoint = serve(&daemon).await;

    // One client holds the daemon while another starts the turn, then goes.
    let holder = leased(&endpoint, "imperium chat", None).await;
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
    client
        .call::<SessionSend>(SessionSendParams {
            session_id: session.session_id,
            text: "hello".into(),
            mode: None,
            agent_id: None,
            confirm_budget: None,
            attachments: Vec::new(),
        })
        .await
        .unwrap();
    wait_for(|| {
        daemon
            .lifetime_status()
            .work
            .iter()
            .any(|work| work.kind == WorkKind::Turn)
    })
    .await;
    drop(holder);

    assert!(
        !exits(&daemon, Duration::from_millis(1500)).await,
        "the turn holds it with no lease"
    );
    assert!(
        exits(&daemon, Duration::from_secs(20)).await,
        "it exits after the turn has ended and the grace period has passed"
    );
}

/// A Hub whose one file, `tiny.gguf`, takes `delay` to arrive.
async fn slow_hub(delay: Duration) -> String {
    use sha2::{Digest, Sha256};
    let mut body = b"GGUF".to_vec();
    body.extend(3u32.to_le_bytes());
    body.extend(0u64.to_le_bytes());
    body.extend(0u64.to_le_bytes());
    let listed = body.clone();
    let tree = move || {
        let body = listed.clone();
        async move {
            axum::Json(serde_json::json!([{"type": "file", "path": "tiny.gguf",
                "size": body.len(),
                "lfs": {"oid": imperium_api::encoding::hex(&Sha256::digest(&body))}}]))
        }
    };
    let resolve = move || {
        let body = body.clone();
        async move {
            tokio::time::sleep(delay).await;
            body
        }
    };
    let app: Router = Router::new()
        .route(
            "/api/models/{owner}/{name}/tree/{revision}",
            axum::routing::get(tree),
        )
        .route(
            "/{owner}/{name}/resolve/{revision}/{*path}",
            axum::routing::get(resolve),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
async fn a_running_download_holds_the_daemon_until_it_ends() {
    let data = tempfile::tempdir().unwrap();
    let hub = slow_hub(Duration::from_secs(3)).await;
    let daemon = daemon_with("", data.path());
    daemon.use_hub(imperium_local::hf::Hub::new(hub, None));
    daemon.start_lifetime(Some(Duration::from_millis(500)));
    let endpoint = serve(&daemon).await;

    let holder = leased(&endpoint, "imperium models pull", None).await;
    let mut client = Client::connect(&endpoint, me()).await.unwrap();
    client
        .call::<ModelDownload>(ModelDownloadParams {
            repo: "acme/tiny-GGUF".into(),
            quant: None,
            file: Some("tiny.gguf".into()),
            revision: None,
            mmproj: false,
        })
        .await
        .unwrap();
    wait_for(|| {
        daemon
            .lifetime_status()
            .work
            .iter()
            .any(|work| work.kind == WorkKind::Job)
    })
    .await;
    drop(holder);

    assert!(
        !exits(&daemon, Duration::from_millis(1500)).await,
        "the download holds it with no lease"
    );
    assert!(
        exits(&daemon, Duration::from_secs(20)).await,
        "it exits after the download has ended and the grace period has passed"
    );
}

#[tokio::test]
async fn a_failed_turn_is_not_a_request_a_phone_can_answer() {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    // A model that refuses every request, so the turn fails.
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async { (axum::http::StatusCode::BAD_REQUEST, "no") }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n[phone]\nbind = \"127.0.0.1:0\"\n"
    );
    let daemon = daemon_with(&config, data.path());
    daemon.start_phone().await.unwrap();
    // A paired device that may approve.
    let code = daemon.devices().new_code();
    daemon.devices().pair(&code.code, "phone").unwrap();
    daemon.start_lifetime(Some(Duration::from_millis(500)));
    let endpoint = serve(&daemon).await;

    let holder = leased(&endpoint, "imperium chat", None).await;
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
    let live = daemon.session(&session.session_id).unwrap();
    wait_for(|| live.status().0 == imperium_protocol::session::SessionStatus::NeedsAttention).await;
    assert!(!live.is_asking());
    drop(holder);

    assert!(
        daemon
            .lifetime_status()
            .work
            .iter()
            .all(|work| work.kind != WorkKind::Approval),
        "a failed turn is not waiting on anyone"
    );
    assert!(
        exits(&daemon, Duration::from_secs(20)).await,
        "the daemon exits though a phone that may approve is paired"
    );
}

#[tokio::test]
async fn an_explicit_stop_refuses_new_leases() {
    let data = tempfile::tempdir().unwrap();
    let daemon = daemon_with("", data.path());
    let endpoint = serve(&daemon).await;
    let mut stopper = Client::connect(&endpoint, me()).await.unwrap();
    stopper.call::<Shutdown>(ShutdownParams {}).await.unwrap();
    let mut late = Client::connect(&endpoint, me()).await.unwrap();
    match late.lease(LeaseKind::Cli, "late", None).await {
        Err(ClientError::Rpc(error)) => assert_eq!(error.code, codes::DAEMON_STOPPING),
        other => panic!("expected the stopping daemon to refuse the lease, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_that_start_the_daemon_at_once_share_one() {
    let home = tempfile::tempdir().unwrap();
    let file = Paths::under(home.path()).daemon_endpoint_file();
    let mut launch = Launch::shared(
        PathBuf::from(env!("CARGO_BIN_EXE_imperium-core")),
        Some(Duration::from_secs(2)),
    );
    launch
        .envs
        .push(("IMPERIUM_HOME".into(), home.path().into()));
    launch.log = Some(home.path().join("imperium-core.log"));
    launch.timeout = Duration::from_secs(60);
    let launch = Arc::new(launch);

    const CLIENTS: u32 = 6;
    let attaching: Vec<_> = (0..CLIENTS)
        .map(|n| {
            let file = file.clone();
            let launch = Arc::clone(&launch);
            tokio::spawn(async move {
                let lease = LeaseRequest {
                    kind: LeaseKind::Cli,
                    label: format!("client {n}"),
                    pid: Some(std::process::id()),
                };
                attach(&file, me(), Some(lease), Some(&*launch)).await
            })
        })
        .collect();
    let mut attached = Vec::new();
    for attaching in attaching {
        let (client, endpoint) = attaching
            .await
            .unwrap()
            .unwrap_or_else(|error| panic!("a client could not attach: {error}"));
        attached.push((client, endpoint.pid));
    }
    let daemon_pid = attached[0].1;
    assert_ne!(daemon_pid, std::process::id());
    assert!(
        attached.iter().all(|(_, pid)| *pid == daemon_pid),
        "every client reached the same daemon: {:?}",
        attached.iter().map(|(_, pid)| pid).collect::<Vec<_>>()
    );

    let status = attached[0]
        .0
        .call::<SystemLifetime>(SystemLifetimeParams::default())
        .await
        .unwrap();
    assert_eq!(status.mode, LifetimeMode::Shared);
    assert_eq!(status.leases.len(), CLIENTS as usize);

    // The last client to go takes the daemon with it.
    drop(attached);
    for _ in 0..600 {
        if !file.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the daemon did not exit after the last client left");
}
