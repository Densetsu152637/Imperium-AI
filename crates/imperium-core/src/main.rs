use std::fs::{File, TryLockError};
use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use imperium_config::{KeychainSecrets, Paths, SecretStore};
use imperium_core::{AccessToken, DaemonState, EndpointFile};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How long shutdown waits for sessions' background commands to be killed.
const BACKGROUND_STOP_WAIT: Duration = Duration::from_secs(5);

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
        .thread_name("imperium-core-worker")
        .enable_all()
        .build()
        .context("could not start the I/O runtime")?
        .block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let paths = Paths::from_env()?;
    let config = imperium_config::load(&paths, None, std::env::vars())?;
    for warning in config.warnings() {
        tracing::warn!("{warning}");
    }
    let settings = config.settings()?;
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
    // Opt-in (`daemon.socket`): the same protocol on a socket only this user can open. It is
    // bound after the single-instance lock, so a leftover socket from a crash is safe to replace.
    let ipc = if settings.daemon.socket {
        let listener = imperium_ipc::Listener::bind(paths.data_dir())
            .context("could not open the local socket")?;
        tracing::info!(socket = %listener.address(), "listening on a local socket");
        Some(listener)
    } else {
        None
    };
    let _endpoint = EndpointFile::write(
        &paths.daemon_endpoint_file(),
        &DaemonEndpoint {
            url: format!("ws://{address}/rpc"),
            token: token.as_str().to_string(),
            pid: std::process::id(),
            protocol: PROTOCOL_VERSION,
            socket: ipc.as_ref().map(imperium_ipc::Listener::address),
        },
    )
    .context("could not write the endpoint file")?;
    tracing::info!(%address, protocol = %PROTOCOL_VERSION, "imperium-core listening");

    let (stop, _) = watch::channel(false);
    let local = imperium_core::LocalRuntime::start(&paths, &settings);
    let secrets: Arc<dyn SecretStore> = Arc::new(KeychainSecrets);
    let daemon = DaemonState::with_secrets(paths.clone(), settings, local, secrets);
    daemon.remember_config(&config);
    daemon.start_pets();
    daemon.reconcile_workspaces();
    // After the working folders are in place, before sessions resume and read `Memory.md`.
    daemon.migrate_memory();
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
    match daemon.start_openai().await {
        Ok(Some(address)) => tracing::info!(%address, "the OpenAI-compatible API is on"),
        Ok(None) => {}
        // The API is optional: the daemon runs without it, and `api.get` says why.
        Err(error) => tracing::error!(%error, "the OpenAI-compatible API did not start"),
    }
    daemon.start_load_monitor();
    daemon.start_project_agents();
    // After the agents started, so their sessions are known.
    daemon.sweep_orphan_scratch();
    daemon.start_scheduler();
    daemon.start_autofix(std::sync::Arc::new(imperium_github::GhCli));
    daemon.start_auto_archive();
    daemon.start_session_index();
    let ipc_server = ipc.map(|listener| {
        tokio::spawn(imperium_core::serve_ipc(
            listener,
            token.clone(),
            daemon.clone(),
            wait_for_stop(stop.subscribe()),
        ))
    });
    let server = tokio::spawn(imperium_core::serve(
        listener,
        token,
        daemon.clone(),
        wait_for_stop(stop.subscribe()),
    ));
    // Ctrl+C, or a client's `system.shutdown` (the desktop app's Stop, `imperium daemon stop`).
    tokio::select! {
        signal = tokio::signal::ctrl_c() => signal.context("could not listen for Ctrl+C")?,
        () = daemon.shutdown_requested() => {}
    }
    tracing::info!("shutting down");
    // Background commands must not outlive the daemon.
    daemon.stop_background_processes(BACKGROUND_STOP_WAIT).await;
    stop.send(true).ok();
    // Open WebSocket connections can hold graceful shutdown; stop waiting after a grace period.
    // The local socket's file is removed when its server ends.
    let both = async {
        if let Some(ipc_server) = ipc_server {
            ipc_server
                .await
                .context("local socket server task failed")??;
        }
        server.await.context("server task failed")??;
        anyhow::Ok(())
    };
    match tokio::time::timeout(SHUTDOWN_GRACE, both).await {
        Ok(result) => result?,
        Err(_) => tracing::warn!("connections still open after the grace period; exiting"),
    }
    Ok(())
}

/// Resolves when the daemon is told to stop (or its sender is gone, which means the same).
async fn wait_for_stop(mut signal: watch::Receiver<bool>) {
    let _ = signal.wait_for(|stopping| *stopping).await;
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
        Err(TryLockError::WouldBlock) => bail!("imperium-core is already running for this user"),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("could not lock {}", path.display()))
        }
    }
}
