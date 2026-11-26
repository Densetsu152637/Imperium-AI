//! Cap units for the processes Imperium starts ([resource caps plan](../../../docs/plans/resource-caps.md), L1).
//!
//! A **unit** is one process tree with a memory cap, a process (or, on Linux, task) cap and an
//! optional CPU cap. The OS enforces them where it can: a Job Object on Windows (L2), a cgroup v2
//! child of a delegated subtree on Linux (L3), and otherwise per-process rlimits plus a poll of
//! the tree (L3's fallback, L4 on macOS). Units can sit in a session group, and every capped unit
//! sits under one total.
//!
//! Every process starts inside its unit: on Windows it is created suspended, assigned, then
//! resumed; on Linux a launcher (`Config::launcher`) joins the cgroup or sets the rlimits and
//! then `exec`s the program; on macOS the tree is polled from its first sample. A failure at any
//! step ends the process before its code runs and reports `refused_start`.
//!
//! When a cap stops a unit, [`Unit::ended`] says which limit, its value and the unit's peak.

#![warn(unreachable_pub)]

#[cfg(target_os = "linux")]
pub mod cgroup;
#[cfg(target_os = "linux")]
pub mod launcher;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
use windows as backend;

use std::{
    ffi::{OsStr, OsString},
    fmt,
    ops::{Deref, DerefMut},
    path::PathBuf,
    process::ExitStatus,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::{
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{mpsc, watch},
};

/// What a unit runs, which decides its default caps and priority (L6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Command,
    /// The user's own terminals: a unit with no caps (S2).
    Terminal,
    Mcp,
    Hook,
    Cas,
    Script,
    Browser,
    Internal,
    Model,
}

impl Class {
    /// The class's name, as settings and notices spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Terminal => "terminal",
            Self::Mcp => "mcp",
            Self::Hook => "hook",
            Self::Cas => "cas",
            Self::Script => "script",
            Self::Browser => "browser",
            Self::Internal => "internal",
            Self::Model => "model",
        }
    }
}

/// A unit's caps; `None` is no cap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// Memory of the whole tree, in bytes (committed memory on Windows, the cgroup's charge on
    /// Linux, the summed footprint on macOS and in the Linux fallback).
    pub memory_bytes: Option<u64>,
    /// Processes in the tree: Windows' active-process limit and the macOS poll's count.
    pub processes: Option<u32>,
    /// Linux tasks (`pids.max`), which counts threads as well as processes.
    pub tasks: Option<u32>,
    /// Percent of the whole machine's CPU (1 to 100).
    pub cpu_percent: Option<u16>,
}

/// What to start a unit as.
#[derive(Clone, Debug)]
pub struct UnitSpec {
    pub class: Class,
    pub caps: Caps,
    /// The session whose aggregate the unit counts against, if any.
    pub session: Option<String>,
    /// A short description for notices (cut and cleaned by the caller).
    pub label: String,
}

/// Which limit stopped a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Memory,
    Processes,
    Tasks,
    Cpu,
    SessionMemory,
    SessionProcesses,
    TotalMemory,
    TotalProcesses,
}

impl Limit {
    /// The limit's name in `resource.limited` (L10).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Processes => "processes",
            Self::Tasks => "tasks",
            Self::Cpu => "cpu",
            Self::SessionMemory => "session_memory",
            Self::SessionProcesses => "session_processes",
            Self::TotalMemory => "total_memory",
            Self::TotalProcesses => "total_processes",
        }
    }

    fn is_memory(self) -> bool {
        matches!(self, Self::Memory | Self::SessionMemory | Self::TotalMemory)
    }
}

/// How this host enforces caps (`imperium limits status`, L10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mechanism {
    Job,
    Cgroup,
    RlimitPoll,
    Poll,
}

impl Mechanism {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::Cgroup => "cgroup",
            Self::RlimitPoll => "rlimit + poll",
            Self::Poll => "poll",
        }
    }
}

/// How a unit's main process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// It exited on its own (or was killed by something other than a cap or [`Unit::stop`]).
    Exited(ExitStatus),
    /// A cap stopped the whole unit. `value` is the cap; `peak` the unit's peak of the same
    /// measure (bytes for memory limits, a count for process and task limits).
    Limited { limit: Limit, value: u64, peak: u64 },
    /// [`Unit::stop`] stopped it.
    Stopped(ExitStatus),
}

/// A unit's use of its resources.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub memory_bytes: u64,
    pub peak_memory_bytes: u64,
    pub processes: u32,
    pub peak_processes: u32,
    /// Linux only: threads and processes, as `pids.current` counts them.
    pub tasks: Option<u32>,
    pub peak_tasks: Option<u32>,
    /// CPU time of the whole tree, where the mechanism accounts it (a job, `cpu.stat`).
    pub cpu_time: Option<Duration>,
}

/// Why a unit could not be set up or started; the process, if any, was ended before running.
#[derive(Debug)]
pub struct LimitError {
    /// The step that failed (`job`, `assign`, `resume`, `cgroup`, `spawn`, ...).
    pub stage: &'static str,
    pub message: String,
}

impl LimitError {
    pub(crate) fn new(stage: &'static str, message: impl fmt::Display) -> Self {
        Self {
            stage,
            message: message.to_string(),
        }
    }
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refused_start: {}: {}", self.stage, self.message)
    }
}

impl std::error::Error for LimitError {}

/// The program that puts a process in its unit on Linux before `exec`ing the real one (L3): the
/// daemon's hidden `limit` subcommand, or the test probe's `launch`.
#[derive(Clone, Debug)]
pub struct Launcher {
    pub program: PathBuf,
    /// Arguments before the launcher's own options (the subcommand's name).
    pub args: Vec<OsString>,
}

/// How a [`Limits`] is set up.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Caps of the total: every capped unit except models and terminals.
    pub total: Caps,
    /// Linux: the delegated cgroup v2 subtree units live in. `None` uses rlimits and the poll.
    pub cgroup_root: Option<PathBuf>,
    /// Linux: the launcher (required there; ignored elsewhere).
    pub launcher: Option<Launcher>,
}

/// The cap units of one daemon.
pub struct Limits {
    inner: backend::Limits,
}

impl fmt::Debug for Limits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Limits")
            .field("mechanism", &self.mechanism())
            .finish_non_exhaustive()
    }
}

impl Limits {
    /// Sets up the total (and, on Linux with a `cgroup_root`, checks and prepares the subtree).
    ///
    /// # Errors
    /// When the total cannot be created or the cgroup root is not usable.
    pub fn new(config: Config) -> Result<Self, LimitError> {
        Ok(Self {
            inner: backend::Limits::new(config)?,
        })
    }

    #[must_use]
    pub fn mechanism(&self) -> Mechanism {
        self.inner.mechanism()
    }

    /// Creates or updates a session's aggregate. Units naming an unknown session get one with no
    /// caps.
    ///
    /// # Errors
    /// When the session's job or cgroup cannot be set up.
    pub fn set_session(&self, session: &str, caps: Caps) -> Result<(), LimitError> {
        self.inner.set_session(session, caps)
    }

    /// A command for `program` that [`UnitCommand::spawn`] starts inside a new unit. Arguments,
    /// environment, working folder and stdio are set on it as on any [`Command`]; its creation
    /// flags (Windows) and process group (Unix) belong to the unit.
    ///
    /// # Errors
    /// When the unit's job or cgroup cannot be created.
    pub fn command(
        &self,
        spec: UnitSpec,
        program: impl AsRef<OsStr>,
    ) -> Result<UnitCommand, LimitError> {
        let (command, prepared) = self.inner.command(spec, program.as_ref())?;
        Ok(UnitCommand { command, prepared })
    }
}

/// A command that starts in its own unit.
pub struct UnitCommand {
    command: Command,
    prepared: backend::Prepared,
}

impl fmt::Debug for UnitCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitCommand")
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

impl Deref for UnitCommand {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.command
    }
}

impl DerefMut for UnitCommand {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.command
    }
}

impl UnitCommand {
    /// Starts the process inside its unit. Must be called within a Tokio runtime.
    ///
    /// # Errors
    /// `refused_start`: the process could not be started inside the unit, and nothing of it ran.
    pub fn spawn(self) -> Result<Unit, LimitError> {
        let Self {
            mut command,
            prepared,
        } = self;
        #[cfg(unix)]
        command.process_group(0);
        let (mut child, os) = prepared.spawn(&mut command)?;
        let pid = child.id().unwrap_or_default();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();
        let (ended_tx, ended_rx) = watch::channel(None);
        let monitor = os.monitor();
        tokio::spawn(supervise(
            child,
            Arc::clone(&os),
            monitor,
            stop_rx,
            ended_tx,
        ));
        Ok(Unit {
            pid,
            os,
            stop_tx,
            ended: ended_rx,
            stdin,
            stdout,
            stderr,
        })
    }
}

/// A running unit. Dropping it stops everything left in it.
pub struct Unit {
    pid: u32,
    os: Arc<backend::UnitOs>,
    stop_tx: mpsc::UnboundedSender<()>,
    ended: watch::Receiver<Option<Ending>>,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

impl fmt::Debug for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Unit")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl Unit {
    /// The main process's id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Current and peak use.
    #[must_use]
    pub fn usage(&self) -> Usage {
        self.os.usage()
    }

    /// Caps this unit was given that this host's mechanism does not enforce (the CPU cap on macOS
    /// and in the Linux fallback, for example). They are reported, not silently dropped.
    #[must_use]
    pub fn not_enforced(&self) -> Vec<Limit> {
        self.os.not_enforced()
    }

    /// Stops the whole tree. [`Unit::ended`] then reports [`Ending::Stopped`] unless a cap got
    /// there first.
    pub fn stop(&self) {
        let _ = self.stop_tx.send(());
    }

    /// Waits for the main process to end and says why it did.
    pub async fn ended(&mut self) -> Ending {
        match self.ended.wait_for(Option::is_some).await {
            Ok(ending) => (*ending).unwrap_or_else(|| Ending::Stopped(exit_status_unknown())),
            // The supervisor is gone without a word; only a runtime shutting down does that.
            Err(_) => Ending::Stopped(exit_status_unknown()),
        }
    }

    /// The unit's job (Windows).
    #[cfg(windows)]
    #[must_use]
    pub fn job(&self) -> &windows::Job {
        self.os.job()
    }

    /// How long the thread snapshot that found the suspended main thread took (Windows, L2).
    #[cfg(windows)]
    #[must_use]
    pub fn snapshot_time(&self) -> Duration {
        self.os.snapshot_time()
    }
}

#[cfg(unix)]
fn exit_status_unknown() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;
    ExitStatus::from_raw(9)
}

#[cfg(windows)]
fn exit_status_unknown() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt as _;
    ExitStatus::from_raw(1)
}

/// What the supervisor and [`Unit::usage`] share: peaks and the first limit that stopped it.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    limited: Mutex<Option<(Limit, u64)>>,
    stopped: AtomicBool,
    pub(crate) peak_memory: AtomicU64,
    pub(crate) peak_processes: AtomicU32,
    pub(crate) peak_tasks: AtomicU32,
    pub(crate) memory: AtomicU64,
    pub(crate) processes: AtomicU32,
    pub(crate) tasks: AtomicU32,
}

impl Shared {
    /// Records `limit` as the reason the unit stops, unless one is already recorded.
    pub(crate) fn limit(&self, limit: Limit, value: u64) {
        let mut slot = self
            .limited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some((limit, value));
        }
    }

    pub(crate) fn limited(&self) -> Option<(Limit, u64)> {
        *self
            .limited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a sample: current values and their peaks.
    pub(crate) fn sample(&self, memory: u64, processes: u32, tasks: u32) {
        self.memory.store(memory, Ordering::Relaxed);
        self.processes.store(processes, Ordering::Relaxed);
        self.tasks.store(tasks, Ordering::Relaxed);
        self.peak_memory.fetch_max(memory, Ordering::Relaxed);
        self.peak_processes.fetch_max(processes, Ordering::Relaxed);
        self.peak_tasks.fetch_max(tasks, Ordering::Relaxed);
    }
}

/// Watches one unit until its main process ends, enforces what the OS leaves to the daemon,
/// reports the ending, and stops what is left when the unit is stopped or dropped.
///
/// The backend's `Monitor` offers: `interval` (how often `check` samples), `check` (a limit the
/// unit is past, which is then enforced), `changed` (resolves when the OS says something changed,
/// inotify; never where it cannot), `kill` (stop the tree while its main process is unreaped),
/// `after_exit` (the limit the OS's counters show ended it) and `kill_remaining` (stop what is
/// left after the main process is reaped, and clean up).
async fn supervise(
    mut child: Child,
    os: Arc<backend::UnitOs>,
    mut monitor: backend::Monitor,
    mut stop_rx: mpsc::UnboundedReceiver<()>,
    ended_tx: watch::Sender<Option<Ending>>,
) {
    let shared = &os.shared;
    let mut tick = tokio::time::interval(monitor.interval());
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut dropped = false;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            request = stop_rx.recv(), if !dropped => {
                dropped = request.is_none();
                shared.stopped.store(true, Ordering::SeqCst);
                // The main process is not reaped (its wait has not completed), so its process
                // group id is still its own.
                monitor.kill();
            }
            () = monitor.changed() => {
                if let Some((limit, value)) = monitor.check() {
                    shared.limit(limit, value);
                    monitor.kill();
                }
            }
            _ = tick.tick() => {
                if let Some((limit, value)) = monitor.check() {
                    shared.limit(limit, value);
                    monitor.kill();
                }
            }
        }
    };
    if let Some((limit, value)) = monitor.after_exit() {
        shared.limit(limit, value);
    }
    let usage = os.usage();
    let status = status.unwrap_or_else(|_| exit_status_unknown());
    let ending = match shared.limited() {
        Some((limit, value)) => Ending::Limited {
            limit,
            value,
            peak: if limit.is_memory() {
                usage.peak_memory_bytes
            } else if limit == Limit::Tasks {
                u64::from(usage.peak_tasks.unwrap_or(usage.peak_processes))
            } else {
                u64::from(usage.peak_processes)
            },
        },
        None if shared.stopped.load(Ordering::SeqCst) => Ending::Stopped(status),
        None => Ending::Exited(status),
    };
    let _ = ended_tx.send(Some(ending));
    // Descendants may still run. They stay in the unit until it is stopped or dropped.
    if !dropped {
        let _ = stop_rx.recv().await;
    }
    monitor.kill_remaining().await;
}
