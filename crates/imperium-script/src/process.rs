//! The worker process and its pipes, whatever launched it: a plain child on Linux and macOS (the
//! worker confines itself), a confined launch on Windows.

use std::{any::Any, ffi::OsString, fmt, path::Path};

use tokio::io::{AsyncRead, AsyncWrite};

/// The daemon's end of a pipe the worker writes.
pub(crate) type Reader = Box<dyn AsyncRead + Send + Unpin>;
/// The daemon's end of the pipe the worker reads.
pub(crate) type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// A started worker.
pub(crate) struct Spawned {
    pub(crate) process: WorkerProcess,
    pub(crate) stdin: Writer,
    pub(crate) stdout: Reader,
    pub(crate) stderr: Reader,
    /// Kept for as long as the run: its working folder (removed when dropped).
    pub(crate) keep: Vec<Box<dyn Any + Send + Sync>>,
}

/// How a worker exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Exit {
    /// The exit code, when it exited rather than being signalled.
    pub(crate) code: Option<i32>,
    /// For a failure reason.
    pub(crate) text: String,
}

/// The worker process.
pub(crate) enum WorkerProcess {
    #[cfg(unix)]
    Child(tokio::process::Child),
    #[cfg(windows)]
    Confined(crate::windows::ConfinedProcess),
}

impl fmt::Debug for WorkerProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkerProcess")
            .field("pid", &self.id())
            .finish()
    }
}

impl WorkerProcess {
    pub(crate) fn id(&self) -> Option<u32> {
        match self {
            #[cfg(unix)]
            Self::Child(child) => child.id(),
            #[cfg(windows)]
            Self::Confined(process) => Some(process.id()),
        }
    }

    /// Sends the kill; does not wait.
    pub(crate) fn start_kill(&mut self) {
        match self {
            #[cfg(unix)]
            Self::Child(child) => {
                let _ = child.start_kill();
            }
            #[cfg(windows)]
            Self::Confined(process) => process.kill(),
        }
    }

    /// Waits for the process to exit.
    pub(crate) async fn wait(&mut self) -> Option<Exit> {
        match self {
            #[cfg(unix)]
            Self::Child(child) => child.wait().await.ok().map(|status| Exit {
                code: status.code(),
                text: status.to_string(),
            }),
            #[cfg(windows)]
            Self::Confined(process) => process.wait().await.map(|code| Exit {
                code: Some(code as i32),
                text: format!("exit code {code:#x}"),
            }),
        }
    }
}

/// Starts the worker as a plain child: it confines itself before it reads anything. Empty
/// environment, an empty private working folder, and only the three pipes besides (the worker
/// closes every other inherited descriptor first).
#[cfg(unix)]
pub(crate) fn spawn(
    worker: &Path,
    memory_mb: u64,
    extra_args: &[OsString],
) -> Result<Spawned, String> {
    use std::process::Stdio;

    let scratch = ScratchDir::create()
        .map_err(|err| format!("cannot make the worker's working folder: {err}"))?;
    let mut command = tokio::process::Command::new(worker);
    command
        .current_dir(&scratch.0)
        .arg("--memory-mb")
        .arg(memory_mb.to_string())
        .args(extra_args)
        // An empty environment: no tokens, no paths.
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|err| format!("cannot start the worker {}: {err}", worker.display()))?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.start_kill();
        return Err("the worker's pipes are missing".to_owned());
    };
    Ok(Spawned {
        process: WorkerProcess::Child(child),
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Box::new(stderr),
        keep: vec![Box::new(scratch)],
    })
}

/// Starts the worker confined by the daemon (`crate::windows::launch`).
#[cfg(windows)]
pub(crate) async fn spawn(
    worker: &Path,
    memory_mb: u64,
    extra_args: &[OsString],
    forced: Option<&str>,
) -> Result<Spawned, String> {
    if forced == Some("labels") {
        return Err("labels: forced to fail by the test hook".to_owned());
    }
    crate::windows::labels::status().map_err(|reason| format!("labels: {reason}"))?;
    let mut args = vec![
        OsString::from("--memory-mb"),
        OsString::from(memory_mb.to_string()),
    ];
    args.extend(extra_args.iter().cloned());
    let confined = crate::windows::spawn(worker, &args, memory_mb, forced)?;
    let crate::windows::Confined {
        process,
        stdin,
        stdout,
        stderr,
        folder,
    } = confined;
    let (stdin, stdout, stderr) = connect(stdin, stdout, stderr)
        .await
        .map_err(|err| format!("pipes: {err}"))?;
    Ok(Spawned {
        process: WorkerProcess::Confined(process),
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Box::new(stderr),
        keep: vec![Box::new(folder)],
    })
}

/// The pipes' only client opened them before the launch, so each connect returns at once.
#[cfg(windows)]
async fn connect(
    stdin: tokio::net::windows::named_pipe::NamedPipeServer,
    stdout: tokio::net::windows::named_pipe::NamedPipeServer,
    stderr: tokio::net::windows::named_pipe::NamedPipeServer,
) -> std::io::Result<(
    tokio::net::windows::named_pipe::NamedPipeServer,
    tokio::net::windows::named_pipe::NamedPipeServer,
    tokio::net::windows::named_pipe::NamedPipeServer,
)> {
    stdin.connect().await?;
    stdout.connect().await?;
    stderr.connect().await?;
    Ok((stdin, stdout, stderr))
}

/// An empty folder made for one worker to start in (plan C1: an empty working folder).
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct ScratchDir(std::path::PathBuf);

#[cfg(unix)]
impl ScratchDir {
    fn create() -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "imperium-script-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

#[cfg(unix)]
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}
