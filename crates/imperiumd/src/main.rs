use std::fs::{File, TryLockError};
use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use imperium_config::{KeychainSecrets, Paths, SecretStore};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperiumd::{AccessToken, DaemonState, EndpointFile};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tracing_subscriber::EnvFilter;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("IMPERIUM_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();
    // One worker per core (Tokio's default): every agent is a task, so busy agents spread
    // across cores while idle ones cost nothing (see docs/02-architecture.md).
    tokio::runtime::Builder::new_multi_thread()
        .thread_name("imperiumd-worker")
        .enable_all()
        .build()
        .context("could not start the I/O runtime")?
        .block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let settings = imperium_config::load(&paths, None, std::env::vars())?.settings()?;
    std::fs::create_dir_all(paths.data_dir())
        .with_context(|| format!("could not create {}", paths.data_dir().display()))?;
    let _lock = acquire_single_instance_lock(&paths)?;

    let listener = TcpListener::bind(SocketAddr::from((
        Ipv4Addr::LOCALHOST,
        settings.daemon.port,
    )))
    .await
    .with_context(|| format!("could not listen on port {}", settings.daemon.port))?;
    let address = listener.local_addr()?;
    let token = AccessToken::generate().context("could not generate an access token")?;
    let _endpoint = EndpointFile::write(
        &paths.daemon_endpoint_file(),
        &DaemonEndpoint {
            url: format!("ws://{address}/rpc"),
            token: token.as_str().to_string(),
            pid: std::process::id(),
            protocol: PROTOCOL_VERSION,
        },
    )
    .context("could not write the endpoint file")?;
    tracing::info!(%address, protocol = %PROTOCOL_VERSION, "imperiumd listening");

    let (stop, stopped) = oneshot::channel();
    let local = imperiumd::LocalRuntime::start(&paths, &settings);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeychainSecrets);
    let daemon = DaemonState::with_secrets(paths.clone(), settings, local, secrets);
    daemon.start_pets();
    let resumed = daemon.resume_sessions();
    if resumed > 0 {
        tracing::info!(
            sessions = resumed,
            "continued sessions from before the restart"
        );
    }
    if let Some(address) = daemon
        .start_phone()
        .await
        .context("could not start the phone listener")?
    {
        tracing::info!(%address, "phones can connect");
    }
    daemon.start_project_agents();
    daemon.start_scheduler();
    daemon.start_autofix(std::sync::Arc::new(imperium_github::GhCli));
    let server = tokio::spawn(imperiumd::serve(listener, token, daemon.clone(), async {
        stopped.await.ok();
    }));
    // Ctrl+C, or a client's `system.shutdown` (the desktop app's Stop, `imperium daemon stop`).
    tokio::select! {
        signal = tokio::signal::ctrl_c() => signal.context("could not listen for Ctrl+C")?,
        () = daemon.shutdown_requested() => {}
    }
    tracing::info!("shutting down");
    stop.send(()).ok();
    // Open WebSocket connections can hold graceful shutdown; stop waiting after a grace period.
    match tokio::time::timeout(SHUTDOWN_GRACE, server).await {
        Ok(joined) => joined.context("server task failed")??,
        Err(_) => tracing::warn!("connections still open after the grace period; exiting"),
    }
    Ok(())
}

fn acquire_single_instance_lock(paths: &Paths) -> anyhow::Result<File> {
    let path = paths.daemon_lock_file();
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => bail!("imperiumd is already running for this user"),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("could not lock {}", path.display()))
        }
    }
}
