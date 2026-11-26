//! Runs a shell on a pseudo terminal on Linux, macOS and Windows, with no knowledge of sessions
//! or the protocol (stage 1 of `docs/plans/terminal-file-panes.md`).
//!
//! [`Pty::spawn`] starts the program on a PTY (`openpty` on Unix, ConPTY on Windows) from one
//! dedicated spawner thread. A reader thread fills a 1 MiB ring of output addressed by a running
//! byte offset ([`Replay`]) and calls subscribers; [`Pty::write`] hands input to a writer thread
//! through a bounded queue and never blocks the caller; [`Pty::end`] ends the shell, its
//! background jobs and everything they started (decision D5): on Unix by session, on Linux
//! through the per-terminal [`supervisor`], on Windows through a Job Object and a walk of the
//! process tree.
//!
//! `portable-pty` 0.9.0 closes every descriptor above 2 in the child before `exec`
//! (`close_random_fds`), resets the common signals to their defaults, makes the child a session
//! leader with the PTY as its controlling terminal, and passes argv to the program unchanged
//! (`arg0` is `argv[0]`, no shell wrapping, no login rewriting for an explicit program).
//! Its environment starts as the parent's copy and `SHELL` is always set; so the spawn clears it and
//! passes [`Spec::env`] as the whole environment. A missing working folder falls back to
//! `$HOME` silently, so [`Pty::spawn`] checks the folder itself.

#[cfg(unix)]
mod freeze;
mod ring;
mod spawner;
#[cfg(target_os = "linux")]
pub mod supervisor;
#[cfg(unix)]
mod sweep;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

pub use ring::{RING_CAPACITY, Replay, Ring};

/// Input queued for the shell, in bytes; a write that would pass it fails.
pub const WRITE_QUEUE_CAPACITY: usize = 1024 * 1024;
/// How long [`Pty::end`] waits for the supervisor to end the processes before it falls back to
/// the session sweep.
#[cfg(target_os = "linux")]
const END_WAIT: Duration = Duration::from_secs(9);
/// How long the exit waits for the reader to finish before announcing itself.
const DRAIN_WAIT: Duration = Duration::from_secs(1);

/// What to run and how.
#[derive(Debug, Clone)]
pub struct Spec {
    /// The program, an absolute path chosen by the caller (the shell choice is not this crate's).
    pub program: OsString,
    pub args: Vec<OsString>,
    /// The working folder; it must exist.
    pub cwd: PathBuf,
    /// The child's whole environment (nothing is inherited).
    pub env: Vec<(OsString, OsString)>,
    pub cols: u16,
    pub rows: u16,
    /// Linux: how the supervisor is started. `None` re-runs this process's own executable
    /// (through `/proc/self/exe`, so a replaced file on disk cannot change what runs) with
    /// [`supervisor::SUBCOMMAND`]. **Never fill this from protocol input, a setting a repository
    /// can write or any other untrusted source**: it names a program that runs with the user's
    /// rights. It exists for tests.
    pub supervisor: Option<Supervisor>,
    /// Linux: the process whose death ends the terminal. `None` is this process. **Never fill
    /// this from untrusted input** either; it exists for tests.
    pub daemon: Option<DaemonRef>,
}

impl Spec {
    /// A terminal of 80 columns by 24 rows with an empty environment.
    pub fn new(program: impl Into<OsString>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
            supervisor: None,
            daemon: None,
        }
    }
}

/// How to start the Linux supervisor: `<exe> <args> --daemon <pid>:<start> <flags> -- <program>`.
#[derive(Debug, Clone)]
pub struct Supervisor {
    pub exe: PathBuf,
    /// Before `--daemon` (the daemon's executable gets [`supervisor::SUBCOMMAND`] here).
    pub args: Vec<OsString>,
    /// After `--daemon` (the supervisor's test-support flags).
    pub flags: Vec<OsString>,
}

/// A process the supervisor watches: its pid and its start time (field 22 of `/proc/<pid>/stat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonRef {
    pub pid: u32,
    pub start_time: u64,
}

/// Why a terminal could not start.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("a terminal needs at least one column and one row")]
    BadSize,
    #[error("the working folder {0} is not a folder")]
    NotAFolder(PathBuf),
    /// The host refuses a syscall the Linux supervisor needs; the terminal is refused here.
    #[error("this host refuses {syscall} ({reason}); terminals need it, so none can start here")]
    MissingSyscall {
        syscall: &'static str,
        reason: String,
    },
    #[error("could not start the terminal: {0}")]
    Start(String),
    #[error("the thread that starts terminals is gone")]
    SpawnerGone,
}

/// Why input was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("the input queue is full")]
    QueueFull,
    #[error("the terminal has ended")]
    Closed,
}

/// How the terminal's process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub code: u32,
    /// The signal that ended it, when `portable-pty` names one.
    pub signal: Option<String>,
}

/// What a subscriber is told.
#[derive(Debug)]
pub enum Event<'a> {
    /// Output: its `seq` (the running byte offset of its first byte) and the bytes. A multi-byte
    /// character is never split between two events.
    Data { seq: u64, bytes: &'a [u8] },
    /// The shell ended. Output that was still in flight is delivered before it, up to a second.
    Exited(&'a Exit),
}

/// A subscription's handle, for [`Pty::unsubscribe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionId(u64);

type Callback = dyn Fn(Event<'_>) + Send + Sync;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct State {
    ring: Ring,
    subscribers: Vec<(u64, Arc<Callback>)>,
    next_id: u64,
    exit: Option<Exit>,
    /// Whether the exit has been announced to the subscribers present at that moment; a later
    /// subscriber is handed the exit by `subscribe` instead.
    announced: bool,
    reader_done: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                ring: Ring::new(RING_CAPACITY),
                subscribers: Vec::new(),
                next_id: 0,
                exit: None,
                announced: false,
                reader_done: false,
            }),
            changed: Condvar::new(),
        }
    }

    /// Appends output to the ring and tells the subscribers. Taking the list under the same lock
    /// as the push means a subscriber that joined meanwhile either has the chunk in its replay or
    /// gets it as an event, never both and never neither.
    fn publish(&self, chunk: &[u8]) {
        let (seq, subscribers) = {
            let mut state = lock(&self.state);
            let seq = state.ring.end();
            state.ring.push(chunk);
            let subscribers: Vec<Arc<Callback>> = state
                .subscribers
                .iter()
                .map(|(_, callback)| Arc::clone(callback))
                .collect();
            (seq, subscribers)
        };
        for callback in subscribers {
            // A panicking subscriber must not end the reader thread (and with it the output).
            let _ = catch_unwind(AssertUnwindSafe(|| {
                callback(Event::Data { seq, bytes: chunk });
            }));
        }
    }

    fn reader_finished(&self) {
        lock(&self.state).reader_done = true;
        self.changed.notify_all();
    }

    fn set_exit(&self, exit: Exit) {
        lock(&self.state).exit = Some(exit);
        self.changed.notify_all();
    }

    #[cfg(any(target_os = "linux", windows))]
    fn exited(&self) -> bool {
        lock(&self.state).exit.is_some()
    }

    fn wait_exit(&self, timeout: Duration) -> Option<Exit> {
        let state = lock(&self.state);
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.exit.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        state.exit.clone()
    }

    fn wait_reader(&self, timeout: Duration) {
        let state = lock(&self.state);
        let _ = self
            .changed
            .wait_timeout_while(state, timeout, |state| !state.reader_done)
            .unwrap_or_else(PoisonError::into_inner);
    }

    fn announce(&self, exit: &Exit) {
        let subscribers: Vec<Arc<Callback>> = {
            let mut state = lock(&self.state);
            state.announced = true;
            state
                .subscribers
                .iter()
                .map(|(_, callback)| Arc::clone(callback))
                .collect()
        };
        for callback in subscribers {
            let _ = catch_unwind(AssertUnwindSafe(|| callback(Event::Exited(exit))));
        }
    }
}

/// Input waiting for the shell, a bounded queue the writer thread empties.
#[derive(Default)]
struct WriteQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

#[derive(Default)]
struct QueueState {
    bytes: VecDeque<u8>,
    closed: bool,
}

impl WriteQueue {
    fn push(&self, input: &[u8]) -> Result<(), WriteError> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(WriteError::Closed);
        }
        if state.bytes.len() + input.len() > WRITE_QUEUE_CAPACITY {
            return Err(WriteError::QueueFull);
        }
        state.bytes.extend(input);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.bytes.clear();
        drop(state);
        self.ready.notify_all();
    }
}

fn write_loop(queue: &WriteQueue, mut out: Box<dyn Write + Send>) {
    let mut chunk = Vec::with_capacity(8192);
    loop {
        {
            let mut state = lock(&queue.state);
            while state.bytes.is_empty() && !state.closed {
                state = queue
                    .ready
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.closed {
                return;
            }
            let count = state.bytes.len().min(8192);
            chunk.clear();
            chunk.extend(state.bytes.drain(..count));
        }
        // Outside the lock: a shell that is not reading blocks this thread, not the callers.
        if out.write_all(&chunk).and_then(|()| out.flush()).is_err() {
            lock(&queue.state).closed = true;
            return;
        }
    }
}

/// The bytes at the end of `bytes` that start a UTF-8 character which is not complete yet (at
/// most three), so a character split across two reads is delivered whole.
fn incomplete_tail(bytes: &[u8]) -> usize {
    let len = bytes.len();
    for back in 1..=len.min(3) {
        let byte = bytes[len - back];
        if byte & 0xC0 == 0x80 {
            // A continuation byte: the lead is further back.
            continue;
        }
        let need = match byte {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            // ASCII or not a lead: nothing to hold.
            _ => return 0,
        };
        return if back < need { back } else { 0 };
    }
    0
}

fn read_loop(mut reader: Box<dyn Read + Send>, shared: &Shared) {
    let mut buffer = vec![0u8; 16 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                pending.extend_from_slice(&buffer[..count]);
                let whole = pending.len() - incomplete_tail(&pending);
                if whole > 0 {
                    let chunk: Vec<u8> = pending.drain(..whole).collect();
                    shared.publish(&chunk);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        shared.publish(&pending);
    }
    shared.reader_finished();
}

type Master = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;

/// Closes the pseudo console on a thread of its own: on Windows `ClosePseudoConsole` can block
/// until the output has been read, and the reader thread keeps draining meanwhile.
fn close_master(master: &Master) {
    let taken = lock(master).take();
    if let Some(master) = taken {
        let _ = thread::Builder::new()
            .name("imperium-pty-close".into())
            .spawn(move || drop(master));
    }
}

/// Ends the terminal's processes; held by the [`Pty`] until `end` or drop.
struct Terminator {
    pid: Option<u32>,
    /// Read where the shell's exit tells whether its pid is still its own (Linux, Windows).
    #[cfg(any(target_os = "linux", windows))]
    shared: Arc<Shared>,
    master: Master,
    /// A pidfd of the supervisor, opened before anything could reap it: `SIGTERM` goes to the
    /// process that was started, never to a reused pid.
    #[cfg(target_os = "linux")]
    pidfd: Option<rustix::fd::OwnedFd>,
    /// The job holding the shell and its descendants; closing it kills them all.
    #[cfg(windows)]
    job: Option<win32job::Job>,
}

impl Terminator {
    #[cfg(unix)]
    fn run(self) {
        use rustix::process::Pid;

        let sid = self
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(Pid::from_raw);
        // Linux: the supervisor ends every descendant, whichever session it is in.
        #[cfg(target_os = "linux")]
        {
            if let Some(pidfd) = &self.pidfd
                && !self.shared.exited()
            {
                let _ = rustix::process::pidfd_send_signal(pidfd, rustix::process::Signal::TERM);
                self.shared.wait_exit(END_WAIT);
            }
        }
        // Everywhere: by session (the PTY's child is its leader). On Linux this catches what
        // survives a supervisor that was itself killed; on macOS it is the whole job.
        if let Some(sid) = sid {
            sweep::sweep(sid);
        }
        close_master(&self.master);
    }

    #[cfg(windows)]
    fn run(self) {
        use std::process::{Command, Stdio};

        let Self {
            pid,
            shared,
            master,
            job,
        } = self;
        // The shell's tree first: a child started before the shell joined its job is not in the
        // job. With a job, only while the shell is still in it (so the id is still its own).
        let ours = match &job {
            Some(job) => job
                .query_process_id_list()
                .is_ok_and(|pids| pid.is_some_and(|pid| pids.contains(&(pid as usize)))),
            None => !shared.exited(),
        };
        // `taskkill` from the System32 folder (SHGetKnownFolderPath), never through `PATH` or an
        // environment variable.
        if ours
            && let Some(pid) = pid
            && let Some(system) =
                dirs_sys::known_folder(windows_sys::Win32::UI::Shell::FOLDERID_System)
        {
            let _ = Command::new(system.join("taskkill.exe"))
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        // Then the job, which kills every process in it.
        drop(job);
        close_master(&master);
    }
}

/// A shell on a pseudo terminal.
pub struct Pty {
    shared: Arc<Shared>,
    queue: Arc<WriteQueue>,
    master: Master,
    terminator: Mutex<Option<Terminator>>,
    pid: Option<u32>,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pty")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// The argv the PTY's child is started with.
fn launch_argv(spec: &Spec) -> Result<Vec<OsString>, SpawnError> {
    #[cfg(target_os = "linux")]
    {
        supervisor::check_host().map_err(|reason| SpawnError::MissingSyscall {
            syscall: "pidfd_open",
            reason,
        })?;
        #[cfg(feature = "test-support")]
        {
            if FORCE_MISSING_SYSCALL.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(SpawnError::MissingSyscall {
                    syscall: "pidfd_open",
                    reason: "forced by a test".into(),
                });
            }
        }
        let daemon = match spec.daemon {
            Some(daemon) => daemon,
            None => {
                let pid = std::process::id();
                let start_time = supervisor::process_start_time(pid).map_err(|error| {
                    SpawnError::Start(format!("cannot read our start time: {error}"))
                })?;
                DaemonRef { pid, start_time }
            }
        };
        let (exe, mut argv) = match &spec.supervisor {
            Some(supervisor) => (supervisor.exe.clone(), supervisor.args.clone()),
            // `/proc/self/exe` is resolved by the forked child at `exec`, to this very executable,
            // even if the file on disk was replaced or deleted since (an update).
            None => (
                PathBuf::from("/proc/self/exe"),
                vec![OsString::from(supervisor::SUBCOMMAND)],
            ),
        };
        argv.insert(0, exe.into_os_string());
        argv.push("--daemon".into());
        argv.push(format!("{}:{}", daemon.pid, daemon.start_time).into());
        if let Some(supervisor) = &spec.supervisor {
            argv.extend(supervisor.flags.iter().cloned());
        }
        argv.push("--".into());
        argv.push(spec.program.clone());
        argv.extend(spec.args.iter().cloned());
        Ok(argv)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut argv = vec![spec.program.clone()];
        argv.extend(spec.args.iter().cloned());
        Ok(argv)
    }
}

/// Makes `pidfd_open` look missing to [`Pty::spawn`] (the supervisor's own check has the
/// `--test-no-pidfd` flag). Process-wide, so only a test binary of its own should set it.
#[cfg(feature = "test-support")]
static FORCE_MISSING_SYSCALL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Test support: makes every later [`Pty::spawn`] on Linux fail as a host without `pidfd_open`.
#[cfg(feature = "test-support")]
pub fn force_missing_syscall(missing: bool) {
    FORCE_MISSING_SYSCALL.store(missing, std::sync::atomic::Ordering::SeqCst);
}

type Started = (
    Box<dyn MasterPty + Send>,
    Box<dyn portable_pty::Child + Send + Sync>,
);

fn start(
    argv: Vec<OsString>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    size: PtySize,
) -> Result<Started, String> {
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|error| format!("could not open a pseudo terminal: {error}"))?;
    let mut command = CommandBuilder::from_argv(argv);
    command.env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    command.cwd(cwd);
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("could not start the program: {error}"))?;
    // Our copy of the slave must go, or the reader never sees the end of the output.
    drop(pair.slave);
    Ok((pair.master, child))
}

#[cfg(windows)]
fn assign_job(child: &dyn portable_pty::Child) -> Result<win32job::Job, String> {
    let handle = child
        .as_raw_handle()
        .ok_or_else(|| "the shell has no process handle".to_owned())?;
    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = win32job::Job::create_with_limit_info(&limits)
        .map_err(|error| format!("could not create a job: {error}"))?;
    job.assign_process(handle as isize)
        .map_err(|error| format!("could not put the shell in its job: {error}"))?;
    Ok(job)
}

impl Pty {
    /// Starts the program on a new pseudo terminal. It blocks until the child is running (call it
    /// from a blocking thread in async code).
    pub fn spawn(spec: Spec) -> Result<Self, SpawnError> {
        if spec.cols == 0 || spec.rows == 0 {
            return Err(SpawnError::BadSize);
        }
        if !spec.cwd.is_dir() {
            return Err(SpawnError::NotAFolder(spec.cwd.clone()));
        }
        let argv = launch_argv(&spec)?;
        let size = PtySize {
            rows: spec.rows,
            cols: spec.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let (cwd, env) = (spec.cwd, spec.env);
        let (master, mut child) =
            spawner::run(move || start(argv, cwd, env, size))?.map_err(SpawnError::Start)?;
        let reader = master
            .try_clone_reader()
            .map_err(|error| SpawnError::Start(format!("no reader: {error}")))?;
        let writer = master
            .take_writer()
            .map_err(|error| SpawnError::Start(format!("no writer: {error}")))?;
        let pid = child.process_id();
        // Fail closed: a shell that cannot be put in its job could not be ended reliably, so it
        // is killed and the terminal refused.
        #[cfg(windows)]
        let job = match assign_job(child.as_ref()) {
            Ok(job) => job,
            Err(reason) => {
                let _ = child.kill();
                // Closing the pseudo console can block until its output is read; not here.
                let _ = thread::Builder::new()
                    .name("imperium-pty-close".into())
                    .spawn(move || {
                        drop(reader);
                        drop(writer);
                        drop(master);
                    });
                return Err(SpawnError::Start(reason));
            }
        };
        #[cfg(target_os = "linux")]
        let pidfd = pid
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
            .and_then(|pid| {
                rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok()
            });

        let shared = Arc::new(Shared::new());
        let queue = Arc::new(WriteQueue::default());
        let master: Master = Arc::new(Mutex::new(Some(master)));

        let reading = Arc::clone(&shared);
        thread_named("imperium-pty-reader", move || read_loop(reader, &reading))?;
        let writing = Arc::clone(&queue);
        thread_named("imperium-pty-writer", move || write_loop(&writing, writer))?;
        let waiting = Arc::clone(&shared);
        #[cfg(windows)]
        let waiting_master = Arc::clone(&master);
        thread_named("imperium-pty-waiter", move || {
            let exit = match child.wait() {
                Ok(status) => Exit {
                    code: status.exit_code(),
                    signal: status.signal().map(str::to_owned),
                },
                Err(_) => Exit {
                    code: 1,
                    signal: None,
                },
            };
            waiting.set_exit(exit.clone());
            // ConPTY's output only ends when the pseudo console closes.
            #[cfg(windows)]
            close_master(&waiting_master);
            waiting.wait_reader(DRAIN_WAIT);
            waiting.announce(&exit);
        })?;

        let terminator = Terminator {
            pid,
            #[cfg(any(target_os = "linux", windows))]
            shared: Arc::clone(&shared),
            master: Arc::clone(&master),
            #[cfg(target_os = "linux")]
            pidfd,
            #[cfg(windows)]
            job: Some(job),
        };
        Ok(Self {
            shared,
            queue,
            master,
            terminator: Mutex::new(Some(terminator)),
            pid,
        })
    }

    /// The pid of the PTY's child: the supervisor on Linux, else the shell.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Queues input for the shell. A full queue (1 MiB) is an error, never a wait.
    pub fn write(&self, input: &[u8]) -> Result<(), WriteError> {
        self.queue.push(input)
    }

    /// Tells the terminal its new size.
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        let guard = lock(&self.master);
        let master = guard
            .as_ref()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotConnected))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    /// The output from `from` on (see [`Replay`]).
    pub fn replay(&self, from: u64) -> Replay {
        lock(&self.shared.state).ring.replay(from)
    }

    /// The `seq` the next byte of output will get.
    pub fn end_seq(&self) -> u64 {
        lock(&self.shared.state).ring.end()
    }

    /// Replays the output from `from` and then delivers new output and the exit to `callback`,
    /// with nothing missed or doubled in between. The callback runs on the reader thread: it must
    /// not block, and a panic in it is caught and dropped.
    ///
    /// The third value is the exit, **only if it was already announced to the earlier
    /// subscribers**: such a subscriber would never get an [`Event::Exited`], so it is handed the
    /// exit here, under the same lock. If the shell has ended but the announcement is still
    /// pending (the reader is draining, up to a second), the callback gets the event instead. A
    /// callback is **not registered** once the exit was announced: nothing more will come, and
    /// the returned [`SubscriptionId`] then names nothing.
    pub fn subscribe(
        &self,
        from: u64,
        callback: impl Fn(Event<'_>) + Send + Sync + 'static,
    ) -> (SubscriptionId, Replay, Option<Exit>) {
        let callback: Arc<Callback> = Arc::new(callback);
        let mut state = lock(&self.shared.state);
        let id = state.next_id;
        state.next_id += 1;
        let replay = state.ring.replay(from);
        let exit = if state.announced {
            state.exit.clone()
        } else {
            state.subscribers.push((id, callback));
            None
        };
        (SubscriptionId(id), replay, exit)
    }

    /// Stops a subscription. A callback that the reader thread has already begun to call may
    /// still run once after this returns.
    pub fn unsubscribe(&self, id: SubscriptionId) {
        lock(&self.shared.state)
            .subscribers
            .retain(|(found, _)| *found != id.0);
    }

    /// How the shell ended, if it has.
    pub fn exit(&self) -> Option<Exit> {
        lock(&self.shared.state).exit.clone()
    }

    /// Waits up to `timeout` for the shell to end.
    pub fn wait_exit(&self, timeout: Duration) -> Option<Exit> {
        self.shared.wait_exit(timeout)
    }

    /// Ends the shell, its background jobs and their children (decision D5), and returns how the
    /// shell ended. It blocks for up to a few seconds; call it from a blocking thread.
    pub fn end(&self) -> Option<Exit> {
        let terminator = lock(&self.terminator).take();
        if let Some(terminator) = terminator {
            terminator.run();
        }
        self.queue.close();
        self.shared.wait_exit(Duration::from_secs(5))
    }
}

#[cfg(all(windows, feature = "test-support"))]
impl Pty {
    /// Test support: the pids in the terminal's Job Object (`None` once it is closed).
    pub fn job_process_ids(&self) -> Option<Vec<u32>> {
        let guard = lock(&self.terminator);
        let job = guard.as_ref()?.job.as_ref()?;
        let pids = job.query_process_id_list().ok()?;
        Some(
            pids.into_iter()
                .filter_map(|pid| u32::try_from(pid).ok())
                .collect(),
        )
    }

    /// Test support: closes the Job Object alone, with no walk of the process tree, to show that
    /// the job by itself ends what is in it.
    pub fn close_job_only(&self) {
        if let Some(terminator) = lock(&self.terminator).as_mut() {
            terminator.job = None;
        }
    }
}

impl Drop for Pty {
    /// A terminal that is dropped is ended, on a thread of its own so the drop does not block.
    fn drop(&mut self) {
        self.queue.close();
        let terminator = lock(&self.terminator).take();
        if let Some(terminator) = terminator {
            let _ = thread::Builder::new()
                .name("imperium-pty-end".into())
                .spawn(move || terminator.run());
        }
    }
}

fn thread_named(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), SpawnError> {
    thread::Builder::new()
        .name(name.into())
        .spawn(work)
        .map(|_| ())
        .map_err(|error| SpawnError::Start(format!("could not start a thread: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_split_character_is_held_until_it_is_whole() {
        let smile = "\u{1F600}".as_bytes(); // F0 9F 98 80
        assert_eq!(incomplete_tail(b"abc"), 0);
        assert_eq!(incomplete_tail(&smile[..1]), 1);
        assert_eq!(incomplete_tail(&smile[..2]), 2);
        assert_eq!(incomplete_tail(&smile[..3]), 3);
        assert_eq!(incomplete_tail(smile), 0);
        assert_eq!(incomplete_tail("é".as_bytes()), 0);
        assert_eq!(incomplete_tail(&"é".as_bytes()[..1]), 1);
        // Invalid bytes are passed on, not held.
        assert_eq!(incomplete_tail(&[0xFF]), 0);
        assert_eq!(incomplete_tail(&[0x80, 0x80, 0x80]), 0);
        assert_eq!(incomplete_tail(b"ab\xE2\x82"), 2);
    }

    #[test]
    fn the_write_queue_is_bounded_and_refuses_after_close() {
        let queue = WriteQueue::default();
        let full: Vec<u8> = std::iter::repeat_n(0, WRITE_QUEUE_CAPACITY).collect();
        assert_eq!(queue.push(&full), Ok(()));
        assert_eq!(queue.push(b"x"), Err(WriteError::QueueFull));
        queue.close();
        assert_eq!(queue.push(b"x"), Err(WriteError::Closed));
    }
}
