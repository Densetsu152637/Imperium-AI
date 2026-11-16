//! Finding the daemon for a client, starting it when none runs, and holding a lease on it
//! (docs/02-architecture.md#shared-daemon-lifetime-implemented).
//!
//! [`attach`] reads the endpoint file, connects, and registers a lease. When no daemon answers
//! and the caller allows it, it starts `imperium-core --exit-when-idle` detached from the
//! caller's console and waits for it to listen. Several clients may start one at the same
//! moment: the daemon takes a single-instance lock before it listens, so one wins and the rest
//! exit at once, and every client attaches to the winner.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use imperium_protocol::discovery::{DaemonEndpoint, read_endpoint};
use imperium_protocol::jsonrpc::codes;
use imperium_protocol::methods::{LeaseKind, Peer};

use crate::{Client, ClientError};

/// How long a freshly started daemon may take to listen.
pub const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How large the daemon's log may grow before a new one is started.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;
/// How long between looks for a daemon that is starting.
const POLL: Duration = Duration::from_millis(100);
/// How long before a daemon that is still not there is started again: the first may have failed.
const RESPAWN_AFTER: Duration = Duration::from_secs(3);

/// How to start the daemon.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub envs: Vec<(OsString, OsString)>,
    /// Where the daemon's log goes; discarded without one.
    pub log: Option<PathBuf>,
    /// How long to wait for the daemon to listen.
    pub timeout: Duration,
}

impl Launch {
    /// Starts `program` as a daemon that exits when no client and no work uses it, after `grace`
    /// (the daemon's own default of 10 seconds without one).
    pub fn shared(program: PathBuf, grace: Option<Duration>) -> Self {
        let arg = match grace {
            Some(grace) => format!("--exit-when-idle={}", grace.as_secs().max(1)),
            None => "--exit-when-idle".to_string(),
        };
        Launch {
            program,
            args: vec![arg.into()],
            envs: Vec::new(),
            log: None,
            timeout: START_TIMEOUT,
        }
    }

    /// Starts the daemon once, detached, without waiting for it.
    pub fn spawn(&self) -> std::io::Result<()> {
        if let Some(log) = &self.log {
            if let Some(dir) = log.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // The log is appended to by every daemon a client starts; keep one earlier file.
            if std::fs::metadata(log).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
                let _ = std::fs::rename(log, log.with_extension("log.old"));
            }
        }
        let build = || -> std::io::Result<Command> {
            let mut command = Command::new(&self.program);
            command
                .args(&self.args)
                .envs(self.envs.iter().cloned())
                .stdin(Stdio::null())
                .stdout(Stdio::null());
            match &self.log {
                Some(log) => {
                    let file = std::fs::File::options()
                        .create(true)
                        .append(true)
                        .open(log)?;
                    command.stderr(Stdio::from(file));
                }
                None => {
                    command.stderr(Stdio::null());
                }
            }
            Ok(command)
        };
        let mut child = spawn_detached(build)?;
        // Reaps the daemon if it exits while this process still runs.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Starts the command apart from this process's console and process group, so closing the
/// terminal or pressing Ctrl+C there does not end the daemon: its lifetime is its leases.
fn spawn_detached(
    build: impl Fn() -> std::io::Result<Command>,
) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        // A job object that ends its processes with the terminal would end the daemon too;
        // leaving it is refused when the job does not allow it, and then the daemon stays in.
        build()?
            .creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
            .or_else(|_| build()?.creation_flags(flags).spawn())
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        build()?.process_group(0).spawn()
    }
    #[cfg(not(any(windows, unix)))]
    {
        build()?.spawn()
    }
}

/// `imperium-core` beside the running executable, else on the `PATH`.
pub fn find_daemon() -> Option<PathBuf> {
    let name = format!("imperium-core{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(&name))
        .filter(|path| path.is_file());
    beside.or_else(|| {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(&name))
            .find(|path| path.is_file())
    })
}

/// The lease a client takes on the daemon it attaches to.
#[derive(Debug, Clone)]
pub struct LeaseRequest {
    pub kind: LeaseKind,
    /// What the client is doing, such as `imperium chat`.
    pub label: String,
    /// The client's process id, for a daemon on the same machine; `None` for one reached over a
    /// forward, where the id means nothing to it.
    pub pid: Option<u32>,
}

enum Attempt {
    Attached(Box<Client>, DaemonEndpoint),
    /// No daemon answers, or the one that did is going away.
    NotUp(ClientError),
    /// Something a new daemon would not fix, such as an incompatible protocol.
    Failed(ClientError),
}

async fn attempt(endpoint_file: &Path, peer: &Peer, lease: Option<&LeaseRequest>) -> Attempt {
    let endpoint = match read_endpoint(endpoint_file) {
        Ok(endpoint) => endpoint,
        Err(error) => return Attempt::NotUp(error.into()),
    };
    let mut client = match Client::connect(&endpoint, peer.clone()).await {
        Ok(client) => client,
        Err(error @ (ClientError::Connect(_) | ClientError::Protocol(_))) => {
            return Attempt::NotUp(error);
        }
        Err(error) => return Attempt::Failed(error),
    };
    if let Some(lease) = lease {
        match client.lease(lease.kind, &lease.label, lease.pid).await {
            Ok(_) => {}
            // A daemon of an earlier protocol takes no leases; it was not started to exit.
            Err(ClientError::Rpc(error)) if error.code == codes::METHOD_NOT_FOUND => {}
            Err(error @ ClientError::Rpc(_)) if is_stopping(&error) => {
                return Attempt::NotUp(error);
            }
            Err(error @ (ClientError::Connect(_) | ClientError::Protocol(_))) => {
                return Attempt::NotUp(error);
            }
            Err(error) => return Attempt::Failed(error),
        }
    }
    Attempt::Attached(Box::new(client), endpoint)
}

fn is_stopping(error: &ClientError) -> bool {
    matches!(error, ClientError::Rpc(error) if error.code == codes::DAEMON_STOPPING)
}

/// Connects to the daemon the endpoint file names and takes `lease` on it, returning the
/// connection (which holds the lease) and the endpoint it reached. When none answers (or the
/// one that does is exiting) and `launch` is given, starts one and waits for it; a daemon that
/// is still not there after `launch.timeout` is an error.
pub async fn attach(
    endpoint_file: &Path,
    peer: Peer,
    lease: Option<LeaseRequest>,
    launch: Option<&Launch>,
) -> Result<(Client, DaemonEndpoint), ClientError> {
    let mut last = match attempt(endpoint_file, &peer, lease.as_ref()).await {
        Attempt::Attached(client, endpoint) => return Ok((*client, endpoint)),
        Attempt::Failed(error) => return Err(error),
        Attempt::NotUp(error) => error,
    };
    let Some(launch) = launch else {
        return Err(last);
    };
    let deadline = Instant::now() + launch.timeout;
    let mut started: Option<Instant> = None;
    loop {
        if started.is_none_or(|at| at.elapsed() >= RESPAWN_AFTER) {
            launch.spawn().map_err(|error| {
                ClientError::Launch(format!(
                    "could not start {}: {error}",
                    launch.program.display()
                ))
            })?;
            started = Some(Instant::now());
        }
        tokio::time::sleep(POLL).await;
        match attempt(endpoint_file, &peer, lease.as_ref()).await {
            Attempt::Attached(client, endpoint) => return Ok((*client, endpoint)),
            Attempt::Failed(error) => return Err(error),
            Attempt::NotUp(error) => last = error,
        }
        if Instant::now() >= deadline {
            return Err(ClientError::Launch(format!(
                "imperium-core did not start within {} seconds ({last})",
                launch.timeout.as_secs()
            )));
        }
    }
}
