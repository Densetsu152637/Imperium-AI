//! A supervised local speech-to-text server: whisper.cpp's `whisper-server`, started when
//! someone speaks, kept while it is used, and stopped when it is idle or a language model needs
//! the memory it holds.
//!
//! - **On demand.** [`SpeechServer::acquire`] starts the server if it is not running (loading
//!   the model takes seconds), and returns a [`SpeechLease`] naming its OpenAI-compatible base
//!   URL. The server keeps running between recordings.
//! - **Health.** A start counts only once the server answers; it is abandoned after
//!   `start_timeout`, with the server's log named. A server that exits on its own is noticed by
//!   the next call, which starts a fresh one; a call that failed marks the server for a restart.
//! - **Idle stop.** A watchdog stops a server nobody has used for `idle_stop`, never while a
//!   lease is held.
//! - **Memory beside the language models.** Before starting, the server's footprint
//!   ([`footprint`]) is planned against the room left beside the loaded models ([`Room`]): on the
//!   GPU when it fits there, else on the CPU, else the start is refused with a plain message.
//!   It never swaps a language model out. While it runs, the memory is reserved
//!   ([`Room::reserve`]), so models are planned around it; when a model cannot load only because
//!   of it, the server stops itself once idle ([`SideLoad::yield_now`]) and the model loads.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio::time::Instant;

use crate::planner::Budget;
use crate::supervisor::{SideLoad, Supervisor};

const MIB: u64 = 1024 * 1024;
/// How often the watchdog looks at the server.
pub const WATCH: Duration = Duration::from_secs(5);
/// How often the readiness of a starting server is checked.
const POLL: Duration = Duration::from_millis(200);
/// After stopping for a language model, the server does not take the GPU again for this long.
const GPU_COOLDOWN: Duration = Duration::from_secs(30);

/// The path whisper-server serves transcriptions on, so the server speaks the same route as an
/// OpenAI-compatible endpoint and `imperium-media` needs no special case.
pub const INFERENCE_PATH: &str = "/v1/audio/transcriptions";

/// Memory the server needs for a model file of `model_bytes`: the weights, the working buffers
/// that grow with them, and a fixed overhead. A planning figure, not a measurement.
pub fn footprint(model_bytes: u64) -> u64 {
    model_bytes
        .saturating_add(model_bytes / 4)
        .saturating_add(256 * MIB)
}

/// Why a server did not start, or a transcription could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpeechError {
    #[error("{0}")]
    Setup(String),
    #[error(
        "not enough memory for local speech-to-text: it needs about {needed_mib} MiB and \
         {available_mib} MiB is free beside the loaded models"
    )]
    NoRoom { needed_mib: u64, available_mib: u64 },
    #[error("{0}")]
    Start(String),
    #[error("{0}")]
    TimedOut(String),
}

/// Where the server runs and what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechPlacement {
    /// Offloaded to the GPU.
    pub gpu: bool,
    /// Memory reserved beside the language models.
    pub vram_bytes: u64,
    pub ram_bytes: u64,
}

/// Decides where a server needing `need` bytes runs, given the `room` left beside the loaded
/// models (`None` when no local models are supervised, so nothing is accounted) and whether it
/// may use the GPU at all. The GPU when the room there is enough, else the CPU.
pub fn place(
    need: u64,
    room: Option<Budget>,
    gpu_allowed: bool,
) -> Result<SpeechPlacement, SpeechError> {
    let Some(room) = room else {
        return Ok(SpeechPlacement {
            gpu: gpu_allowed,
            vram_bytes: 0,
            ram_bytes: 0,
        });
    };
    if gpu_allowed && room.unified && room.ram_bytes >= need {
        // One pool of memory: the GPU draws on the same RAM.
        return Ok(SpeechPlacement {
            gpu: true,
            vram_bytes: 0,
            ram_bytes: need,
        });
    }
    if gpu_allowed && !room.unified && room.vram_bytes >= need {
        return Ok(SpeechPlacement {
            gpu: true,
            vram_bytes: need,
            ram_bytes: 0,
        });
    }
    if room.ram_bytes >= need {
        return Ok(SpeechPlacement {
            gpu: false,
            vram_bytes: 0,
            ram_bytes: need,
        });
    }
    Err(SpeechError::NoRoom {
        needed_mib: need / MIB,
        available_mib: room
            .ram_bytes
            .max(if gpu_allowed { room.vram_bytes } else { 0 })
            / MIB,
    })
}

/// What the speech server asks of the memory accounting it sits beside (the language-model
/// supervisor).
pub trait Room: Send + Sync + fmt::Debug {
    /// The memory left beside the loaded models.
    fn room(&self) -> Budget;
    /// Completes once no language model is being loaded or swapped, so the room read next is
    /// not about to shrink.
    fn settled(&self) -> StopFuture<'_>;
    /// Holds `vram_bytes` and `ram_bytes` beside the models from now on, replacing what was
    /// held; `(0, 0)` gives it back.
    fn reserve(&self, vram_bytes: u64, ram_bytes: u64);
}

impl Room for Supervisor {
    fn room(&self) -> Budget {
        let budget = self.budget();
        let (vram, ram) = self
            .loaded()
            .iter()
            .fold((0u64, 0u64), |(vram, ram), model| {
                (vram + model.vram_bytes, ram + model.ram_bytes)
            });
        Budget {
            vram_bytes: budget.vram_bytes.saturating_sub(vram),
            ram_bytes: budget.ram_bytes.saturating_sub(ram),
            unified: budget.unified,
        }
    }

    fn settled(&self) -> StopFuture<'_> {
        Box::pin(Supervisor::settled(self))
    }

    fn reserve(&self, vram_bytes: u64, ram_bytes: u64) {
        self.set_side_load(vram_bytes, ram_bytes);
    }
}

/// Gives the reservation back when dropped while armed: a start or stop that is cancelled
/// half way must not leave memory held for a server that is not there.
struct Hold<'a> {
    server: &'a SpeechServer,
    armed: bool,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.server.reserve(0, 0);
        }
    }
}

/// What a server is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechLaunch {
    pub binary: PathBuf,
    pub model: PathBuf,
    pub gpu: bool,
    pub threads: u32,
    pub language: String,
    pub convert: bool,
    pub extra_args: Vec<String>,
    pub start_timeout: Duration,
    /// Where the server's output goes.
    pub log: PathBuf,
}

impl SpeechLaunch {
    /// The arguments after the program and before `--port` and `--host`: the model, the
    /// transcription route, the language, and what the settings add.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--model".to_string(),
            self.model.display().to_string(),
            "--inference-path".to_string(),
            INFERENCE_PATH.to_string(),
            "--language".to_string(),
            self.language.clone(),
        ];
        if self.threads > 0 {
            args.push("--threads".into());
            args.push(self.threads.to_string());
        }
        if self.convert {
            args.push("--convert".into());
        }
        if !self.gpu {
            args.push("--no-gpu".into());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

pub type StopFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
pub type LaunchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn SpeechProcess>, SpeechError>> + Send + 'a>>;

/// A running speech server.
pub trait SpeechProcess: Send + Sync {
    /// The OpenAI-compatible base URL, ending before `/audio/transcriptions`.
    fn base_url(&self) -> &str;
    fn pid(&self) -> Option<u32>;
    /// Why it is no longer running, when it exited on its own.
    fn exited(&self) -> Option<String>;
    fn stop(&self) -> StopFuture<'_>;
}

/// Starts speech servers; tests use one that serves from inside the process.
pub trait SpeechLauncher: Send + Sync + fmt::Debug {
    /// Starts a server and returns once it answers.
    fn launch<'a>(&'a self, spec: &'a SpeechLaunch) -> LaunchFuture<'a>;
}

/// Starts `whisper-server` as a child process.
#[derive(Debug, Default)]
pub struct ProcessSpeechLauncher {
    http: reqwest::Client,
}

impl ProcessSpeechLauncher {
    pub fn new() -> Self {
        Self::default()
    }

    async fn start(&self, spec: &SpeechLaunch) -> Result<Box<dyn SpeechProcess>, SpeechError> {
        let port = crate::process::free_port().map_err(SpeechError::Start)?;
        if let Some(dir) = spec.log.parent() {
            std::fs::create_dir_all(dir).map_err(|error| SpeechError::Start(error.to_string()))?;
        }
        let log = std::fs::File::create(&spec.log)
            .map_err(|error| SpeechError::Start(format!("{}: {error}", spec.log.display())))?;
        let mut command = Command::new(&spec.binary);
        command
            .args(spec.args())
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(
                log.try_clone()
                    .map_err(|error| SpeechError::Start(error.to_string()))?,
            )
            .stderr(log)
            .kill_on_drop(true);
        crate::process::library_path(&mut command, &spec.binary);
        let mut child = command.spawn().map_err(|error| {
            SpeechError::Setup(format!(
                "could not start {}: {error}; set voice.local.binary to a whisper.cpp \
                 whisper-server",
                spec.binary.display()
            ))
        })?;
        // Written down beside the language-model servers, so a daemon that dies without
        // stopping it does not leave it running: the next one's startup sweep ends it.
        if let (Some(pid), Some(logs)) = (child.id(), spec.log.parent()) {
            crate::ledger::record(logs, pid);
        }
        let root = format!("http://127.0.0.1:{port}");
        let ready = tokio::time::timeout(
            spec.start_timeout,
            self.wait_ready(&root, || match child.try_wait() {
                Ok(Some(status)) => Err(format!("whisper-server exited while loading ({status})")),
                Ok(None) => Ok(()),
                Err(error) => Err(error.to_string()),
            }),
        )
        .await;
        match ready {
            Ok(Ok(())) if matches!(child.try_wait(), Ok(Some(_))) => {
                Err(SpeechError::Start(format!(
                    "whisper-server exited right after it started; see {}",
                    spec.log.display()
                )))
            }
            Ok(Ok(())) => Ok(Box::new(ProcessSpeech {
                pid: child.id(),
                child: Mutex::new(Some(child)),
                base_url: format!("{root}/v1"),
            })),
            Ok(Err(message)) => Err(SpeechError::Start(format!(
                "{message}; see {}",
                spec.log.display()
            ))),
            Err(_) => Err(SpeechError::TimedOut(format!(
                "whisper-server did not load its model within {} s; see {}",
                spec.start_timeout.as_secs(),
                spec.log.display()
            ))),
        }
    }

    /// Polls until the server answers: `/health` where it has one, else its start page. `alive`
    /// says whether the server is still starting, or why it is not.
    async fn wait_ready(
        &self,
        root: &str,
        mut alive: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        loop {
            alive()?;
            for path in ["/health", "/"] {
                let answer = self.http.get(format!("{root}{path}")).send().await;
                if answer.is_ok_and(|response| response.status().is_success()) {
                    return Ok(());
                }
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

impl SpeechLauncher for ProcessSpeechLauncher {
    fn launch<'a>(&'a self, spec: &'a SpeechLaunch) -> LaunchFuture<'a> {
        Box::pin(self.start(spec))
    }
}

struct ProcessSpeech {
    child: Mutex<Option<Child>>,
    pid: Option<u32>,
    base_url: String,
}

impl SpeechProcess for ProcessSpeech {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn exited(&self) -> Option<String> {
        let mut child = lock(&self.child);
        match child.as_mut()?.try_wait() {
            Ok(Some(status)) => Some(format!("whisper-server exited ({status})")),
            Ok(None) | Err(_) => None,
        }
    }

    fn stop(&self) -> StopFuture<'_> {
        let child = lock(&self.child).take();
        Box::pin(async move {
            if let Some(mut child) = child
                && !matches!(child.try_wait(), Ok(Some(_)))
                && let Err(error) = child.kill().await
            {
                tracing::warn!(%error, "could not stop whisper-server");
            }
        })
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a [`SpeechServer`] runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechConfig {
    pub binary: PathBuf,
    pub model: PathBuf,
    /// May the server use the GPU at all.
    pub gpu: bool,
    pub threads: u32,
    pub language: String,
    pub convert: bool,
    pub extra_args: Vec<String>,
    /// Without a transcription for this long the server stops; zero keeps it running.
    pub idle_stop: Duration,
    pub start_timeout: Duration,
    /// The server's log file.
    pub log: PathBuf,
    /// How often the watchdog looks.
    pub watch: Duration,
}

struct Running {
    process: Arc<dyn SpeechProcess>,
    placement: SpeechPlacement,
    generation: u64,
    started: Instant,
    last_used: Instant,
    /// Leases held.
    active: u32,
    /// A call failed: the next one starts a fresh server.
    bad: bool,
}

#[derive(Default)]
struct State {
    running: Option<Running>,
    generation: u64,
    /// Why the last server stopped, for the status line.
    last_stop: Option<String>,
}

/// What the server is doing now, for status views and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub gpu: bool,
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    pub active: u32,
    pub idle_for: Option<Duration>,
    pub up_for: Option<Duration>,
    pub last_stop: Option<String>,
}

/// The supervised local whisper. Cheap to share: hand out `Arc<SpeechServer>`.
pub struct SpeechServer {
    config: SpeechConfig,
    launcher: Arc<dyn SpeechLauncher>,
    room: Mutex<Option<Weak<dyn Room>>>,
    state: Mutex<State>,
    /// Starts and stops happen one at a time.
    gate: tokio::sync::Mutex<()>,
    /// A task is waiting to stop the server for a language model.
    yielding: AtomicBool,
    /// The daemon is shutting down: nothing starts.
    closed: AtomicBool,
    /// No GPU placement until then: the server just left it for a language model.
    no_gpu_until: Mutex<Option<Instant>>,
    me: Weak<SpeechServer>,
}

impl fmt::Debug for SpeechServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpeechServer")
            .field("config", &self.config)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

/// A claim on the running server; while one is held the server is not stopped for being idle
/// or to make room.
pub struct SpeechLease {
    server: Arc<SpeechServer>,
    generation: u64,
    base_url: String,
}

impl SpeechLease {
    /// The base URL of the OpenAI-compatible API (`.../v1`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The call this lease served failed: the next one starts a fresh server.
    pub fn failed(&self) {
        let mut state = lock(&self.server.state);
        if let Some(running) = state.running.as_mut()
            && running.generation == self.generation
        {
            running.bad = true;
        }
    }
}

impl Drop for SpeechLease {
    fn drop(&mut self) {
        let mut state = lock(&self.server.state);
        if let Some(running) = state.running.as_mut()
            && running.generation == self.generation
        {
            running.active = running.active.saturating_sub(1);
            running.last_used = Instant::now();
        }
    }
}

impl SpeechServer {
    pub fn new(config: SpeechConfig, launcher: Arc<dyn SpeechLauncher>) -> Arc<Self> {
        Arc::new_cyclic(|me| SpeechServer {
            config,
            launcher,
            room: Mutex::new(None),
            state: Mutex::new(State::default()),
            gate: tokio::sync::Mutex::new(()),
            yielding: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            no_gpu_until: Mutex::new(None),
            me: Weak::clone(me),
        })
    }

    pub fn config(&self) -> &SpeechConfig {
        &self.config
    }

    /// Accounts the server's memory beside `supervisor`'s language models from now on, and lets
    /// the supervisor ask it to give the memory back. Safe to call again.
    pub fn attach(self: &Arc<Self>, supervisor: &Arc<Supervisor>) {
        let room: Arc<dyn Room> = supervisor.clone();
        *lock(&self.room) = Some(Arc::downgrade(&room));
        let yielder: Arc<dyn SideLoad> = self.clone();
        supervisor.set_side_yielder(Arc::downgrade(&yielder));
        // A server already running holds its memory from before.
        let held = lock(&self.state)
            .running
            .as_ref()
            .map(|running| running.placement);
        if let Some(held) = held {
            room.reserve(held.vram_bytes, held.ram_bytes);
        }
    }

    fn room(&self) -> Option<Arc<dyn Room>> {
        lock(&self.room).as_ref().and_then(Weak::upgrade)
    }

    fn reserve(&self, vram_bytes: u64, ram_bytes: u64) {
        if let Some(room) = self.room() {
            room.reserve(vram_bytes, ram_bytes);
        }
    }

    pub fn status(&self) -> SpeechStatus {
        let state = lock(&self.state);
        match state.running.as_ref() {
            Some(running) => SpeechStatus {
                running: true,
                pid: running.process.pid(),
                gpu: running.placement.gpu,
                vram_bytes: running.placement.vram_bytes,
                ram_bytes: running.placement.ram_bytes,
                active: running.active,
                idle_for: (running.active == 0).then(|| running.last_used.elapsed()),
                up_for: Some(running.started.elapsed()),
                last_stop: state.last_stop.clone(),
            },
            None => SpeechStatus {
                running: false,
                pid: None,
                gpu: false,
                vram_bytes: 0,
                ram_bytes: 0,
                active: 0,
                idle_for: None,
                up_for: None,
                last_stop: state.last_stop.clone(),
            },
        }
    }

    /// The server, started if it is not running, claimed until the lease is dropped.
    pub async fn acquire(self: &Arc<Self>) -> Result<SpeechLease, SpeechError> {
        let _gate = self.gate.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(SpeechError::Setup(
                "the daemon is shutting down; local speech-to-text is not available".into(),
            ));
        }
        if let Some(lease) = self.claim_running() {
            return Ok(lease);
        }
        // A server that exited, or whose last call failed and nobody holds, is replaced.
        self.stop_running("it was restarted after a failure").await;
        self.start().await
    }

    /// A lease on the running server, unless it exited or is marked bad with nobody using it.
    fn claim_running(self: &Arc<Self>) -> Option<SpeechLease> {
        let mut state = lock(&self.state);
        let running = state.running.as_mut()?;
        if running.process.exited().is_some() || (running.bad && running.active == 0) {
            return None;
        }
        running.active += 1;
        running.last_used = Instant::now();
        Some(SpeechLease {
            server: Arc::clone(self),
            generation: running.generation,
            base_url: running.process.base_url().to_string(),
        })
    }

    async fn start(self: &Arc<Self>) -> Result<SpeechLease, SpeechError> {
        let config = &self.config;
        let model_bytes = tokio::fs::metadata(&config.model)
            .await
            .map_err(|error| {
                SpeechError::Setup(format!(
                    "the whisper model {} cannot be read ({error}); set voice.local.model",
                    config.model.display()
                ))
            })?
            .len();
        let room = self.room();
        if let Some(room) = &room {
            // A model mid-load has not taken its memory yet: look after it has.
            room.settled().await;
        }
        let cooling = lock(&self.no_gpu_until).is_some_and(|until| Instant::now() < until);
        let placement = place(
            footprint(model_bytes),
            room.map(|room| room.room()),
            config.gpu && !cooling,
        )?;
        // Held before the model loads, so a language model planned meanwhile sees it; given
        // back if this start fails or is cancelled.
        self.reserve(placement.vram_bytes, placement.ram_bytes);
        let mut hold = Hold {
            server: self,
            armed: true,
        };
        let spec = SpeechLaunch {
            binary: config.binary.clone(),
            model: config.model.clone(),
            gpu: placement.gpu,
            threads: config.threads,
            language: config.language.clone(),
            convert: config.convert,
            extra_args: config.extra_args.clone(),
            start_timeout: config.start_timeout,
            log: config.log.clone(),
        };
        let process: Arc<dyn SpeechProcess> = match self.launcher.launch(&spec).await {
            Ok(process) => Arc::from(process),
            Err(error) => {
                lock(&self.state).last_stop = Some(error.to_string());
                return Err(error);
            }
        };
        let now = Instant::now();
        let (generation, lease) = {
            let mut state = lock(&self.state);
            state.generation += 1;
            let generation = state.generation;
            state.running = Some(Running {
                process: Arc::clone(&process),
                placement,
                generation,
                started: now,
                last_used: now,
                active: 1,
                bad: false,
            });
            state.last_stop = None;
            (
                generation,
                SpeechLease {
                    server: Arc::clone(self),
                    generation,
                    base_url: process.base_url().to_string(),
                },
            )
        };
        tracing::info!(
            gpu = placement.gpu,
            pid = ?process.pid(),
            vram_mib = placement.vram_bytes / MIB,
            ram_mib = placement.ram_bytes / MIB,
            "local speech-to-text started"
        );
        hold.armed = false;
        self.watch(generation);
        Ok(lease)
    }

    /// Stops the running server (if any) and gives its memory back. Call with the gate held.
    async fn stop_running(&self, why: &str) {
        let running = {
            let mut state = lock(&self.state);
            let running = state.running.take();
            if running.is_some() {
                state.last_stop = Some(why.to_string());
            }
            running
        };
        let Some(running) = running else {
            return;
        };
        // Its memory is held until the process is gone (or the stop is cancelled, which drops
        // the child and kills it).
        let _hold = Hold {
            server: self,
            armed: true,
        };
        running.process.stop().await;
        tracing::info!(reason = why, "local speech-to-text stopped");
    }

    /// Stops the server for good, in use or not: the daemon is shutting down.
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _gate = self.gate.lock().await;
        self.stop_running("the daemon stopped").await;
    }

    /// Stops the server now if nothing is using it; returns whether it stopped.
    pub async fn stop_if_unused(&self, why: &str) -> bool {
        let _gate = self.gate.lock().await;
        let unused = lock(&self.state)
            .running
            .as_ref()
            .is_some_and(|running| running.active == 0);
        if unused {
            self.stop_running(why).await;
        }
        unused
    }

    /// Looks after the server of `generation` until it stops: noticing an exit, and stopping
    /// it when idle.
    fn watch(&self, generation: u64) {
        let me = Weak::clone(&self.me);
        let every = self.config.watch;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(server) = me.upgrade() else {
                    return;
                };
                if !server.look(generation).await {
                    return;
                }
            }
        });
    }

    /// One look of the watchdog; false when there is nothing left to watch.
    async fn look(&self, generation: u64) -> bool {
        enum Verdict {
            Gone,
            Fine,
            Exited(String),
            Idle,
        }
        let idle_stop = self.config.idle_stop;
        let verdict = {
            let state = lock(&self.state);
            match state.running.as_ref() {
                Some(running) if running.generation == generation => {
                    if let Some(why) = running.process.exited() {
                        Verdict::Exited(why)
                    } else if !idle_stop.is_zero()
                        && running.active == 0
                        && running.last_used.elapsed() >= idle_stop
                    {
                        Verdict::Idle
                    } else {
                        Verdict::Fine
                    }
                }
                _ => Verdict::Gone,
            }
        };
        match verdict {
            Verdict::Gone => false,
            Verdict::Fine => true,
            Verdict::Exited(why) => {
                let _gate = self.gate.lock().await;
                if self.is_current(generation) {
                    self.stop_running(&why).await;
                }
                false
            }
            Verdict::Idle => {
                let _gate = self.gate.lock().await;
                // Used meanwhile, or replaced: look again.
                let still_idle = lock(&self.state).running.as_ref().is_some_and(|running| {
                    running.generation == generation
                        && running.active == 0
                        && running.last_used.elapsed() >= idle_stop
                });
                if still_idle {
                    self.stop_running("it was idle").await;
                    return false;
                }
                self.is_current(generation)
            }
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        lock(&self.state)
            .running
            .as_ref()
            .is_some_and(|running| running.generation == generation)
    }
}

impl SideLoad for SpeechServer {
    /// A language model needs the memory: stop as soon as nothing is transcribing.
    fn yield_now(&self) {
        let (running, generation) = {
            let state = lock(&self.state);
            (
                state.running.is_some(),
                state.running.as_ref().map(|running| running.generation),
            )
        };
        if !running || self.yielding.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Weak::clone(&self.me);
        tokio::spawn(async move {
            for _ in 0..600 {
                let Some(server) = me.upgrade() else {
                    return;
                };
                let done = !server.is_current_or_none(generation)
                    || server
                        .stop_if_unused("it made room for a language model")
                        .await;
                if done {
                    if !server.status().running {
                        *lock(&server.no_gpu_until) = Some(Instant::now() + GPU_COOLDOWN);
                    }
                    server.yielding.store(false, Ordering::SeqCst);
                    return;
                }
                drop(server);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if let Some(server) = me.upgrade() {
                server.yielding.store(false, Ordering::SeqCst);
            }
        });
    }
}

impl SpeechServer {
    /// Whether the server of `generation` (or any, when `None`) is still the one running.
    fn is_current_or_none(&self, generation: Option<u64>) -> bool {
        match generation {
            Some(generation) => self.is_current(generation),
            None => lock(&self.state).running.is_some(),
        }
    }
}

/// The `whisper-server` to run: `configured` when set, else the one found on `PATH`. A bare
/// name is never returned, so a program in the current folder is not run by accident.
pub fn find_binary(configured: Option<&Path>) -> Result<PathBuf, SpeechError> {
    if let Some(path) = configured.filter(|path| !path.as_os_str().is_empty()) {
        return Ok(path.to_path_buf());
    }
    let names: &[&str] = if cfg!(windows) {
        &["whisper-server.exe", "whisper-server"]
    } else {
        &["whisper-server"]
    };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(SpeechError::Setup(
        "whisper-server was not found on PATH; set voice.local.binary to a whisper.cpp \
         whisper-server"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    fn room(vram_gib: u64, ram_gib: u64) -> Budget {
        Budget {
            vram_bytes: vram_gib * GIB,
            ram_bytes: ram_gib * GIB,
            unified: false,
        }
    }

    #[test]
    fn the_footprint_grows_with_the_model() {
        assert_eq!(footprint(0), 256 * MIB);
        assert_eq!(footprint(400 * MIB), 400 * MIB + 100 * MIB + 256 * MIB);
    }

    #[test]
    fn it_takes_the_gpu_when_it_fits_and_the_cpu_when_it_does_not() {
        let need = footprint(150 * MIB);
        let gpu = place(need, Some(room(2, 8)), true).unwrap();
        assert_eq!(
            gpu,
            SpeechPlacement {
                gpu: true,
                vram_bytes: need,
                ram_bytes: 0
            }
        );
        let cpu = place(need, Some(room(0, 8)), true).unwrap();
        assert_eq!(
            cpu,
            SpeechPlacement {
                gpu: false,
                vram_bytes: 0,
                ram_bytes: need
            }
        );
        // The GPU turned off in the settings stays off whatever the room.
        assert!(!place(need, Some(room(8, 8)), false).unwrap().gpu);
    }

    #[test]
    fn no_room_anywhere_is_refused_plainly() {
        let error = place(footprint(GIB), Some(room(0, 1)), true).unwrap_err();
        assert!(
            matches!(error, SpeechError::NoRoom { needed_mib, available_mib }
                if needed_mib > available_mib),
            "{error}"
        );
        assert!(error.to_string().contains("not enough memory"), "{error}");
    }

    #[test]
    fn unified_memory_is_one_pool() {
        let need = footprint(150 * MIB);
        let unified = Budget {
            vram_bytes: 0,
            ram_bytes: 4 * GIB,
            unified: true,
        };
        let placed = place(need, Some(unified), true).unwrap();
        assert!(placed.gpu);
        assert_eq!((placed.vram_bytes, placed.ram_bytes), (0, need));
    }

    #[test]
    fn without_supervised_models_nothing_is_accounted() {
        let placed = place(footprint(GIB), None, true).unwrap();
        assert_eq!(
            placed,
            SpeechPlacement {
                gpu: true,
                vram_bytes: 0,
                ram_bytes: 0
            }
        );
    }

    #[test]
    fn the_arguments_serve_the_openai_route_and_follow_the_settings() {
        let mut launch = SpeechLaunch {
            binary: PathBuf::from("whisper-server"),
            model: PathBuf::from("ggml-base.en.bin"),
            gpu: true,
            threads: 0,
            language: "auto".into(),
            convert: false,
            extra_args: Vec::new(),
            start_timeout: Duration::from_secs(1),
            log: PathBuf::from("whisper.log"),
        };
        let args = launch.args();
        assert_eq!(
            args,
            [
                "--model",
                "ggml-base.en.bin",
                "--inference-path",
                INFERENCE_PATH,
                "--language",
                "auto"
            ]
        );
        launch.gpu = false;
        launch.threads = 4;
        launch.convert = true;
        launch.extra_args = vec!["--no-timestamps".into()];
        let args = launch.args();
        assert!(args.windows(2).any(|pair| pair == ["--threads", "4"]));
        for flag in ["--convert", "--no-gpu", "--no-timestamps"] {
            assert!(args.iter().any(|arg| arg == flag), "{flag} in {args:?}");
        }
    }

    #[test]
    fn a_configured_binary_is_used_as_given() {
        assert_eq!(
            find_binary(Some(Path::new("/opt/whisper/whisper-server"))).unwrap(),
            PathBuf::from("/opt/whisper/whisper-server")
        );
    }

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        root
    }

    #[tokio::test]
    async fn readiness_waits_until_the_health_check_passes() {
        use axum::http::StatusCode;
        use axum::routing::get;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        // Loading the model: 503 on the first few looks.
        let app = axum::Router::new().route(
            "/health",
            get(move || {
                let counter = Arc::clone(&counter);
                async move {
                    if counter.fetch_add(1, Ordering::SeqCst) < 2 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    }
                }
            }),
        );
        let root = serve(app).await;
        let launcher = ProcessSpeechLauncher::new();
        launcher.wait_ready(&root, || Ok(())).await.unwrap();
        assert!(
            hits.load(Ordering::SeqCst) >= 3,
            "kept looking while loading"
        );
    }

    #[tokio::test]
    async fn readiness_stops_looking_when_the_server_is_gone() {
        let launcher = ProcessSpeechLauncher::new();
        // Nothing listens there; the exit is reported at once rather than at the timeout.
        let error = launcher
            .wait_ready("http://127.0.0.1:9", || {
                Err("whisper-server exited while loading (exit code: 1)".into())
            })
            .await
            .unwrap_err();
        assert!(error.contains("exited while loading"), "{error}");
    }

    #[tokio::test]
    async fn a_missing_binary_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let spec = SpeechLaunch {
            binary: dir.path().join("does-not-exist"),
            model: dir.path().join("model.bin"),
            gpu: false,
            threads: 0,
            language: "en".into(),
            convert: false,
            extra_args: Vec::new(),
            start_timeout: Duration::from_secs(1),
            log: dir.path().join("logs").join("whisper.log"),
        };
        let error = match ProcessSpeechLauncher::new().launch(&spec).await {
            Ok(_) => panic!("started a binary that is not there"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("voice.local.binary"),
            "names the setting: {error}"
        );
    }

    /// A stand-in `whisper-server` that runs for a few seconds and never serves.
    #[cfg(windows)]
    fn silent_server(dir: &Path) -> PathBuf {
        let path = dir.join("whisper-server.cmd");
        std::fs::write(
            &path,
            "@echo off
ping -n 4 127.0.0.1 >nul
",
        )
        .unwrap();
        path
    }

    /// A stand-in `whisper-server` that runs for a few seconds and never serves.
    #[cfg(unix)]
    fn silent_server(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("whisper-server");
        std::fs::write(
            &path,
            "#!/bin/sh
exec sleep 4
",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn a_started_server_is_written_in_the_children_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let spec = SpeechLaunch {
            binary: silent_server(dir.path()),
            model: dir.path().join("model.bin"),
            gpu: false,
            threads: 0,
            language: "en".into(),
            convert: false,
            extra_args: Vec::new(),
            start_timeout: Duration::from_millis(500),
            log: logs.join("whisper.log"),
        };
        // It never answers, so the start times out; it was running, and written down, by then.
        assert!(ProcessSpeechLauncher::new().launch(&spec).await.is_err());
        let ledger = std::fs::read_to_string(logs.join("children.txt")).expect("a ledger");
        let line = ledger.lines().next().expect("one server written down");
        let mut parts = line.splitn(3, ' ');
        assert!(parts.next().unwrap().parse::<u32>().is_ok(), "its id");
        assert!(
            parts.next().unwrap().parse::<u64>().is_ok(),
            "its start time"
        );
        assert!(!parts.next().unwrap().is_empty(), "its name");
        assert_eq!(ledger.lines().count(), 1);
    }
}
