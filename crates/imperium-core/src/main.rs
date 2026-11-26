use std::fs::{File, TryLockError};
use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use imperium_config::{AutoSecrets, Paths, SecretBackend, SecretStore};
use imperium_core::{AccessToken, DaemonState, EndpointFile};
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How long a shared daemon (`--exit-when-idle`) waits with no client and no work by default:
/// long enough that one command after another reuses it.
const IDLE_GRACE: Duration = Duration::from_secs(10);
/// How long shutdown waits for sessions' background commands to be killed.
const BACKGROUND_STOP_WAIT: Duration = Duration::from_secs(5);

fn main() -> anyhow::Result<()> {
    imperium_core::run_pty_supervisor_if_asked();
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
    let idle_grace = exit_when_idle(std::env::args().skip(1))?;
    let paths = Paths::from_env()?;
    let config = imperium_config::load(&paths, None, std::env::vars())?;
    for warning in config.warnings() {
        tracing::warn!("{warning}");
    }
    let settings = config.settings()?;
    std::fs::create_dir_all(paths.data_dir())
        .with_context(|| format!("could not create {}", paths.data_dir().display()))?;
    let _lock = acquire_single_instance_lock(&paths)?;
    end_children_with_daemon();
    // Windows: the NO_READ_UP labels code mode's Low-integrity worker needs, in the background.
    imperium_core::code_mode::label_folders(&paths, &settings.workspaces.root(&paths));

    let listener = TcpListener::bind(SocketAddr::from((
        Ipv4Addr::LOCALHOST,
        settings.daemon.port,
    )))
    .await
    .with_context(|| format!("could not listen on port {}", settings.daemon.port))?;
    let address = listener.local_addr()?;
    // A command in the OS sandbox must not reach the daemon (see `imperium_tools::hide_loopback_port`).
    imperium_tools::hide_loopback_port(address.port());
    let token = AccessToken::generate().context("could not generate an access token")?;
    // `daemon.socket` (on by default): the same protocol on a socket only this user can open. It
    // is bound after the single-instance lock, so a leftover socket from a crash is safe to
    // replace. A socket that cannot be opened is logged rather than fatal: the loopback listener
    // still serves the app, only evaluation runs and plugin approvals need the socket.
    let ipc = if settings.daemon.socket {
        match imperium_ipc::Listener::bind(paths.data_dir()) {
            Ok(listener) => {
                tracing::info!(socket = %listener.address(), "listening on a local socket");
                Some(listener)
            }
            Err(error) => {
                tracing::warn!(%error, data_dir = %paths.data_dir().display(), "could not open the local socket; evaluation runs and plugin approvals need it");
                None
            }
        }
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
    let auto_secrets =
        AutoSecrets::open_daemon(&paths).context("could not open the secret store")?;
    match auto_secrets.backend() {
        SecretBackend::Keychain => match auto_secrets.migrate() {
            Ok(report) if report.moved > 0 => tracing::info!(
                moved = report.moved,
                "moved secrets from the encrypted file into the OS keychain"
            ),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "could not move the encrypted secret file into the keychain")
            }
        },
        SecretBackend::EncryptedFile => tracing::warn!("{}", auto_secrets.fallback_warning()),
    }
    let secrets: Arc<dyn SecretStore> = Arc::new(auto_secrets);
    let daemon = DaemonState::with_secrets(paths.clone(), settings, local, secrets);
    daemon.remember_config(&config);
    daemon.start_pets();
    daemon.reconcile_workspaces();
    // After the working folders are in place, before sessions resume and read `Memory.md`.
    daemon.migrate_memory();
    // Before any session loads plugins: records what is installed so nothing that runs today stops.
    daemon.migrate_plugins();
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
        imperium_tools::hide_loopback_port(address.port());
        tracing::info!(%address, "phones can connect");
    }
    match daemon.start_openai().await {
        Ok(Some(address)) => {
            imperium_tools::hide_loopback_port(address.port());
            tracing::info!(%address, "the OpenAI-compatible API is on");
        }
        Ok(None) => {}
        // The API is optional: the daemon runs without it, and `api.get` says why.
        Err(error) => tracing::error!(%error, "the OpenAI-compatible API did not start"),
    }
    daemon.start_load_monitor();
    daemon.start_project_agents();
    daemon.start_initiative();
    // After the agents started, so their sessions are known.
    daemon.sweep_orphan_scratch();
    daemon.start_scheduler();
    // Pull-request data over the API with `gh`'s token; failed-check logs still through `gh`.
    match imperium_core::daemon_github() {
        Ok(github) => daemon.start_autofix(std::sync::Arc::new(github)),
        Err(error) => tracing::warn!(%error, "GitHub auto-fix did not start"),
    }
    daemon.start_auto_archive();
    daemon.start_terminal_sweeper();
    daemon.start_session_index();
    // A daemon a client started for itself exits when no client and no work uses it. The clock
    // starts here, after the slow part of starting, so a client that is waiting has its grace.
    if let Some(grace) = idle_grace {
        tracing::info!(
            grace_s = grace.as_secs(),
            "exits when no client or work uses it"
        );
    }
    daemon.start_lifetime(idle_grace);
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
    // Background commands and terminals must not outlive the daemon; both end at once, within
    // the same wait.
    tokio::join!(
        daemon.stop_background_processes(BACKGROUND_STOP_WAIT),
        daemon.stop_terminals(BACKGROUND_STOP_WAIT),
    );
    if tokio::time::timeout(BACKGROUND_STOP_WAIT, daemon.stop_speech())
        .await
        .is_err()
    {
        tracing::warn!("the local speech server did not stop in time");
    }
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

/// On Windows, puts the daemon in a job that ends every process in it when the daemon goes, by
/// whatever means (a crash, Task Manager, a forced stop): its servers, commands and MCP servers
/// then never outlive it. The job is nested in any the daemon was started in, and the jobs that
/// background commands make for themselves nest in this one. Elsewhere the servers are written
/// to a ledger and the next daemon ends what a killed one left (`imperium_local::ledger`).
fn end_children_with_daemon() {
    #[cfg(windows)]
    {
        let mut limits = win32job::ExtendedLimitInfo::new();
        limits.limit_kill_on_job_close();
        // What the daemon opens for the user (a folder in Explorer) breaks away from the job
        // with `CREATE_BREAKAWAY_FROM_JOB` and outlives the daemon; everything else stays in.
        limits.limit_breakaway_ok();
        match win32job::Job::create_with_limit_info(&limits)
            .and_then(|job| job.assign_current_process().map(|()| job))
        {
            // Closing the job is what ends the children: keep it open until the process ends.
            Ok(job) => std::mem::forget(job),
            Err(error) => {
                tracing::warn!(%error, "could not tie the daemon's children to it");
            }
        }
    }
}

/// How long a daemon started with `--exit-when-idle[=SECONDS]` waits with no client and no work
/// before it exits (the default is 10 seconds); `None` for a daemon that runs until stopped.
fn exit_when_idle(args: impl IntoIterator<Item = String>) -> anyhow::Result<Option<Duration>> {
    let mut grace = None;
    for arg in args {
        let seconds = match arg.strip_prefix("--exit-when-idle") {
            Some("") => {
                grace = Some(IDLE_GRACE);
                continue;
            }
            Some(rest) => rest.strip_prefix('='),
            None => None,
        };
        let Some(seconds) = seconds else {
            tracing::warn!(%arg, "ignoring an argument imperium-core does not take");
            continue;
        };
        let seconds: u64 = seconds
            .parse()
            .with_context(|| format!("{arg}: the grace period is a whole number of seconds"))?;
        grace = Some(Duration::from_secs(seconds.max(1)));
    }
    Ok(grace)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Option<Duration>> {
        exit_when_idle(args.iter().map(ToString::to_string))
    }

    #[test]
    fn a_daemon_runs_until_stopped_unless_asked_to_exit_when_idle() {
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(
            parse(&["--exit-when-idle"]).unwrap(),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            parse(&["--exit-when-idle=30"]).unwrap(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse(&["--exit-when-idle=0"]).unwrap(),
            Some(Duration::from_secs(1)),
            "a grace period is at least a second"
        );
        assert!(parse(&["--exit-when-idle=soon"]).is_err());
    }
}
