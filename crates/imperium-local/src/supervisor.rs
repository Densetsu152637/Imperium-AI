//! Loads local models into supervised runtime servers, hands out slots on them, and swaps them
//! in and out of memory.
//!
//! Each model is served by the first available [`RuntimeBackend`] that accepts its format
//! (`llama-server` for GGUF, `vllm serve` for Hugging Face folders by default).
//! Each loaded model is one server started with `--parallel <slots>` and one KV cache pool of
//! `pool_tokens` shared by its slots (`--kv-unified`). An agent takes a slot (a semaphore permit
//! and a slot index) and a reservation of tokens from the pool for one model call and releases
//! both when the call ends, so agents that are running tools or waiting hold none; callers queue
//! first-come, first-served while every slot is busy or the pool is full, and an agent gets back
//! the slot it used last when that slot is free. The reservation bounds what the call may use
//! (the lease carries the `max_tokens` to send), so a full pool makes calls wait, not fail.
//!
//! One agent may use the whole pool, up to the model's context ceiling (see [`crate::context`]).
//! A pool no setting fixes starts small (or at what the call loading the model needs) and grows
//! on demand: a call whose prompt and reply do not fit makes the model restart with the pool
//! doubled, once the calls running on it are done, even while agents are mid-turn on it (their
//! slots are saved and restored). An unloaded model forgets its grown pool.
//!
//! The supervisor is the one memory arbiter for every session. Memory is reserved per loaded
//! model against the budget measured when it started. A model that does not fit beside the
//! loaded ones swaps out models nobody is using: no slot held, no call queued, and no agent of
//! another session mid-turn on it ([`crate::arbiter`] decides which, and when to wait instead), saving to disk the slots of their agents that can still come back; an
//! agent whose slot was saved gets it restored into its model before its next call. Calls for a
//! model that is not loaded wait in arrival order, and a loaded model takes on new agents only
//! while no earlier call for another model is waiting, so work is batched by model and nothing
//! waits longer than the batch ahead of it. Loads and unloads run one at a time, swaps are
//! limited per minute (past the limit a call that needs one waits for the minute), and no call
//! waits longer than `max_wait_s`.
//!
//! The supervisor carries out residency decisions; it does not make them on its own. It has no
//! timer and starts no task that unloads anything: idle unloads, settings restarts, explicit
//! unloads, a resuming agent's context and the memory budget are ordered by `imperium-core`'s
//! residency controller through [`Supervisor::execute`], and a call that needs room asks the
//! installed [`ResidencyPolicy`] what to swap out. Every move is reported with its reason
//! ([`Supervisor::moves`], see [`crate::residency`]).
//!
//! A model can be pinned to the CPU lane ([`Supervisor::set_cpu_pinned`]): it is served with no
//! GPU layers, its weights, KV cache and prompt cache count against the RAM budget alone, and it
//! is never a victim of a swap made for VRAM (see [`crate::arbiter`]); it serves one call at a
//! time unless configured otherwise. The speed each model has been measured generating at is
//! kept for the listing and for routing ([`LocalModels::tokens_per_s`]).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use imperium_api::{
    AcquireError, AcquireFuture, Availability, Load as MachineLoad, LoadView, LoadedFuture,
    LocalLease, LocalModels, LocalPresence, LocalRequest, LocalWait, Priority, ResumeFuture,
    SuspendFuture, holder_session,
};
pub use imperium_config::{CPU_LANE_MAX_WAIT_S, CPU_LANE_SLOTS};
use imperium_config::{LaneRow, LlamaCppSettings, LocalModelSettings, ModelLaunch, VllmSettings};
use tokio::sync::{Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore, broadcast};

use crate::arbiter::{self, Decision, Outlook, Policy, Resident, Swaps, Use};
use crate::backend::{self, RuntimeBackend};
use crate::calibration::{self, Calibration, Lane};
use crate::context;
use crate::hardware::Hardware;
use crate::planner::{self, Budget, Footprint, Placement, prompt_cache_mb};
use crate::residency::{Action, LoadAsk, Move, Reason, ResidencyPolicy, Standard};
use crate::scan::{Format, LocalModel};
use crate::slot_files::{self, Saved, SlotFiles};
use crate::speculative::{self, Draft, Drafting};

const MIB: u64 = 1024 * 1024;

/// How often a call waiting for room looks again when nothing has changed, for waits that end
/// with time: another model's residency, the swap limit's window.
const RECHECK: Duration = Duration::from_millis(250);
/// How long after an interactive call last used a model background work may not swap it out,
/// unless [`Supervisor::set_background_policy`] says otherwise.
const DEFAULT_INTERACTIVE_HOLD: Duration = Duration::from_secs(5 * 60);
/// The window `max_swaps_per_min` counts over.
const SWAP_WINDOW: Duration = Duration::from_secs(60);
/// A crash this long after the previous one starts a new streak (see
/// [`crate::residency::crash_backoff`]).
const CRASH_STREAK: Duration = Duration::from_secs(10 * 60);
/// Load speed assumed for a model that has not been loaded yet, to estimate a swap.
const ASSUMED_LOAD_BYTES_PER_S: u64 = 2 * 1024 * MIB;
/// How a model pinned to the CPU lane is served (from its model-table row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuLane {
    /// Slots (`--parallel`).
    pub slots: u32,
    /// Context per slot; `None` takes `runtime.llamacpp.context`.
    pub context: Option<u32>,
    /// Longest a call waits for a slot or for room; `None` takes the longer of
    /// [`CPU_LANE_MAX_WAIT_S`] and `runtime.llamacpp.max_wait_s`.
    pub max_wait_s: Option<u64>,
}

impl Default for CpuLane {
    fn default() -> Self {
        CpuLane {
            slots: CPU_LANE_SLOTS,
            context: None,
            max_wait_s: None,
        }
    }
}

impl CpuLane {
    fn row(self) -> LaneRow {
        LaneRow {
            slots: Some(self.slots.max(1)),
            context: self.context,
            max_wait_s: self.max_wait_s,
        }
    }
}
/// How much of the earlier calls' tokens and time a new call's measurement keeps: the speed
/// follows the model's recent calls, and long generations weigh more than short ones.
const THROUGHPUT_KEEP: f64 = 0.8;

/// The draft model a server is started with for speculative decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSpec {
    pub path: PathBuf,
    /// `--spec-draft-n-max`; `None` keeps llama.cpp's default.
    pub max: Option<u32>,
    /// `--spec-draft-n-min`; `None` keeps llama.cpp's default.
    pub min: Option<u32>,
    /// `--spec-draft-ngl`; `None` lets llama.cpp place every layer.
    pub gpu_layers: Option<u32>,
}

/// What a server is started with.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// The runtime that serves the model.
    pub backend: Arc<dyn RuntimeBackend>,
    pub format: Format,
    pub model_id: String,
    pub model_path: PathBuf,
    pub mmproj: Option<PathBuf>,
    /// Tokens one session (one slot) may use.
    pub context: u32,
    /// Tokens of KV cache all slots share (`--ctx-size`).
    pub pool_tokens: u32,
    pub slots: u32,
    pub gpu_layers: u32,
    /// The model is served on the CPU alone: no GPU device is used at all, not even for
    /// compute buffers or offloaded operations (llama.cpp's `--device none`).
    pub cpu_only: bool,
    pub cache_type_k: String,
    pub cache_type_v: String,
    /// llama.cpp's `--flash-attn`: `auto`, `on`, or `off`.
    pub flash_attn: String,
    /// MiB of host memory for llama.cpp's prompt cache (`--cache-ram`), which keeps the prompts
    /// of slots that were reused by another agent so they need not be read again; 0 disables it.
    pub cache_ram_mb: u64,
    /// The folder slots are saved to before the model is swapped out (`--slot-save-path`);
    /// `None` when they are not saved.
    pub slot_save_path: Option<PathBuf>,
    /// vLLM's `--gpu-memory-utilization` (the share of GPU memory it may take).
    pub gpu_memory_utilization: f64,
    /// The draft model for speculative decoding (llama.cpp only).
    pub draft: Option<DraftSpec>,
    pub extra_args: Vec<String>,
}

impl LaunchSpec {
    /// The runtime's arguments, without the port.
    pub fn args(&self) -> Vec<String> {
        self.backend.launch_args(self)
    }
}

/// Specs are equal when every field is and they name the same runtime.
impl PartialEq for LaunchSpec {
    fn eq(&self, other: &Self) -> bool {
        self.backend.id() == other.backend.id()
            && self.format == other.format
            && self.model_id == other.model_id
            && self.model_path == other.model_path
            && self.mmproj == other.mmproj
            && self.context == other.context
            && self.pool_tokens == other.pool_tokens
            && self.slots == other.slots
            && self.gpu_layers == other.gpu_layers
            && self.cpu_only == other.cpu_only
            && self.cache_type_k == other.cache_type_k
            && self.cache_type_v == other.cache_type_v
            && self.flash_attn == other.flash_attn
            && self.cache_ram_mb == other.cache_ram_mb
            && self.slot_save_path == other.slot_save_path
            && self.gpu_memory_utilization == other.gpu_memory_utilization
            && self.draft == other.draft
            && self.extra_args == other.extra_args
    }
}

/// What saving or restoring a slot moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlotTransfer {
    pub tokens: u64,
    pub bytes: u64,
}

pub type SlotFuture<'a> = Pin<Box<dyn Future<Output = Result<SlotTransfer, String>> + Send + 'a>>;
pub type StopFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
pub type SpeculatingFuture<'a> = Pin<Box<dyn Future<Output = Option<bool>> + Send + 'a>>;

/// A running model server; dropping it stops the server.
pub trait Server: Send + Sync {
    /// OpenAI-compatible base URL, ending in `/v1`.
    fn base_url(&self) -> &str;

    /// The server's process id, for the resource monitor; `None` for in-process servers.
    fn pid(&self) -> Option<u32> {
        None
    }

    /// How the server ended, once it has exited on its own (crashed, killed from outside);
    /// `None` while it runs, and for servers that cannot tell.
    fn exited(&self) -> Option<String> {
        None
    }

    /// Saves `slot`'s cached prompt to `file` in the server's [`LaunchSpec::slot_save_path`].
    fn save_slot<'a>(&'a self, _slot: u32, _file: &'a str) -> SlotFuture<'a> {
        Box::pin(async { Err("this runtime cannot save slots".to_string()) })
    }

    /// Loads `file` from the server's [`LaunchSpec::slot_save_path`] into `slot`.
    fn restore_slot<'a>(&'a self, _slot: u32, _file: &'a str) -> SlotFuture<'a> {
        Box::pin(async { Err("this runtime cannot restore slots".to_string()) })
    }

    /// Frees `slot`'s KV cells (its prompt is gone from the server until restored).
    fn erase_slot(&self, _slot: u32) -> SlotFuture<'_> {
        Box::pin(async { Err("this runtime cannot erase slots".to_string()) })
    }

    /// Whether the running server does speculative decoding: `Some(false)` when it started
    /// with a draft model but could not set it up and runs without. `None` when it cannot say.
    fn speculating(&self) -> SpeculatingFuture<'_> {
        Box::pin(async { None })
    }

    /// Stops the server and returns once it has exited, so its memory is free before the next
    /// model loads.
    fn stop(&self) -> StopFuture<'_> {
        Box::pin(async {})
    }
}

pub type LaunchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn Server>, LaunchError>> + Send + 'a>>;

/// Why a server did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchError {
    pub message: String,
    /// It was still loading when the launcher gave up, so trying again would wait as long.
    pub timed_out: bool,
}

impl LaunchError {
    pub fn timed_out(message: String) -> Self {
        LaunchError {
            message,
            timed_out: true,
        }
    }
}

/// A failure that came quickly, such as the process exiting or not starting.
impl From<String> for LaunchError {
    fn from(message: String) -> Self {
        LaunchError {
            message,
            timed_out: false,
        }
    }
}

impl From<&str> for LaunchError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Starts servers; [`ProcessLauncher`] runs the spec's runtime as a process, tests use
/// in-process fakes.
///
/// [`ProcessLauncher`]: crate::process::ProcessLauncher
pub trait Launcher: Send + Sync + fmt::Debug {
    /// Starts a server and returns once it is ready to serve requests.
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a>;

    /// The folder its servers save slots to when their model is swapped out; `None` when slots
    /// are not saved, so a model swapped back in reads its agents' prompts again.
    fn slot_dir(&self) -> Option<PathBuf> {
        None
    }
}

/// Settings for the built-in runtimes other than llama.cpp ([`crate::backend::builtin`]).
#[derive(Debug, Clone, Default)]
pub struct Runtimes {
    pub vllm: VllmSettings,
    /// vLLM is supported on Linux (including WSL2); elsewhere Hugging Face models are listed
    /// but cannot be loaded.
    pub vllm_available: bool,
}

/// A loaded model as the status view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedStatus {
    pub model_id: String,
    /// The runtime serving it (`llamacpp`, `vllm`, ...).
    pub runtime: String,
    pub base_url: String,
    /// The server's process id, when it runs as a process.
    pub pid: Option<u32>,
    pub slots: u32,
    pub slots_in_use: u32,
    /// Calls waiting for a slot.
    pub queued: u32,
    pub gpu_layers: u32,
    pub all_layers_on_gpu: bool,
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    /// How long it has been unused (no slot in use, no call queued, no agent mid-turn on it);
    /// `None` while it is in use.
    pub idle_for: Option<Duration>,
    /// When it will be unloaded if it stays idle; `None` while it is in use.
    pub unload_in: Option<Duration>,
    /// An agent is mid-turn on it (between its calls): it is not unloaded until the turn ends.
    pub claimed: bool,
    /// Pinned to the CPU lane.
    pub cpu_pinned: bool,
    /// Tokens one session may use.
    pub context: u32,
    /// Tokens of KV cache shared by the slots.
    pub pool_tokens: u32,
    /// Of those, tokens reserved by calls running now.
    pub pool_reserved_tokens: u32,
    /// Started with settings that have changed since; it restarts once nothing is using it.
    pub stale: bool,
    /// The draft model it was started with for speculative decoding, if any.
    pub draft: Option<String>,
    /// Why a configured draft model is not in use, if so (it runs without it).
    pub draft_notice: Option<String>,
    /// Its server exited on its own: calls wait until it is unloaded ([`Reason::Crashed`]) and
    /// loaded again.
    pub crashed: bool,
}

/// Why [`Supervisor::unload`] did not unload a model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnloadError {
    #[error("{0} is not loaded")]
    NotLoaded(String),
    #[error("{0} is in use by {1} slot(s); it unloads once they are released")]
    InUse(String, u32),
    #[error("{0} has calls queued for a slot; it unloads once they are served")]
    Queued(String),
    #[error("an agent is mid-turn on {0}; it unloads once the turn ends")]
    Claimed(String),
    #[error("{0} was used again since it went idle")]
    Used(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    Out,
    In,
}

/// A model swapped out or in, for telemetry ([`Supervisor::swap_events`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SwapEvent {
    pub direction: SwapDirection,
    pub model_id: String,
    /// Out: saving its slots and stopping it. In: starting it.
    pub seconds: f64,
    /// Out: slot state written to disk. In: memory it took.
    pub bytes: u64,
    /// Out: slots saved. In: saved slots its agents will get back.
    pub slots: u32,
    /// Out: `room`, `idle`, `config` (its settings changed), `grow` (a call needs a larger KV
    /// pool), `promote`, `request` (`models.unload`), or `crashed` (its server exited).
    /// In: `swap` (it replaced a
    /// model) or `restore` (it came back to saved slots).
    pub reason: &'static str,
}

/// Which slots of a server are free, and who used each last.
#[derive(Debug)]
struct SlotTable {
    /// Free slots, the one free the longest first.
    free: VecDeque<u32>,
    /// Who held each slot last; its cached prompt is that holder's until someone else takes it.
    last_holder: Vec<Option<String>>,
    /// Slots kept for a holder whose prompt is saved on disk, not in the slot yet.
    reserved: Vec<bool>,
}

impl SlotTable {
    fn new(slots: u32) -> Self {
        SlotTable {
            free: (0..slots).collect(),
            last_holder: vec![None; slots as usize],
            reserved: vec![false; slots as usize],
        }
    }

    /// The free slot `holder` used last, if nobody has used it since; otherwise the one that
    /// has been free the longest, so the prompts most recently used survive the longest, and
    /// one kept for another holder's saved prompt only when no other is free.
    fn take(&mut self, holder: &str) -> Option<u32> {
        let own = self
            .free
            .iter()
            .position(|&slot| self.last_holder[slot as usize].as_deref() == Some(holder));
        let unreserved = || {
            self.free
                .iter()
                .position(|&slot| !self.reserved[slot as usize])
        };
        let slot = self.free.remove(own.or_else(unreserved).unwrap_or(0))?;
        self.reserved[slot as usize] = false;
        Some(slot)
    }

    /// Takes the free slot holding `holder`'s prompt, if one does, to suspend it.
    fn take_own(&mut self, holder: &str) -> Option<u32> {
        let at = self.free.iter().position(|&slot| {
            !self.reserved[slot as usize]
                && self.last_holder[slot as usize].as_deref() == Some(holder)
        })?;
        self.free.remove(at)
    }

    /// Returns a slot taken by [`SlotTable::take_own`]: when its cells were freed it holds
    /// nobody's prompt and is the first one handed out; otherwise it is `holder`'s as before.
    fn give_back_suspended(&mut self, slot: u32, holder: &str, freed: bool) {
        if freed {
            self.last_holder[slot as usize] = None;
            self.free.push_front(slot);
        } else {
            self.give_back(slot, holder);
        }
    }

    fn give_back(&mut self, slot: u32, holder: &str) {
        self.last_holder[slot as usize] = Some(holder.to_string());
        self.free.push_back(slot);
    }

    /// Keeps `slot` for `holder`, whose prompt from it was saved before the model was last
    /// swapped out, unless the slot is already someone's.
    fn reserve(&mut self, slot: u32, holder: &str) -> bool {
        let Some(last) = self.last_holder.get_mut(slot as usize) else {
            return false;
        };
        if last.is_some() {
            return false;
        }
        *last = Some(holder.to_string());
        self.reserved[slot as usize] = true;
        true
    }

    /// Slots holding a holder's prompt, to save before the model is swapped out.
    fn occupied(&self) -> Vec<(u32, String)> {
        (0..)
            .zip(&self.last_holder)
            .filter(|&(slot, _)| !self.reserved[slot as usize])
            .filter_map(|(slot, holder)| Some((slot, holder.clone()?)))
            .collect()
    }
}

/// The slots of one running server, shared by its leases and waiters.
struct SlotPool {
    base_url: String,
    slots: u32,
    /// Tokens one session may use.
    context: u32,
    /// Tokens of KV cache the slots share.
    pool_tokens: u32,
    /// Most tokens a reply may generate before it is retried uncapped; 0 = no cap. Only
    /// reservations use it, so a settings change applies without a restart.
    max_output_tokens: AtomicU32,
    /// One permit per token of the pool, queueing waiters first-come, first-served. Only for
    /// runtimes that address slots (llama.cpp): vLLM manages its own paged KV cache.
    tokens: Option<Arc<Semaphore>>,
    /// One permit per free slot; the semaphore queues waiters first-come, first-served.
    permits: Arc<Semaphore>,
    table: Mutex<SlotTable>,
    /// Calls waiting for a permit.
    waiting: AtomicU32,
    /// Of those, the user's own (background calls wait outside the queue, behind them).
    waiting_interactive: AtomicU32,
    /// Slots held by the user's own calls.
    held_interactive: AtomicU32,
    /// When a slot held by one of the user's calls was last taken or released.
    last_interactive: Mutex<Option<Instant>>,
    /// Whether leases carry a slot index the runtime accepts in requests.
    pins_slots: bool,
    last_used: Mutex<Instant>,
    /// The supervisor's change signal, raised when a slot is released.
    changed: Arc<Notify>,
}

impl SlotPool {
    fn new(
        base_url: String,
        launch: &ModelLaunch,
        pins_slots: bool,
        changed: Arc<Notify>,
    ) -> Arc<Self> {
        let slots = launch.slots;
        Arc::new(SlotPool {
            base_url,
            slots,
            context: launch.context,
            pool_tokens: launch.pool_tokens,
            max_output_tokens: AtomicU32::new(launch.max_output_tokens),
            tokens: pins_slots.then(|| Arc::new(Semaphore::new(launch.pool_tokens as usize))),
            permits: Arc::new(Semaphore::new(slots as usize)),
            table: Mutex::new(SlotTable::new(slots)),
            waiting: AtomicU32::new(0),
            waiting_interactive: AtomicU32::new(0),
            held_interactive: AtomicU32::new(0),
            last_interactive: Mutex::new(None),
            pins_slots,
            last_used: Mutex::new(Instant::now()),
            changed,
        })
    }

    fn in_use(&self) -> u32 {
        self.slots - u32::try_from(self.permits.available_permits()).unwrap_or(self.slots)
    }

    /// Tokens reserved by calls running now.
    fn reserved_tokens(&self) -> u32 {
        self.tokens.as_ref().map_or(0, |tokens| {
            self.pool_tokens
                .saturating_sub(u32::try_from(tokens.available_permits()).unwrap_or(u32::MAX))
        })
    }

    /// What a call of `request` reserves from the pool, and the `max_tokens` it must send.
    fn reservation(&self, request: &LocalRequest) -> Reservation {
        let whole = self.context.min(self.pool_tokens).max(1);
        let max_output_tokens = self.max_output_tokens.load(Ordering::SeqCst);
        match request.prompt_tokens {
            Some(prompt) if !request.uncapped && max_output_tokens > 0 => {
                let reserve = prompt.saturating_add(max_output_tokens).min(whole);
                // A prompt that already fills the context leaves no room to cap: the server's
                // own over-context error is the answer.
                let max_tokens = (prompt < whole && reserve > prompt).then(|| reserve - prompt);
                Reservation {
                    tokens: reserve,
                    max_tokens,
                }
            }
            _ => Reservation {
                tokens: whole,
                max_tokens: None,
            },
        }
    }

    /// Wraps a granted slot (and tokens) in a lease on the slot `holder` should get.
    fn lease(
        self: &Arc<Self>,
        granted: Granted,
        holder: &str,
        priority: Priority,
    ) -> Result<LocalLease, String> {
        let Granted {
            slot: permit,
            tokens,
            max_tokens,
        } = granted;
        let index = lock(&self.table)
            .take(holder)
            .ok_or("a slot permit was granted with no free slot")?;
        let interactive = priority == Priority::Interactive;
        if interactive {
            self.held_interactive.fetch_add(1, Ordering::SeqCst);
            *lock(&self.last_interactive) = Some(Instant::now());
        }
        Ok(LocalLease::new(
            self.base_url.clone(),
            self.pins_slots.then_some(index),
            max_tokens.filter(|_| tokens.is_some()),
            Slot {
                pool: Arc::clone(self),
                index,
                holder: holder.to_string(),
                interactive,
                _permit: permit,
                _tokens: tokens,
            },
        ))
    }

    /// Whether one of the user's own calls held a slot within `within`, or holds one now.
    fn used_interactively(&self, within: Duration) -> bool {
        self.held_interactive.load(Ordering::SeqCst) > 0
            || lock(&self.last_interactive).is_some_and(|at| at.elapsed() < within)
    }
}

/// What a call reserves from a model's token pool.
struct Reservation {
    /// Tokens of the pool.
    tokens: u32,
    /// What the agent must send as `max_tokens`.
    max_tokens: Option<u32>,
}

/// What a call was granted: its slot, its tokens, and the `max_tokens` they allow.
struct Granted {
    slot: OwnedSemaphorePermit,
    tokens: Option<OwnedSemaphorePermit>,
    max_tokens: Option<u32>,
}

/// Counts a call as queued while it waits for tokens and then for a slot (once, not twice), also
/// when the wait is cancelled. A model with a call queued is not swapped out.
struct Queued {
    pool: Arc<SlotPool>,
    /// Tokens already taken, when the wait is for a slot.
    tokens: Option<OwnedSemaphorePermit>,
    /// Tokens to wait for when none are taken yet.
    want: u32,
    max_tokens: Option<u32>,
}

impl Queued {
    /// Only the user's own calls wait in the queue; background calls wait for the queue to empty.
    fn new(
        pool: Arc<SlotPool>,
        tokens: Option<OwnedSemaphorePermit>,
        reservation: &Reservation,
    ) -> Self {
        pool.waiting.fetch_add(1, Ordering::SeqCst);
        pool.waiting_interactive.fetch_add(1, Ordering::SeqCst);
        Queued {
            pool,
            tokens,
            want: reservation.tokens,
            max_tokens: reservation.max_tokens,
        }
    }
}

impl Drop for Queued {
    fn drop(&mut self) {
        self.pool.waiting.fetch_sub(1, Ordering::SeqCst);
        self.pool.waiting_interactive.fetch_sub(1, Ordering::SeqCst);
        self.pool.changed.notify_waiters();
    }
}

struct Loaded {
    /// Held so the server keeps running; dropping the last reference stops it.
    server: Arc<dyn Server>,
    /// The runtime serving it (`llamacpp`, `vllm`, ...).
    runtime: String,
    pool: Arc<SlotPool>,
    /// Memory it holds, its prompt cache included.
    placement: Placement,
    /// The part of `placement.ram_bytes` that is the prompt cache.
    cache_bytes: u64,
    /// Pinned to the CPU lane: it holds RAM only.
    cpu_pinned: bool,
    /// Saved slots fit this server only when saved under the same key; `None` when its slots
    /// are not saved.
    key: Option<u64>,
    loaded_at: Instant,
    /// Agents that took a slot on it since it was loaded: the batch it is serving.
    batch: HashSet<String>,
    /// What it was started with.
    launch: ModelLaunch,
    /// Its settings changed since: it restarts once nothing is using it.
    stale: bool,
    /// A call needs a larger KV pool: no call takes a slot on it, and it restarts with the
    /// larger pool once the calls running on it are done, even with agents mid-turn on it.
    growing: bool,
    /// Started with a smaller pool than `launch` asks for, to keep every layer on the GPU beside
    /// the models loaded then.
    squeezed: bool,
    /// It runs (partly) on the CPU while the GPU went idle: no call takes a slot on it, and it
    /// restarts once its calls are done, to be placed on the GPU (swap reason `promote`).
    promoting: bool,
    /// A call found its server gone ([`Server::exited`]): no call takes a slot on it until the
    /// residency controller unloads it ([`Reason::Crashed`]).
    crashed: bool,
    /// The draft model it runs with, if any.
    draft: Option<String>,
    /// Why a configured draft is not in use.
    draft_notice: Option<String>,
}

impl Loaded {
    fn in_use(&self) -> u32 {
        self.pool.in_use()
    }

    /// Whether its server has exited on its own.
    fn has_crashed(&self) -> bool {
        self.crashed || self.server.exited().is_some()
    }

    /// The pool a call of `request` needs this server restarted with, when it does not fit the
    /// current one and the pool may grow.
    ///
    /// One call that does not fit grows the pool towards the context ceiling; a call that fits
    /// but finds the pool filled by the others (past the high-water mark: it would have to
    /// wait) grows it towards what fits in memory, so the pool follows how many agents work at
    /// once. A pool squeezed beside other models does not grow until it is next loaded.
    fn growth(&self, request: &LocalRequest) -> Option<u32> {
        if self.pool.tokens.is_none() || self.launch.pool_fixed || self.squeezed {
            return None;
        }
        let need = context::demand(
            request.prompt_tokens,
            self.pool.max_output_tokens.load(Ordering::SeqCst),
        )?;
        let pool = self.pool.pool_tokens;
        if need > pool {
            return context::grow_target(pool, need, self.pool.context);
        }
        let filled = self.pool.reserved_tokens().saturating_add(need);
        context::grow_target(pool, filled, self.launch.pool_max)
    }

    /// A slot is in use or a call is queued for one.
    fn busy(&self) -> bool {
        self.in_use() > 0 || self.pool.waiting.load(Ordering::SeqCst) > 0
    }

    /// Applies the settings that only shape reservations, which need no restart.
    fn retune(&mut self, now: &ModelLaunch) {
        self.launch.max_output_tokens = now.max_output_tokens;
        self.pool
            .max_output_tokens
            .store(now.max_output_tokens, Ordering::SeqCst);
    }

    fn restore_handle(&self) -> Restore {
        Restore {
            server: Arc::clone(&self.server),
            key: self.key,
        }
    }
}

/// What restoring a holder's saved slot into a new lease needs.
struct Restore {
    server: Arc<dyn Server>,
    key: Option<u64>,
}

/// A call for a model, in arrival order.
#[derive(Debug)]
struct Call {
    seq: u64,
    model: String,
    priority: Priority,
    /// It reached the model's slots (it holds one or is queued for one, or it is a background
    /// call waiting its turn for one).
    admitted: bool,
    /// A background call waiting, in order, for a slot on its loaded model.
    slot_wait: bool,
}

#[derive(Default)]
struct State {
    loaded: HashMap<String, Loaded>,
    calls: Vec<Call>,
    next_call: u64,
    /// When models were swapped out to make room, within the last [`SWAP_WINDOW`] (Tokio's
    /// clock, which tests can advance).
    swaps: VecDeque<tokio::time::Instant>,
    /// Seconds each model last took to load, to estimate a swap.
    load_seconds: HashMap<String, f64>,
    /// How fast each model has generated, kept after it is unloaded.
    throughput: HashMap<String, Throughput>,
    /// Slots being saved for a holder while it waits (`(model, holder)`): its next call on
    /// the model waits for the save, so it restores the newest context, never an older one.
    saving: HashSet<(String, String)>,
    /// Models whose server crashed, and how often in a row, for the reload backoff.
    crashes: HashMap<String, Crash>,
}

/// A model's recent crashes.
#[derive(Debug, Clone, Copy)]
struct Crash {
    /// When it last crashed (Tokio's clock, which tests can advance).
    at: tokio::time::Instant,
    /// Crashes in a row, each within [`CRASH_STREAK`] of the one before.
    streak: u32,
}

/// Tokens generated over the seconds spent generating them, over a model's recent calls.
#[derive(Debug, Clone, Copy, Default)]
struct Throughput {
    tokens: f64,
    seconds: f64,
    /// Calls over the same recent window as `tokens` and `seconds`.
    calls: f64,
}

/// What a call is assumed to take, in seconds, before any has been measured, for the queue
/// wait routing expects.
const DEFAULT_CALL_SECONDS: f64 = 30.0;

impl Throughput {
    fn add(&mut self, tokens: f64, seconds: f64) {
        self.tokens = self.tokens * THROUGHPUT_KEEP + tokens;
        self.seconds = self.seconds * THROUGHPUT_KEEP + seconds;
        self.calls = self.calls * THROUGHPUT_KEEP + 1.0;
    }

    /// Seconds a recent call spent generating, on average.
    fn seconds_per_call(&self) -> Option<f64> {
        (self.calls > 0.0 && self.seconds > 0.0).then(|| self.seconds / self.calls)
    }

    fn per_second(&self) -> Option<f64> {
        (self.seconds > 0.0 && self.tokens > 0.0).then(|| self.tokens / self.seconds)
    }
}

impl State {
    /// Whether a call for another model that has not reached its slots arrived before `seq`.
    /// The user's calls do not wait behind background ones.
    fn behind_another_model(&self, seq: u64, model: &str, priority: Priority) -> bool {
        self.calls.iter().any(|call| {
            call.seq < seq
                && !call.admitted
                && call.model != model
                && (priority == Priority::Background || call.priority == Priority::Interactive)
        })
    }

    /// Whether a call of the user's own is waiting to load a model.
    fn interactive_load_pending(&self) -> bool {
        self.calls
            .iter()
            .any(|call| call.priority == Priority::Interactive && !call.admitted)
    }

    fn set_admitted(&mut self, seq: u64, admitted: bool) {
        if let Some(call) = self.calls.iter_mut().find(|call| call.seq == seq) {
            call.admitted = admitted;
            if !admitted {
                call.slot_wait = false;
            }
        }
    }

    /// Marks a background call as waiting in line for a slot on its loaded model.
    fn set_slot_wait(&mut self, seq: u64) {
        if let Some(call) = self.calls.iter_mut().find(|call| call.seq == seq) {
            call.admitted = true;
            call.slot_wait = true;
        }
    }

    /// Whether a background call for `model` that arrived before `seq` waits for a slot.
    fn earlier_slot_waiter(&self, seq: u64, model: &str) -> bool {
        self.calls.iter().any(|call| {
            call.seq < seq
                && call.slot_wait
                && call.model == model
                && call.priority == Priority::Background
        })
    }

    /// Whether another swap fits within `max_per_min`.
    fn swaps(&mut self, max_per_min: u32) -> Swaps {
        while self
            .swaps
            .front()
            .is_some_and(|at| at.elapsed() >= SWAP_WINDOW)
        {
            self.swaps.pop_front();
        }
        if max_per_min == 0 {
            Swaps::Off
        } else if self.swaps.len() < max_per_min as usize {
            Swaps::Allowed
        } else {
            Swaps::Spent
        }
    }
}

/// A call's place in line; it leaves the line when the call gets its slot, fails, or is
/// cancelled, which wakes the calls behind it.
struct Ticket<'a> {
    supervisor: &'a Supervisor,
    seq: u64,
    since: Instant,
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        lock(&self.supervisor.state)
            .calls
            .retain(|call| call.seq != self.seq);
        self.supervisor.changed.notify_waiters();
    }
}

/// What agents have said about themselves: who is mid-turn on which model, and who is waiting
/// for subagents.
#[derive(Debug, Default)]
struct Presence {
    users: Mutex<HashMap<String, User>>,
}

#[derive(Debug, Default)]
struct User {
    model: String,
    /// Turns in progress (one, normally).
    working: u32,
    /// `await_tasks` calls in progress.
    waiting: u32,
}

#[derive(Debug, Clone, Copy)]
enum Mark {
    Working,
    Waiting,
}

impl Presence {
    fn enter(
        self: &Arc<Self>,
        changed: &Arc<Notify>,
        model: &str,
        holder: &str,
        mark: Mark,
    ) -> LocalPresence {
        {
            let mut users = lock(&self.users);
            let user = users.entry(holder.to_string()).or_default();
            user.model = model.to_string();
            match mark {
                Mark::Working => user.working += 1,
                Mark::Waiting => user.waiting += 1,
            }
        }
        changed.notify_waiters();
        LocalPresence::new(Present {
            presence: Arc::clone(self),
            changed: Arc::clone(changed),
            holder: holder.to_string(),
            mark,
        })
    }

    /// Whether an agent of `batch` is mid-turn on `model` (working on it, not waiting), and
    /// whether any of them belongs to a session other than `requester`'s.
    fn claims(
        users: &HashMap<String, User>,
        model: &str,
        batch: &HashSet<String>,
        requester: Option<&str>,
    ) -> Claim {
        let mut claim = Claim::None;
        for holder in batch {
            let mid_turn = users
                .get(holder)
                .is_some_and(|user| user.model == model && user.working > 0 && user.waiting == 0);
            if !mid_turn {
                continue;
            }
            if requester == Some(holder_session(holder)) {
                claim = Claim::Own;
            } else {
                return Claim::Others;
            }
        }
        claim
    }
}

/// Who is mid-turn on a loaded model, from one session's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    /// Nobody.
    None,
    /// Only agents of the requester's own session.
    Own,
    /// An agent of another session (also when the requester is unknown).
    Others,
}

/// One mark of [`Presence`]; dropping it withdraws the mark.
struct Present {
    presence: Arc<Presence>,
    changed: Arc<Notify>,
    holder: String,
    mark: Mark,
}

impl Drop for Present {
    fn drop(&mut self) {
        {
            let mut users = lock(&self.presence.users);
            if let Some(user) = users.get_mut(&self.holder) {
                match self.mark {
                    Mark::Working => user.working = user.working.saturating_sub(1),
                    Mark::Waiting => user.waiting = user.waiting.saturating_sub(1),
                }
                if user.working == 0 && user.waiting == 0 {
                    users.remove(&self.holder);
                }
            }
        }
        self.changed.notify_waiters();
    }
}

/// What a call for a model does next.
enum Step {
    /// It has a slot.
    Leased(LocalLease, Restore),
    /// It waits for a slot on the loaded model.
    Queued(Queued),
    /// It may load the model.
    Load,
    /// It waits its turn.
    Wait,
    /// The holder's own slot on the model is being saved (it yielded its cells while it
    /// waited): the call waits for the save, then restores what it wrote.
    Saving,
    /// A background call gives way to the user's calls, or to a saturated machine; waiting
    /// this way does not count against `max_wait_s`.
    Yield,
    /// The model must restart (its pool grows, or it moves to the GPU) and nothing runs on it
    /// any more: this call restarts it, then asks again.
    Restart,
}

/// What the call that may load a model found once loads were its own.
enum Plan {
    /// The model was loaded meanwhile, or the call is no longer first in line.
    Retry,
    /// Wait for room; `yielded` when a background call waits on the user's calls (it does not
    /// count against `max_wait_s`).
    Wait {
        yielded: bool,
    },
    /// The model's server crashed lately and keeps crashing: it stays down a while
    /// ([`ResidencyPolicy::crash_backoff`]).
    Backoff,
    Load(Box<Load>),
}

struct Load {
    backend: Arc<dyn RuntimeBackend>,
    placement: Placement,
    /// What the prompt cache is sized from.
    cache_room: Budget,
    /// Models to swap out first, already out of the loaded set.
    victims: Vec<(String, Loaded)>,
    /// The call that loads it, admitted once the model is in the loaded set.
    call: u64,
    priority: Priority,
    /// What it is started with.
    launch: ModelLaunch,
    /// The KV pool it is started with: `launch.pool_tokens`, or less to keep its layers on the
    /// GPU beside the models loaded now.
    served_pool: u32,
    /// It is pinned to the CPU lane.
    cpu_pinned: bool,
    /// The draft model it is started with, and why a configured one is not used.
    drafting: Drafting,
    /// Why the victims give way.
    why: Reason,
}

/// Supervises local model servers. Cheap to share: hand out `Arc<Supervisor>`.
pub struct Supervisor {
    /// Replaced when the folders are scanned again, such as after a download.
    models: Mutex<Vec<LocalModel>>,
    settings: LlamaCppSettings,
    /// Per-model settings, replaced when the app saves them.
    model_settings: Mutex<BTreeMap<String, LocalModelSettings>>,
    /// In order of preference; a model is served by the first available one accepting it.
    backends: Vec<Arc<dyn RuntimeBackend>>,
    launcher: Arc<dyn Launcher>,
    /// Memory free when the supervisor started, less the RAM headroom.
    start_budget: Budget,
    /// The budget loads are planned against now; the residency controller refreshes it.
    budget_now: Mutex<Budget>,
    /// Decides how to make room for a call ([`Supervisor::set_policy`]).
    policy: Mutex<Arc<dyn ResidencyPolicy>>,
    state: Mutex<State>,
    presence: Arc<Presence>,
    /// Raised whenever a slot, a call, an agent's presence, or the loaded models change.
    changed: Arc<Notify>,
    /// Loads and unloads happen one at a time: they contend for the disk and the PCIe bus, and
    /// two agents asking for the same model load it once. A swap-out holds it until its
    /// servers have exited, even when the call that started it was cancelled.
    swapping: Arc<tokio::sync::Mutex<()>>,
    /// Measured corrections to the planner's memory estimates ([`Supervisor::set_calibration`]).
    calibration: Mutex<Option<Arc<Calibration>>>,
    /// Saved slots; `None` when slots are not saved.
    slot_files: Option<Mutex<SlotFiles>>,
    /// Models pinned to the CPU lane, with their slots.
    cpu_pinned: Mutex<HashMap<String, CpuLane>>,
    /// The KV pool each model's calls have asked for beyond the one it starts with; forgotten
    /// when the model is unloaded, so it starts small again.
    grown: Mutex<HashMap<String, u32>>,
    /// Per model, the draft model its server failed to start with and why: it loads without
    /// that draft until its settings or the installed models change.
    draft_failed: Mutex<HashMap<String, (String, String)>>,
    /// Why each waiting holder's call waits, while it does.
    waits: Mutex<HashMap<String, LocalWait>>,
    events: broadcast::Sender<SwapEvent>,
    /// Every move, with its reason ([`Supervisor::moves`]).
    moves: broadcast::Sender<Move>,
    /// How background calls give way to the user's.
    background: Mutex<BackgroundPolicy>,
    /// Itself, for swap-outs that run in a task of their own.
    me: Weak<Supervisor>,
}

/// How calls of [`Priority::Background`] give way (see [`Supervisor::set_background_policy`]).
#[derive(Clone)]
struct BackgroundPolicy {
    /// The daemon's load view; background calls wait while it reports `Saturated`.
    view: Option<Arc<dyn LoadView>>,
    /// How long after the user's own call last used a model background work may not swap it out.
    hold: Duration,
}

/// What the supervisor's slots are doing, by whose calls they are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    /// Slots held by the user's own calls.
    pub interactive_in_use: u32,
    /// The user's own calls waiting for a slot, for tokens of the KV pool, or for room to load.
    pub interactive_queued: u32,
    /// Slots held by background calls.
    pub background_in_use: u32,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("models", &lock(&self.models).len())
            .field("budget", &self.budget())
            .field("backends", &self.backends)
            .field("launcher", &self.launcher)
            .finish_non_exhaustive()
    }
}

/// Takes the memory planned for a draft that is not in use out of `placement`.
fn release_draft(placement: &mut Placement, draft: planner::DraftMemory) {
    let in_ram = draft.ram_bytes.min(draft.bytes);
    placement.vram_bytes = placement.vram_bytes.saturating_sub(draft.bytes - in_ram);
    placement.ram_bytes = placement.ram_bytes.saturating_sub(in_ram);
    placement.required_bytes = placement.required_bytes.saturating_sub(draft.bytes);
}

/// What `model` needs started with `launch`, with `draft`'s memory added when it drafts.
fn drafted_footprint(
    model: &LocalModel,
    launch: &ModelLaunch,
    draft: Option<&Draft>,
    on_cpu: bool,
    factor_permille: u32,
) -> Footprint {
    let footprint = Footprint::of(model, launch).calibrated(factor_permille);
    match draft {
        Some(draft) => footprint.with_draft(draft.memory(launch.pool_tokens, on_cpu)),
        None => footprint,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The state stays consistent across a panic: every update is a single insert or remove.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Supervisor {
    /// Creates a supervisor with the built-in runtimes ([`backend::builtin`]). It starts no task
    /// of its own: idle unloads and settings restarts are the residency controller's to order
    /// ([`Supervisor::execute`]).
    pub fn start(
        models: Vec<LocalModel>,
        settings: LlamaCppSettings,
        runtimes: Runtimes,
        hardware: &Hardware,
        launcher: Arc<dyn Launcher>,
    ) -> Arc<Self> {
        let backends = backend::builtin(&settings, runtimes);
        Self::with_backends(models, settings, backends, hardware, launcher)
    }

    /// Creates a supervisor serving models with `backends`, in order of preference. Slots are
    /// saved to the launcher's [`Launcher::slot_dir`] unless `swap_cache_mb` is 0.
    pub fn with_backends(
        models: Vec<LocalModel>,
        settings: LlamaCppSettings,
        backends: Vec<Arc<dyn RuntimeBackend>>,
        hardware: &Hardware,
        launcher: Arc<dyn Launcher>,
    ) -> Arc<Self> {
        let budget = Budget::of(hardware, settings.ram_headroom_mb * MIB);
        let slot_files = launcher
            .slot_dir()
            .filter(|_| settings.swap_cache_mb > 0)
            .and_then(
                |dir| match SlotFiles::open(dir.clone(), settings.swap_cache_mb * MIB) {
                    Ok(files) => Some(Mutex::new(files)),
                    Err(error) => {
                        tracing::warn!(dir = %dir.display(), %error, "could not create the slot folder; slots of models swapped out will not be saved");
                        None
                    }
                },
            );
        Arc::new_cyclic(|me| Supervisor {
            models: Mutex::new(models),
            model_settings: Mutex::new(settings.models.clone()),
            settings,
            backends,
            launcher,
            start_budget: budget,
            budget_now: Mutex::new(budget),
            policy: Mutex::new(Arc::new(Standard)),
            state: Mutex::new(State::default()),
            presence: Arc::default(),
            changed: Arc::new(Notify::new()),
            swapping: Arc::default(),
            calibration: Mutex::new(None),
            slot_files,
            cpu_pinned: Mutex::default(),
            grown: Mutex::default(),
            draft_failed: Mutex::default(),
            waits: Mutex::default(),
            events: broadcast::channel(64).0,
            moves: broadcast::channel(256).0,
            background: Mutex::new(BackgroundPolicy {
                view: None,
                hold: DEFAULT_INTERACTIVE_HOLD,
            }),
            me: Weak::clone(me),
        })
    }

    pub fn models(&self) -> Vec<LocalModel> {
        lock(&self.models).clone()
    }

    /// Replaces the models after a new scan. Loaded models keep running; one that is gone
    /// from disk can no longer be loaded again.
    pub fn set_models(&self, models: Vec<LocalModel>) {
        *lock(&self.models) = models;
        lock(&self.draft_failed).clear();
    }

    /// The memory loads are planned against now: measured when the supervisor started, and
    /// replaced by the residency controller's live readings ([`Action::SetBudget`]).
    pub fn budget(&self) -> Budget {
        *lock(&self.budget_now)
    }

    /// The memory measured when the supervisor started. Context ceilings are sized against it,
    /// so they do not move with the live budget (a pool that does not fit is cut instead).
    pub fn start_budget(&self) -> Budget {
        self.start_budget
    }

    pub fn settings(&self) -> &LlamaCppSettings {
        &self.settings
    }

    /// Models swapped out and in, as they happen.
    pub fn swap_events(&self) -> broadcast::Receiver<SwapEvent> {
        self.events.subscribe()
    }

    /// Everything done to memory (loads, unloads, slots saved and restored, pools resized, the
    /// budget), each with its reason, as it happens.
    pub fn moves(&self) -> broadcast::Receiver<Move> {
        self.moves.subscribe()
    }

    /// Raised whenever a slot, a call, an agent's presence, or the loaded models change, so a
    /// controller can look again.
    pub fn changes(&self) -> Arc<Notify> {
        Arc::clone(&self.changed)
    }

    /// Decides how calls make room from now on ([`Standard`] until this is called).
    pub fn set_policy(&self, policy: Arc<dyn ResidencyPolicy>) {
        *lock(&self.policy) = policy;
        self.changed.notify_waiters();
    }

    /// Plans loads against `budget` from now on; returns whether it changed. Models already
    /// loaded keep their placement.
    pub fn set_budget(&self, budget: Budget, reason: Reason) -> bool {
        {
            let mut now = lock(&self.budget_now);
            if *now == budget {
                return false;
            }
            tracing::info!(
                vram_mib = budget.vram_bytes / MIB,
                ram_mib = budget.ram_bytes / MIB,
                was_vram_mib = now.vram_bytes / MIB,
                was_ram_mib = now.ram_bytes / MIB,
                reason = reason.name(),
                "the local memory budget changed"
            );
            *now = budget;
        }
        self.record(Move::done(
            Action::SetBudget {
                vram_bytes: budget.vram_bytes,
                ram_bytes: budget.ram_bytes,
            },
            reason,
        ));
        self.changed.notify_waiters();
        true
    }

    /// Carries out what the residency controller decided. Nothing in use is ever moved:
    ///
    /// - [`Action::Unload`] unloads a model nobody uses (no slot in use, no call queued, no
    ///   agent mid-turn on it), saving its slots; [`Reason::Config`] restarts a model whose
    ///   settings changed the same way. [`Reason::Crashed`] takes a model whose server exited out
    ///   of the loaded set whoever holds it (there is nothing left to save or wait for), and
    ///   refuses one whose server still runs.
    /// - [`Action::SaveSlot`] saves an agent's slot and frees its cells (it must not be in use).
    /// - [`Action::RestoreSlot`] and [`Action::Load`] bring an agent's context back ahead of its
    ///   next call, loading its model if needed, with a call that reserves almost nothing.
    /// - [`Action::ResizePool`] makes a model restart with a pool of at least `tokens` once no
    ///   call runs on it.
    /// - [`Action::SetBudget`] replaces the memory budget.
    pub async fn execute(&self, action: Action, reason: Reason) -> Result<(), String> {
        match action {
            Action::Unload { model } if reason == Reason::Config => {
                let swapping = Arc::clone(&self.swapping).lock_owned().await;
                self.restart_where(swapping, |(id, entry)| {
                    *id == model && !entry.growing && !entry.promoting
                })
                .await;
                Ok(())
            }
            Action::Unload { model } if reason == Reason::Crashed => {
                self.unload_crashed(&model).await
            }
            Action::Unload { model } => self
                .unload_for(&model, reason)
                .await
                .map_err(|error| error.to_string()),
            Action::SaveSlot { model, holder } => {
                if self.suspend_for(&model, &holder, reason).await {
                    Ok(())
                } else {
                    Err(format!("{holder} holds no idle saved-able slot on {model}"))
                }
            }

            Action::RestoreSlot { model, holder } => self.bring_back(&model, &holder, reason).await,
            Action::Load { model } => self.bring_back(&model, "residency", reason).await,
            Action::ResizePool { model, tokens } => {
                let busy = {
                    let mut state = lock(&self.state);
                    let Some(entry) = state.loaded.get_mut(&model) else {
                        return Err(format!("{model} is not loaded"));
                    };
                    if entry.pool.tokens.is_none() || entry.launch.pool_fixed {
                        return Err(format!("the KV pool of {model} cannot grow"));
                    }
                    entry.stale = true;
                    entry.growing = true;
                    entry.busy()
                };
                self.grow_to(&model, tokens);
                self.changed.notify_waiters();
                if !busy {
                    let swapping = Arc::clone(&self.swapping).lock_owned().await;
                    self.restart_flagged(&model, swapping).await;
                }
                Ok(())
            }
            Action::SetBudget {
                vram_bytes,
                ram_bytes,
            } => {
                let unified = self.budget().unified;
                self.set_budget(
                    Budget {
                        vram_bytes,
                        ram_bytes,
                        unified,
                    },
                    reason,
                );
                Ok(())
            }
        }
    }

    /// Takes a slot on `model` for `holder` with a call that reserves almost nothing, so its
    /// saved context (if any) is restored and the model loaded if needed, then lets it go: the
    /// slot stays `holder`'s for its next call.
    async fn bring_back(&self, model: &str, holder: &str, reason: Reason) -> Result<(), String> {
        let request = LocalRequest {
            priority: Priority::Interactive,
            prompt_tokens: Some(0),
            uncapped: false,
        };
        self.acquire_slot(model, holder, request, reason, true)
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Sets how background calls give way to the user's: they wait while `view` reports
    /// `Saturated`, and may not swap out a model the user's own calls used within
    /// `interactive_hold`.
    pub fn set_background_policy(&self, view: Arc<dyn LoadView>, interactive_hold: Duration) {
        *lock(&self.background) = BackgroundPolicy {
            view: Some(view),
            hold: interactive_hold,
        };
        self.changed.notify_waiters();
    }

    /// Slots in use and calls queued, by whose they are.
    pub fn activity(&self) -> Activity {
        let state = lock(&self.state);
        let mut activity = Activity::default();
        for loaded in state.loaded.values() {
            let interactive = loaded.pool.held_interactive.load(Ordering::SeqCst);
            activity.interactive_in_use += interactive;
            activity.background_in_use += loaded.in_use().saturating_sub(interactive);
            activity.interactive_queued += loaded.pool.waiting_interactive.load(Ordering::SeqCst);
        }
        // The user's calls waiting for room to load their model count as queued too.
        let loading = state
            .calls
            .iter()
            .filter(|call| call.priority == Priority::Interactive && !call.admitted)
            .count();
        activity.interactive_queued += u32::try_from(loading).unwrap_or(u32::MAX);
        activity
    }

    /// Models swapped out to make room in the last minute.
    pub fn recent_swaps(&self) -> u32 {
        let mut state = lock(&self.state);
        // Counts over the window `max_swaps_per_min` counts over.
        state.swaps(u32::MAX);
        u32::try_from(state.swaps.len()).unwrap_or(u32::MAX)
    }

    /// Whether the machine is saturated, so background calls wait.
    fn saturated(&self) -> bool {
        lock(&self.background)
            .view
            .as_ref()
            .is_some_and(|view| view.level() == MachineLoad::Saturated)
    }

    /// Pins the models named in `pins` (model id to how it is served) to the CPU lane,
    /// replacing the earlier pins. Models already loaded keep the placement they were started
    /// with until they restart; one whose settings changed restarts once nothing is using it.
    pub fn set_cpu_pinned(&self, pins: HashMap<String, CpuLane>) {
        *lock(&self.cpu_pinned) = pins;
        self.mark_stale();
    }

    /// Replaces the per-model settings. Loaded models whose launch shape changed are marked
    /// stale; the residency controller restarts them once nothing is using them (see
    /// `unload_stale_now`). This never waits.
    pub fn set_model_settings(&self, settings: BTreeMap<String, LocalModelSettings>) {
        *lock(&self.model_settings) = settings;
        lock(&self.draft_failed).clear();
        self.mark_stale();
    }

    /// Marks the loaded models whose launch shape differs from the settings now. The residency
    /// controller restarts them once nothing is using them ([`Reason::Config`]).
    fn mark_stale(&self) {
        {
            let mut state = lock(&self.state);
            let ids: Vec<String> = state.loaded.keys().cloned().collect();
            for id in ids {
                let now = self.launch(&id);
                if let Some(entry) = state.loaded.get_mut(&id) {
                    entry.stale = entry.launch.restarts_for(&now);
                    entry.retune(&now);
                }
            }
        }
        self.changed.notify_waiters();
    }

    /// How `id` is served if it is pinned to the CPU lane.
    pub fn cpu_lane(&self, id: &str) -> Option<CpuLane> {
        lock(&self.cpu_pinned).get(id).copied().map(|lane| CpuLane {
            slots: lane.slots.max(1),
            ..lane
        })
    }

    /// The slots of `id` if it is pinned to the CPU lane.
    pub fn cpu_slots(&self, id: &str) -> Option<u32> {
        self.cpu_lane(id).map(|lane| lane.slots)
    }

    /// The settings `id` is started with: its own, then the CPU lane's, then the globals, sized
    /// for the model ([`Supervisor::sized`]).
    pub fn launch(&self, id: &str) -> ModelLaunch {
        let lane = self.cpu_lane(id).map(CpuLane::row);
        let configured = self
            .settings
            .launch_with(lock(&self.model_settings).get(id), lane);
        match self.model(id) {
            Some(model) => self.sized(&model, &configured),
            None => configured,
        }
    }

    /// `launch` sized for `model` (see [`crate::context`]): the context becomes the most one
    /// agent may use (what the model was trained for, the configured limit, and the largest
    /// pool that fits on an empty machine without pushing layers off the GPU), and a pool no
    /// setting fixes becomes the one the model's calls have asked for so far.
    pub fn sized(&self, model: &LocalModel, launch: &ModelLaunch) -> ModelLaunch {
        let Some(backend) = self.backend(model.format) else {
            return launch.clone();
        };
        let trained = model.info.context_length;
        let fit = self.fit_tokens(model, launch, backend.as_ref(), trained);
        let grown = lock(&self.grown).get(&model.id).copied().unwrap_or(0);
        context::size(launch, trained, fit, grown, backend.slot_aware())
    }

    /// The largest KV pool `model` started with `launch` fits with on an empty machine: while
    /// every layer fits on the GPU, only pools that keep them all there.
    fn fit_tokens(
        &self,
        model: &LocalModel,
        launch: &ModelLaunch,
        backend: &dyn RuntimeBackend,
        trained: Option<u64>,
    ) -> u32 {
        let cpu = self.cpu_slots(&model.id).is_some();
        let factor = self.memory_factor(&model.id, cpu);
        // A draft that fits beside the model at the least working pool counts in what the
        // pool must leave room for.
        let drafting = self.drafting(model, launch, context::MIN_FIT_TOKENS);
        let place = |tokens: u32| {
            let footprint = drafted_footprint(
                model,
                &ModelLaunch {
                    pool_tokens: tokens,
                    ..launch.clone()
                },
                drafting.draft(),
                cpu,
                factor,
            );
            if cpu {
                backend.place_on_cpu(footprint, self.start_budget)
            } else {
                backend.place(footprint, self.start_budget)
            }
        };
        let Some(base) = place(context::MIN_FIT_TOKENS) else {
            return context::MIN_FIT_TOKENS;
        };
        let whole = base.all_layers_on_gpu;
        context::largest_fitting(
            context::MIN_FIT_TOKENS,
            context::search_limit(launch, trained),
            |tokens| place(tokens).is_some_and(|p| !whole || p.all_layers_on_gpu),
        )
    }

    /// Before `id` is loaded for a call of `request`: has it start with a pool the call fits in,
    /// up to its ceiling, so the call does not restart it at once.
    fn note_demand(&self, id: &str, request: &LocalRequest) {
        let launch = self.launch(id);
        let slot_aware = self
            .model(id)
            .and_then(|model| self.backend(model.format).map(|b| b.slot_aware()))
            .unwrap_or(false);
        if launch.pool_fixed || !slot_aware {
            return;
        }
        let Some(need) = context::demand(request.prompt_tokens, launch.max_output_tokens) else {
            return;
        };
        if let Some(target) = context::grow_target(launch.pool_tokens, need, launch.context) {
            self.grow_to(id, target);
        }
    }

    fn grow_to(&self, id: &str, target: u32) {
        let mut grown = lock(&self.grown);
        let pool = grown.entry(id.to_string()).or_default();
        *pool = (*pool).max(target);
    }

    /// Forgets what `id`'s calls asked of its pool: it starts small again when next loaded.
    fn shrink(&self, id: &str) {
        lock(&self.grown).remove(id);
    }

    /// What loading `model` needs with the settings it is started with.
    pub fn footprint(&self, model: &LocalModel) -> Footprint {
        let launch = self.launch(&model.id);
        let drafting = self.drafting(model, &launch, launch.pool_tokens);
        let cpu = self.cpu_slots(&model.id).is_some();
        drafted_footprint(
            model,
            &launch,
            drafting.draft(),
            cpu,
            self.memory_factor(&model.id, cpu),
        )
    }

    /// Plans memory with the corrections `calibration` holds from now on: measured peaks move
    /// the planner's runtime overhead for the models and lanes they were measured on.
    pub fn set_calibration(&self, calibration: Arc<Calibration>) {
        *lock(&self.calibration) = Some(calibration);
    }

    /// The factor (in thousandths of the static estimate) `id` is planned with.
    fn memory_factor(&self, id: &str, cpu: bool) -> u32 {
        lock(&self.calibration)
            .as_ref()
            .map_or(calibration::NEUTRAL_PERMILLE, |calibration| {
                calibration.factor_permille(id, cpu)
            })
    }

    /// Records the peak memory a loaded model's server was measured at (by a benchmark run)
    /// against what was planned for it, so later plans follow what it really takes. Nothing
    /// happens when no calibration is set, the model is not loaded, or the peak is not known for
    /// where it runs (GPU memory per process is not reported on every driver).
    pub fn record_measured_memory(
        &self,
        id: &str,
        peak_ram_bytes: Option<u64>,
        peak_vram_bytes: Option<u64>,
    ) -> Option<u32> {
        let calibration = lock(&self.calibration).clone()?;
        let loaded = self
            .loaded()
            .into_iter()
            .find(|loaded| loaded.model_id == id)?;
        let unified = self.start_budget.unified;
        let (lane, planned, measured) = if loaded.cpu_pinned || (!unified && loaded.gpu_layers == 0)
        {
            (Lane::Cpu, loaded.ram_bytes, peak_ram_bytes?)
        } else if unified {
            // One pool of memory: the process's resident memory is all there is to read.
            (Lane::Gpu, loaded.ram_bytes, peak_ram_bytes?)
        } else if loaded.all_layers_on_gpu {
            (Lane::Gpu, loaded.vram_bytes, peak_vram_bytes?)
        } else {
            (
                Lane::Partial,
                loaded.vram_bytes + loaded.ram_bytes,
                peak_vram_bytes?.saturating_add(peak_ram_bytes?),
            )
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        calibration.record(id, lane, planned, measured, now)
    }

    /// What `model`'s draft setting comes to when it loads with a KV pool of `pool_tokens`: the
    /// draft is used when it is installed, compatible, and fits on an empty machine beside the
    /// model without pushing the model's layers off the GPU; otherwise the model runs without
    /// it ([`Drafting::Skipped`]).
    pub fn drafting(&self, model: &LocalModel, launch: &ModelLaunch, pool_tokens: u32) -> Drafting {
        let resolved = {
            let models = lock(&self.models);
            speculative::resolve(model, launch, &models)
        };
        let Drafting::On(draft) = resolved else {
            return resolved;
        };
        if let Some((failed, reason)) = lock(&self.draft_failed).get(&model.id)
            && *failed == draft.model.id
        {
            return Drafting::Skipped(reason.clone());
        }
        let Some(backend) = self.backend(model.format) else {
            return Drafting::Skipped(speculative::skipped(
                &draft.model.id,
                "no runtime serves the model",
            ));
        };
        if backend.id() != backend::LLAMA_CPP {
            return Drafting::Skipped(speculative::skipped(
                &draft.model.id,
                "only llama.cpp can use a draft model",
            ));
        }
        let cpu = self.cpu_slots(&model.id).is_some();
        let sized = ModelLaunch {
            pool_tokens,
            ..launch.clone()
        };
        let place = |footprint: Footprint| {
            if cpu {
                backend.place_on_cpu(footprint, self.start_budget)
            } else {
                backend.place(footprint, self.start_budget)
            }
        };
        let factor = self.memory_factor(&model.id, cpu);
        let alone = place(Footprint::of(model, &sized).calibrated(factor));
        let with = place(drafted_footprint(
            model,
            &sized,
            Some(draft.as_ref()),
            cpu,
            factor,
        ));
        let fits = match (&alone, &with) {
            (_, None) => Err("it does not fit in memory beside the model"),
            (Some(alone), Some(with)) if alone.all_layers_on_gpu && !with.all_layers_on_gpu => {
                Err("beside it, some of the model's layers would no longer fit on the GPU")
            }
            _ => Ok(()),
        };
        match fits {
            Ok(()) => Drafting::On(draft),
            Err(reason) => Drafting::Skipped(speculative::skipped(&draft.model.id, reason)),
        }
    }

    /// The draft model `model` runs with while loaded with `launch`'s settings, or what its draft
    /// setting comes to when it loads: the draft's id and, when a configured draft is not used,
    /// the notice saying why.
    pub fn draft_state(
        &self,
        model: &LocalModel,
        launch: &ModelLaunch,
    ) -> (Option<String>, Option<String>) {
        if let Some(loaded) = lock(&self.state)
            .loaded
            .get(&model.id)
            .filter(|loaded| !loaded.launch.restarts_for(launch))
        {
            return (loaded.draft.clone(), loaded.draft_notice.clone());
        }
        let drafting = self.drafting(model, launch, launch.pool_tokens);
        (
            drafting.draft().map(|draft| draft.model.id.clone()),
            drafting.notice().map(str::to_string),
        )
    }

    /// Where `model` goes within a budget when served by `backend`: on the CPU lane if it is
    /// pinned there, else wherever the runtime places it.
    fn placer(
        &self,
        model: &LocalModel,
        backend: Arc<dyn RuntimeBackend>,
    ) -> impl Fn(Budget) -> Option<Placement> {
        let cpu = self.cpu_slots(&model.id).is_some();
        self.placer_on(model, backend, cpu)
    }

    /// Like [`Supervisor::placer`], on the CPU lane (`cpu`) or wherever the runtime places it,
    /// whatever the model's pin.
    fn placer_on(
        &self,
        model: &LocalModel,
        backend: Arc<dyn RuntimeBackend>,
        cpu: bool,
    ) -> impl Fn(Budget) -> Option<Placement> {
        let launch = self.launch(&model.id);
        let drafting = self.drafting(model, &launch, launch.pool_tokens);
        let draft = drafting.draft().cloned();
        let factor = self.memory_factor(&model.id, cpu);
        let model = model.clone();
        // The pool may give way down to a working context (or what the launch starts with, if
        // less); the weights never give way for it.
        let floor = launch
            .pool_tokens
            .min(context::MIN_WORKING_CONTEXT.min(launch.context))
            .max(1);
        let elastic = backend.slot_aware();
        move |budget| {
            let at = |tokens: u32| {
                let footprint = drafted_footprint(
                    &model,
                    &ModelLaunch {
                        pool_tokens: tokens,
                        ..launch.clone()
                    },
                    draft.as_ref(),
                    cpu,
                    factor,
                );
                if cpu {
                    backend.place_on_cpu(footprint, budget)
                } else {
                    backend.place(footprint, budget)
                }
            };
            let full = at(launch.pool_tokens);
            if !elastic || full.is_some_and(|p| cpu || p.all_layers_on_gpu) {
                return full;
            }
            // Beside the models loaded now the whole pool would push layers off the GPU (or
            // not fit): cut the pool instead, while the weights and a working pool fit.
            let Some(least) = at(floor) else {
                return full;
            };
            let keep = |p: &Placement| !least.all_layers_on_gpu || p.all_layers_on_gpu;
            if full.as_ref().is_some_and(keep) {
                return full;
            }
            let tokens = context::largest_fitting(floor, launch.pool_tokens, |tokens| {
                at(tokens).as_ref().is_some_and(keep)
            });
            at(tokens)
        }
    }

    /// What loading `model` now would get, from the placer the loader uses: its placement and
    /// KV pool beside the models loaded (other than itself), or on their memory too when it only
    /// fits once they are swapped out; `None` when it does not fit this machine.
    pub fn estimate(&self, model: &LocalModel) -> Option<(Placement, u32)> {
        let backend = Arc::clone(self.backend(model.format)?);
        let launch = self.launch(&model.id);
        let left = {
            let state = lock(&self.state);
            let (vram, ram) = state.loaded.iter().filter(|(id, _)| **id != model.id).fold(
                (0, 0),
                |(vram, ram), (_, loaded)| {
                    (
                        vram + loaded.placement.vram_bytes,
                        ram + loaded.placement.ram_bytes,
                    )
                },
            );
            Budget {
                vram_bytes: self.budget().vram_bytes.saturating_sub(vram),
                ram_bytes: self.budget().ram_bytes.saturating_sub(ram),
                unified: self.budget().unified,
            }
        };
        let place = self.placer(model, backend);
        let placement = place(left).or_else(|| place(self.budget()))?;
        let drafting = self.drafting(model, &launch, launch.pool_tokens);
        Some((
            placement,
            self.placed_pool(model, &launch, drafting.draft(), &placement),
        ))
    }

    /// Whether `model` fits in memory now: loaded, fitting beside the loaded models, fitting
    /// once loaded models nobody is using are unloaded, or not; with the placement it would get.
    /// An estimate from the planner, as the loader would place it: nothing is loaded.
    pub fn fit_now(&self, model: &LocalModel) -> (planner::Fit, Option<Placement>) {
        let Some(backend) = self.backend(model.format).map(Arc::clone) else {
            return (planner::Fit::DoesNotFit, None);
        };
        let held: Vec<planner::Held> = {
            let state = lock(&self.state);
            if let Some(loaded) = state.loaded.get(&model.id) {
                return (planner::Fit::Loaded, Some(loaded.placement));
            }
            let users = lock(&self.presence.users);
            state
                .loaded
                .iter()
                .map(|(id, loaded)| planner::Held {
                    vram_bytes: loaded.placement.vram_bytes,
                    ram_bytes: loaded.placement.ram_bytes,
                    idle: !loaded.busy()
                        && Presence::claims(&users, id, &loaded.batch, None) == Claim::None,
                })
                .collect()
        };
        planner::fit_beside(&held, self.budget(), self.placer(model, backend))
    }

    /// The KV pool `placement` (from [`Supervisor::placer`]) was made with: the launch's pool,
    /// or less when it was cut to keep the layers on the GPU.
    fn placed_pool(
        &self,
        model: &LocalModel,
        launch: &ModelLaunch,
        draft: Option<&Draft>,
        placement: &Placement,
    ) -> u32 {
        // The draft's context follows the pool: its KV cache grows with it, the rest does not.
        let per_token = model
            .info
            .kv_bytes_per_token_kv(
                planner::cache_element_bytes(&launch.cache_type_k),
                planner::cache_element_bytes(&launch.cache_type_v),
            )
            .map(|per_token| per_token + draft.map_or(0, Draft::kv_bytes_per_token));
        // The overhead as planned, calibrated or not.
        let cpu = self.cpu_slots(&model.id).is_some();
        let overhead = Footprint::of(model, launch)
            .calibrated(self.memory_factor(&model.id, cpu))
            .overhead_bytes;
        let weights = model.size_bytes + overhead + draft.map_or(0, Draft::fixed_bytes);
        match per_token {
            Some(per_token) if per_token > 0 => {
                let kv = placement.required_bytes.saturating_sub(weights);
                u32::try_from(kv / per_token)
                    .unwrap_or(u32::MAX)
                    .min(launch.pool_tokens)
            }
            _ => launch.pool_tokens,
        }
    }

    /// What `model` needs and where it would go on an empty machine when started with `launch`.
    pub fn plan_with(
        &self,
        model: &LocalModel,
        launch: &ModelLaunch,
    ) -> (Footprint, Option<Placement>) {
        let cpu = self.cpu_slots(&model.id).is_some();
        let drafting = self.drafting(model, launch, launch.pool_tokens);
        let footprint = drafted_footprint(
            model,
            launch,
            drafting.draft(),
            cpu,
            self.memory_factor(&model.id, cpu),
        );
        let placement = self.backend(model.format).and_then(|backend| {
            if cpu {
                backend.place_on_cpu(footprint, self.budget())
            } else {
                backend.place(footprint, self.budget())
            }
        });
        (footprint, placement)
    }

    /// The settings the loaded model `id` was started with.
    pub fn loaded_launch(&self, id: &str) -> Option<ModelLaunch> {
        lock(&self.state)
            .loaded
            .get(id)
            .map(|entry| entry.launch.clone())
    }

    /// Where `model` would go on an empty machine, for listings.
    pub fn placement_when_idle(&self, model: &LocalModel) -> Option<Placement> {
        let backend = Arc::clone(self.backend(model.format)?);
        self.placer(model, backend)(self.budget())
    }

    /// How fast `id` has generated in its recent calls, in tokens per second; `None` before a
    /// call finished.
    pub fn tokens_per_s(&self, id: &str) -> Option<f64> {
        lock(&self.state)
            .throughput
            .get(id)
            .and_then(Throughput::per_second)
    }

    /// The runtime that serves `format`: the first available one accepting it.
    fn backend(&self, format: Format) -> Option<&Arc<dyn RuntimeBackend>> {
        self.backends
            .iter()
            .find(|backend| backend.accepts(format) && backend.available())
    }

    pub fn loaded(&self) -> Vec<LoadedStatus> {
        let idle_unload = Duration::from_secs(self.settings.idle_unload_s);
        let state = lock(&self.state);
        let users = lock(&self.presence.users);
        let mut loaded: Vec<_> = state
            .loaded
            .iter()
            .map(|(id, loaded)| {
                let claimed = Presence::claims(&users, id, &loaded.batch, None) != Claim::None;
                let idle_for =
                    (!loaded.busy() && !claimed).then(|| lock(&loaded.pool.last_used).elapsed());
                LoadedStatus {
                    model_id: id.clone(),
                    runtime: loaded.runtime.clone(),
                    base_url: loaded.pool.base_url.clone(),
                    pid: loaded.server.pid(),
                    slots: loaded.pool.slots,
                    slots_in_use: loaded.in_use(),
                    queued: loaded.pool.waiting.load(Ordering::SeqCst),
                    gpu_layers: loaded.placement.gpu_layers,
                    all_layers_on_gpu: loaded.placement.all_layers_on_gpu,
                    vram_bytes: loaded.placement.vram_bytes,
                    ram_bytes: loaded.placement.ram_bytes,
                    idle_for,
                    unload_in: idle_for.map(|idle| idle_unload.saturating_sub(idle)),
                    claimed,
                    cpu_pinned: loaded.cpu_pinned,
                    context: loaded.pool.context,
                    pool_tokens: loaded.pool.pool_tokens,
                    pool_reserved_tokens: loaded.pool.reserved_tokens(),
                    stale: loaded.stale,
                    draft: loaded.draft.clone(),
                    draft_notice: loaded.draft_notice.clone(),
                    crashed: loaded.has_crashed(),
                }
            })
            .collect();
        loaded.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        loaded
    }

    /// Unloads `id` now, saving its slots, if nothing is using it: no slot in use, no call
    /// queued for one, and no agent mid-turn on it (between its calls). Waits its turn behind
    /// a load or unload in flight.
    pub async fn unload(&self, id: &str) -> Result<(), UnloadError> {
        self.unload_for(id, Reason::Request).await
    }

    /// [`Supervisor::unload`], reporting the move with `reason`.
    async fn unload_for(&self, id: &str, reason: Reason) -> Result<(), UnloadError> {
        let swapping = Arc::clone(&self.swapping).lock_owned().await;
        let victim = {
            let mut state = lock(&self.state);
            let users = lock(&self.presence.users);
            let entry = state
                .loaded
                .get(id)
                .ok_or_else(|| UnloadError::NotLoaded(id.to_string()))?;
            let in_use = entry.in_use();
            if in_use > 0 {
                return Err(UnloadError::InUse(id.to_string(), in_use));
            }
            if entry.busy() {
                return Err(UnloadError::Queued(id.to_string()));
            }
            if Presence::claims(&users, id, &entry.batch, None) != Claim::None {
                return Err(UnloadError::Claimed(id.to_string()));
            }
            // Planned while idle, but used again before the unload got its turn.
            let idle_unload = Duration::from_secs(self.settings.idle_unload_s);
            if reason == Reason::Idle && lock(&entry.pool.last_used).elapsed() < idle_unload {
                return Err(UnloadError::Used(id.to_string()));
            }
            state
                .loaded
                .remove_entry(id)
                .into_iter()
                .collect::<Vec<_>>()
        };
        tracing::info!(model = %id, reason = reason.name(), "unloading model");
        self.shrink(id);
        if let Err(error) = self.swap_out_detached(victim, reason, None, swapping).await {
            tracing::warn!(%error, model = %id, "could not unload a model");
        }
        self.changed.notify_waiters();
        Ok(())
    }

    /// Takes `id`, whose server has exited on its own, out of the loaded set, so the next call
    /// for it loads it again (after [`ResidencyPolicy::crash_backoff`] when it keeps crashing).
    /// Calls holding a slot on it have already failed or will; nothing can be saved from it.
    async fn unload_crashed(&self, id: &str) -> Result<(), String> {
        let swapping = Arc::clone(&self.swapping).lock_owned().await;
        let (entry, exit, streak) = {
            let mut state = lock(&self.state);
            let entry = state
                .loaded
                .get(id)
                .ok_or_else(|| format!("{id} is not loaded"))?;
            let Some(exit) = entry.server.exited() else {
                return Err(format!("the server of {id} is running"));
            };
            let entry = state
                .loaded
                .remove(id)
                .ok_or_else(|| format!("{id} is not loaded"))?;
            let now = tokio::time::Instant::now();
            let streak = match state.crashes.get(id) {
                Some(last) if now.duration_since(last.at) < CRASH_STREAK => last.streak + 1,
                _ => 1,
            };
            state
                .crashes
                .insert(id.to_string(), Crash { at: now, streak });
            (entry, exit, streak)
        };
        let started = Instant::now();
        self.shrink(id);
        // Reaps the process; its memory is already free.
        entry.server.stop().await;
        drop(entry);
        drop(swapping);
        let seconds = started.elapsed().as_secs_f64();
        tracing::warn!(model = %id, %exit, streak, "a local model's server exited on its own; it is loaded again at its next call");
        self.record(
            Move::done(
                Action::Unload {
                    model: id.to_string(),
                },
                Reason::Crashed,
            )
            .took(seconds, 0)
            .detail(exit),
        );
        self.emit(SwapEvent {
            direction: SwapDirection::Out,
            model_id: id.to_string(),
            seconds,
            bytes: 0,
            slots: 0,
            reason: Reason::Crashed.swap_label(),
        });
        self.changed.notify_waiters();
        Ok(())
    }

    fn model(&self, id: &str) -> Option<LocalModel> {
        lock(&self.models)
            .iter()
            .find(|model| model.id == id)
            .cloned()
    }

    /// The loaded models as the arbiter sees them, from `requester`'s point of view (`None`
    /// counts every claim as another session's). With `protect`, a model the user's own calls
    /// used within that long counts as in use, so it is not swapped out (and whether any model
    /// was protected is returned). The caller holds `users`, so claims stay as they were read
    /// until it has taken the victims out of the loaded set.
    fn residents(
        &self,
        state: &State,
        users: &HashMap<String, User>,
        protect: Option<Duration>,
        requester: Option<&str>,
    ) -> (Vec<Resident>, bool) {
        let mut protected = false;
        let residents = state
            .loaded
            .iter()
            .map(|(id, loaded)| {
                let guarded = !loaded.busy()
                    && protect.is_some_and(|within| loaded.pool.used_interactively(within));
                Resident {
                    id: id.clone(),
                    vram_bytes: loaded.placement.vram_bytes,
                    ram_bytes: loaded.placement.ram_bytes,
                    cache_bytes: loaded.cache_bytes,
                    cpu_pinned: loaded.cpu_pinned,
                    generating: loaded.in_use() > 0,
                    protected: guarded,
                    use_: if loaded.busy() {
                        Use::Busy
                    } else if guarded {
                        protected = true;
                        Use::Busy
                    } else {
                        match Presence::claims(users, id, &loaded.batch, requester) {
                            Claim::Others => Use::Busy,
                            Claim::Own => Use::Claimed,
                            Claim::None => Use::Free,
                        }
                    },
                    resident_for: loaded.loaded_at.elapsed(),
                    idle_for: lock(&loaded.pool.last_used).elapsed(),
                }
            })
            .collect();
        (residents, protected)
    }

    fn policy(&self, state: &mut State) -> Policy {
        Policy {
            min_residency: Duration::from_secs(self.settings.min_residency_s),
            swaps: state.swaps(self.settings.max_swaps_per_min),
            gain: self.settings.swap_gain,
        }
    }

    /// Expected seconds to swap `model` in: its last load, or an estimate from its size.
    fn swap_seconds(&self, state: &State, model: &LocalModel) -> u32 {
        let seconds = state
            .load_seconds
            .get(&model.id)
            .copied()
            .unwrap_or_else(|| model.size_bytes.div_ceil(ASSUMED_LOAD_BYTES_PER_S) as f64);
        seconds.ceil().clamp(1.0, f64::from(u32::MAX)) as u32
    }

    fn ticket(&self, id: &str, priority: Priority) -> Ticket<'_> {
        let mut state = lock(&self.state);
        let seq = state.next_call;
        state.next_call += 1;
        state.calls.push(Call {
            seq,
            model: id.to_string(),
            priority,
            admitted: false,
            slot_wait: false,
        });
        Ticket {
            supervisor: self,
            seq,
            since: Instant::now(),
        }
    }

    /// Takes a slot on `id` for `holder`'s call, loading the model first if needed; `cause` is
    /// the reason reported for the load and the restore of a saved slot ([`Reason::Call`], or
    /// [`Reason::Resume`] for an agent's context brought back ahead of its next call). Without
    /// `may_load` the call never loads, restarts or swaps a model: it takes a slot on the model
    /// as loaded, or fails with [`AcquireError::NotLoaded`].
    async fn acquire_slot(
        &self,
        id: &str,
        holder: &str,
        request: LocalRequest,
        cause: Reason,
        may_load: bool,
    ) -> Result<LocalLease, AcquireError> {
        let priority = request.priority;
        let model = self
            .model(id)
            .ok_or_else(|| format!("no local model {id:?} in models.folders"))?;
        let ticket = self.ticket(id, priority);
        let lane = self.cpu_lane(id);
        let max_wait = Duration::from_secs(self.launch(id).max_wait_s);
        let mut deadline = tokio::time::Instant::now() + max_wait;
        let setting = if lane.is_some() {
            "max_wait_s of its model-table row or settings, or runtime.llamacpp.max_wait_s"
        } else {
            "max_wait_s of its settings, or runtime.llamacpp.max_wait_s"
        };
        let too_long = || {
            format!(
                "waited {} s for a slot on local model {id} or for room to load it ({setting})",
                max_wait.as_secs()
            )
        };
        // Time spent waiting for a slot or for room, which the lease reports; loading the
        // model, swapping others out, and restoring a saved slot are not counted.
        let mut waited = Duration::ZERO;
        let waiting = Waiting {
            waits: &self.waits,
            holder,
        };
        loop {
            // Listen before looking, so a change between the two is not missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            // A background call does nothing while the machine is saturated.
            let step = if priority == Priority::Background && self.saturated() {
                Step::Yield
            } else {
                self.step(id, holder, &request, &ticket, may_load)?
            };
            if matches!(step, Step::Leased(..) | Step::Queued(_)) {
                self.yield_waiters(id, holder);
            }
            match step {
                Step::Leased(lease, restore) => {
                    let lease = lease.with_wait(waited);
                    return Ok(self.restore(id, holder, lease, restore, cause).await);
                }
                Step::Queued(mut queued) => {
                    let pool = Arc::clone(&queued.pool);
                    let queued_at = Instant::now();
                    // Tokens first, then the slot, so a call never holds a slot while it waits
                    // for tokens.
                    waiting.set(if queued.tokens.is_none() && pool.tokens.is_some() {
                        LocalWait::KvRoom
                    } else {
                        LocalWait::Slot
                    });
                    let tokens = match (queued.tokens.take(), &pool.tokens) {
                        (Some(tokens), _) => Some(tokens),
                        (None, Some(semaphore)) => Some(
                            tokio::time::timeout_at(
                                deadline,
                                Arc::clone(semaphore).acquire_many_owned(queued.want),
                            )
                            .await
                            .map_err(|_| too_long())?
                            .map_err(|_| "the model was unloaded".to_string())?,
                        ),
                        (None, None) => None,
                    };
                    waiting.set(LocalWait::Slot);
                    let slot = tokio::time::timeout_at(
                        deadline,
                        Arc::clone(&pool.permits).acquire_owned(),
                    )
                    .await
                    .map_err(|_| too_long())?
                    .map_err(|_| "the model was unloaded".to_string())?;
                    let granted = Granted {
                        slot,
                        tokens,
                        max_tokens: queued.max_tokens,
                    };
                    drop(queued);
                    waited += queued_at.elapsed();
                    if let Some((lease, restore)) =
                        self.lease_if_current(id, &pool, granted, holder, priority, ticket.seq)?
                    {
                        let lease = lease.with_wait(waited);
                        return Ok(self.restore(id, holder, lease, restore, cause).await);
                    }
                    // Unloaded meanwhile: wait for it again, keeping the place in line.
                    continue;
                }
                // `step` answers these only to a call that may load.
                Step::Load | Step::Restart if !may_load => {
                    return Err(AcquireError::NotLoaded(id.to_string()));
                }
                Step::Load => {
                    self.note_demand(id, &request);
                    // Behind a load or unload in flight, perhaps of another model.
                    waiting.set(LocalWait::OtherModel);
                    let swapping = Arc::clone(&self.swapping).lock_owned();
                    let swapping = tokio::time::timeout_at(deadline, swapping)
                        .await
                        .map_err(|_| too_long())?;
                    match self.plan(&model, holder, priority, &ticket)? {
                        Plan::Retry => continue,
                        Plan::Load(load) => {
                            waiting.set(if load.victims.is_empty() {
                                LocalWait::Loading
                            } else {
                                LocalWait::SavingContext
                            });
                            return self
                                .swap_in(&model, holder, &request, *load, swapping, cause)
                                .await
                                .map(|lease| lease.with_wait(waited))
                                .map_err(AcquireError::Failed);
                        }
                        Plan::Wait { yielded } => {
                            drop(swapping);
                            waiting.set(LocalWait::MakingRoom);
                            if yielded {
                                deadline = tokio::time::Instant::now() + max_wait;
                            }
                        }
                        Plan::Backoff => {
                            drop(swapping);
                            waiting.set(LocalWait::Loading);
                        }
                    }
                }
                Step::Wait => waiting.set(self.why_waiting(id)),
                Step::Saving => waiting.set(LocalWait::SavingContext),
                Step::Restart => {
                    waiting.set(self.why_waiting(id));
                    let swapping = Arc::clone(&self.swapping).lock_owned();
                    let swapping = tokio::time::timeout_at(deadline, swapping)
                        .await
                        .map_err(|_| too_long())?;
                    self.restart_flagged(id, swapping).await;
                    continue;
                }
                // Giving way to the user is not a failure to get room: the wait starts over.
                Step::Yield => {
                    waiting.set(LocalWait::Slot);
                    deadline = tokio::time::Instant::now() + max_wait;
                }
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(too_long().into());
            }
            let asleep = Instant::now();
            let _ = tokio::time::timeout(RECHECK.min(deadline - now), changed.as_mut()).await;
            waited += asleep.elapsed();
        }
    }

    /// Whether the loaded model `id` runs (partly) on the CPU although nothing generates on any
    /// model and it would get more of its layers on the GPU once everything not generating gave
    /// way. A CPU-lane model is only moved when no other session is working either. The runtime
    /// check of the GPU rule: it corrects the placement by re-planning, never by failing.
    fn promotes(&self, state: &State, id: &str, holder: &str, priority: Priority) -> bool {
        let Some(entry) = state.loaded.get(id) else {
            return false;
        };
        let lane = self.cpu_lane(id).is_some();
        if entry.in_use() > 0
            || entry.growing
            || entry.promoting
            || (entry.placement.all_layers_on_gpu && !entry.cpu_pinned)
            || state.loaded.values().any(|other| other.in_use() > 0)
        {
            return false;
        }
        let Some(model) = self.model(id) else {
            return false;
        };
        let Some(backend) = self.backend(model.format).map(Arc::clone) else {
            return false;
        };
        let users = lock(&self.presence.users);
        let session = holder_session(holder);
        if lane && !critic_may_have_gpu(&users, session) {
            return false;
        }
        let protect = (priority == Priority::Background).then(|| lock(&self.background).hold);
        let (residents, _) = self.residents(state, &users, protect, Some(session));
        let mut others: Vec<Resident> = residents.into_iter().filter(|r| r.id != id).collect();
        if self.settings.max_swaps_per_min == 0 {
            // Nothing may be swapped out for it: only the room left as it is counts.
            for other in &mut others {
                other.generating = true;
            }
        }
        let room = arbiter::room_after_yield(self.budget(), &others);
        let now = entry.placement.gpu_layers;
        self.placer_on(&model, backend, false)(room).is_some_and(|best| best.gpu_layers > now)
    }

    /// Takes a slot and tokens on `id` if it is loaded and takes this call, queues for them, or
    /// says whether the call may load the model or must wait its turn. Without `may_load`, a
    /// model that is not loaded as it is (missing, crashed, or waiting to restart) is
    /// [`AcquireError::NotLoaded`], and the call never makes it grow or move.
    fn step(
        &self,
        id: &str,
        holder: &str,
        request: &LocalRequest,
        ticket: &Ticket<'_>,
        may_load: bool,
    ) -> Result<Step, AcquireError> {
        let priority = request.priority;
        let not_loaded = || Err(AcquireError::NotLoaded(id.to_string()));
        let mut state = lock(&self.state);
        let behind = state.behind_another_model(ticket.seq, id, priority);
        if !state.loaded.contains_key(id) {
            if !may_load {
                return not_loaded();
            }
            state.set_admitted(ticket.seq, false);
            return Ok(if behind { Step::Wait } else { Step::Load });
        }
        // Its own slot is being saved (it yielded its cells while it waited): restoring before
        // the save is done would read an older save, or none.
        if state.saving.contains(&(id.to_string(), holder.to_string())) {
            return Ok(Step::Saving);
        }
        if let Some(entry) = state.loaded.get_mut(id)
            && entry.has_crashed()
        {
            // The residency controller unloads it ([`Reason::Crashed`]); the call then loads
            // it again. Told once, so waiting calls do not wake each other.
            let first = !entry.crashed;
            entry.crashed = true;
            state.set_admitted(ticket.seq, false);
            drop(state);
            if first {
                tracing::warn!(model = %id, "a local model's server has exited; it is loaded again");
                self.changed.notify_waiters();
            }
            return if may_load {
                Ok(Step::Wait)
            } else {
                not_loaded()
            };
        }
        if !may_load
            && state
                .loaded
                .get(id)
                .is_some_and(|entry| entry.growing || entry.promoting)
        {
            return not_loaded();
        }
        // At a request boundary, a model running on the CPU while nothing generates is moved
        // to the GPU: no agent runs on the CPU alone while the GPU is idle.
        let promote = may_load && self.promotes(&state, id, holder, priority);
        let Some(entry) = state.loaded.get_mut(id) else {
            return Ok(Step::Wait);
        };
        // Growing or moving: nothing new starts on it, so the calls running drain, and the
        // first call waiting for it restarts it.
        if entry.growing || entry.promoting {
            return Ok(if entry.busy() {
                Step::Wait
            } else {
                Step::Restart
            });
        }
        if promote {
            tracing::warn!(model = %id, gpu_layers = entry.placement.gpu_layers, "a call would run on the CPU while the GPU is idle; the model is placed on the GPU again");
            entry.stale = true;
            entry.promoting = true;
            let busy = entry.busy();
            state.set_admitted(ticket.seq, false);
            drop(state);
            self.changed.notify_waiters();
            return Ok(if busy { Step::Wait } else { Step::Restart });
        }
        if may_load && let Some(target) = entry.growth(request) {
            tracing::info!(model = %id, from = entry.pool.pool_tokens, to = target, "a call needs a larger KV pool; the model restarts with it once its running calls are done");
            self.grow_to(id, target);
            entry.stale = true;
            entry.growing = true;
            let busy = entry.busy();
            state.set_admitted(ticket.seq, false);
            drop(state);
            self.changed.notify_waiters();
            return Ok(if busy { Step::Wait } else { Step::Restart });
        }
        // An agent already in the model's batch carries on; a newcomer joins only while no
        // earlier call for another model is waiting for the room, and not while the model
        // waits to restart with new settings.
        if (behind || entry.stale) && !entry.batch.contains(holder) {
            return Ok(Step::Wait);
        }
        let pool = Arc::clone(&entry.pool);
        // A background call takes a slot only when none of the user's calls is queued for one
        // and no earlier background call is waiting; otherwise it waits outside the queue.
        if priority == Priority::Background
            && (pool.waiting_interactive.load(Ordering::SeqCst) > 0
                || state.earlier_slot_waiter(ticket.seq, id))
        {
            state.set_slot_wait(ticket.seq);
            return Ok(Step::Yield);
        }
        let Some(entry) = state.loaded.get_mut(id) else {
            return Ok(Step::Wait);
        };
        entry.batch.insert(holder.to_string());
        let restore = entry.restore_handle();
        // The permits are taken (or the call queued) under the lock, so the model cannot be
        // swapped out between finding it and using it. Tokens come first, then the slot.
        let reservation = pool.reservation(request);
        let tokens = pool
            .tokens
            .as_ref()
            .map(|tokens| Arc::clone(tokens).try_acquire_many_owned(reservation.tokens));
        let step = match tokens {
            Some(Err(_)) if priority == Priority::Interactive => {
                Step::Queued(Queued::new(pool, None, &reservation))
            }
            Some(Err(_)) => {
                state.set_slot_wait(ticket.seq);
                return Ok(Step::Yield);
            }
            tokens => {
                let tokens = tokens.and_then(Result::ok);
                match Arc::clone(&pool.permits).try_acquire_owned() {
                    Ok(slot) => {
                        let granted = Granted {
                            slot,
                            tokens,
                            max_tokens: reservation.max_tokens,
                        };
                        Step::Leased(pool.lease(granted, holder, priority)?, restore)
                    }
                    Err(_) if priority == Priority::Interactive => {
                        Step::Queued(Queued::new(pool, tokens, &reservation))
                    }
                    Err(_) => {
                        state.set_slot_wait(ticket.seq);
                        return Ok(Step::Yield);
                    }
                }
            }
        };
        state.set_admitted(ticket.seq, true);
        Ok(step)
    }

    /// A lease from permits granted after queueing, if the model is still the one queued for.
    fn lease_if_current(
        &self,
        id: &str,
        pool: &Arc<SlotPool>,
        granted: Granted,
        holder: &str,
        priority: Priority,
        seq: u64,
    ) -> Result<Option<(LocalLease, Restore)>, String> {
        let mut state = lock(&self.state);
        let current = state
            .loaded
            .get(id)
            .filter(|entry| Arc::ptr_eq(&entry.pool, pool))
            .map(Loaded::restore_handle);
        let Some(restore) = current else {
            state.set_admitted(seq, false);
            return Ok(None);
        };
        Ok(Some((pool.lease(granted, holder, priority)?, restore)))
    }

    /// Decides, with loads held for this call, whether to load `model` now and what to swap
    /// out for it.
    fn plan(
        &self,
        model: &LocalModel,
        holder: &str,
        priority: Priority,
        ticket: &Ticket<'_>,
    ) -> Result<Plan, String> {
        let launch = self.launch(&model.id);
        let drafting = self.drafting(model, &launch, launch.pool_tokens);
        let cpu = self.cpu_slots(&model.id).is_some();
        let footprint = drafted_footprint(
            model,
            &launch,
            drafting.draft(),
            cpu,
            self.memory_factor(&model.id, cpu),
        );
        let no_room = || {
            format!(
                "not enough memory for {} (needs {} MiB)",
                model.id,
                footprint.required_bytes() / MIB
            )
        };
        let backend = Arc::clone(self.backend(model.format).ok_or_else(no_room)?);
        let mut state = lock(&self.state);
        // Held until the victims have left the loaded set: a claim either counts, or arrives
        // after the model is gone (its holder then reloads it on its next call).
        let users = lock(&self.presence.users);
        if let Some(entry) = state.loaded.get(&model.id) {
            // Stale: it restarts once free, and this call waits for the new shape.
            return Ok(if entry.stale {
                Plan::Wait { yielded: false }
            } else {
                Plan::Retry
            });
        }
        if state.behind_another_model(ticket.seq, &model.id, priority) {
            return Ok(Plan::Retry);
        }
        // A server that keeps crashing stays down a while before it is loaded again.
        if let Some(crash) = state.crashes.get(&model.id) {
            let decider = Arc::clone(&*lock(&self.policy));
            if crash.at.elapsed() < decider.crash_backoff(&model.id, crash.streak) {
                return Ok(Plan::Backoff);
            }
        }
        // A background call lets the user's calls load their models first, and does not
        // swap out a model the user used lately.
        let protect = (priority == Priority::Background).then(|| lock(&self.background).hold);
        if protect.is_some() && state.interactive_load_pending() {
            return Ok(Plan::Wait { yielded: true });
        }
        let requester = Some(holder_session(holder));
        let (residents, protected) = self.residents(&state, &users, protect, requester);
        let policy = self.policy(&mut state);
        // A CPU-lane model (the critic) runs on the CPU beside the agents generating on the
        // GPU; when the agent it reviews waits on it, nothing generates and no other session
        // is working, it gets the GPU too: the models in the way give way like any (their
        // agents' slots saved, restored when they resume).
        let lane = self.cpu_lane(&model.id).is_some();
        let on_cpu = lane
            && (residents.iter().any(|r| r.generating)
                || !critic_may_have_gpu(&users, holder_session(holder)));
        let budget = self.budget();
        let decider = Arc::clone(&*lock(&self.policy));
        let place = self.placer_on(model, Arc::clone(&backend), on_cpu);
        let ask = |residents: &[Resident]| {
            decider.decide(&LoadAsk {
                model: &model.id,
                holder,
                priority,
                place: &place,
                budget,
                residents,
                waited: ticket.since.elapsed(),
                limits: policy,
            })
        };
        let decision = ask(&residents);
        let (evict, placement, why) = match decision {
            Decision::TooBig => return Err(no_room()),
            Decision::SwapsOff => {
                return Err(format!(
                    "{}: it does not fit beside the loaded models, and swapping is off \
                     (runtime.llamacpp.max_swaps_per_min = 0)",
                    no_room()
                ));
            }
            Decision::Wait => {
                // The wait is giving way to the user only if the model the user used lately is
                // what holds this load up; any other wait keeps the `max_wait_s` cap.
                let yielded = protected && {
                    let (unprotected, _) = self.residents(&state, &users, None, requester);
                    !matches!(ask(&unprotected), Decision::Wait)
                };
                return Ok(Plan::Wait { yielded });
            }
            Decision::Load {
                evict,
                placement,
                why,
            } => (evict, placement, why),
        };
        // The critic taking the GPU from the agents it reviews says so.
        let why = if lane && !on_cpu && !evict.is_empty() {
            Reason::Critic
        } else {
            why
        };
        let cache_room = arbiter::cache_room(budget, &residents, &evict, &placement);
        // Out of the loaded set at once, so nobody takes a slot on them while they are saved.
        let victims: Vec<_> = evict
            .iter()
            .filter_map(|id| state.loaded.remove_entry(id))
            .collect();
        drop(users);
        for (id, _) in &victims {
            self.shrink(id);
        }
        if !victims.is_empty() {
            state.swaps.push_back(tokio::time::Instant::now());
        }
        Ok(Plan::Load(Box::new(Load {
            backend,
            placement,
            cache_room,
            victims,
            call: ticket.seq,
            priority,
            served_pool: self.placed_pool(model, &launch, drafting.draft(), &placement),
            launch,
            cpu_pinned: on_cpu,
            drafting,
            why,
        })))
    }

    /// Swaps out the plan's victims, loads `model`, and gives `holder` its first slot.
    async fn swap_in(
        &self,
        model: &LocalModel,
        holder: &str,
        request: &LocalRequest,
        load: Load,
        swapping: OwnedMutexGuard<()>,
        cause: Reason,
    ) -> Result<LocalLease, String> {
        let Load {
            backend,
            mut placement,
            cache_room,
            victims,
            call,
            priority,
            mut launch,
            cpu_pinned,
            served_pool,
            drafting,
            why,
        } = load;
        // A pool cut to keep the layers on the GPU beside the loaded models: one agent may use
        // at most all of it. `launch` stays what the settings ask, so the model is not taken
        // for stale.
        let squeezed = served_pool < launch.pool_tokens;
        let served = ModelLaunch {
            pool_tokens: served_pool,
            context: if squeezed {
                launch.context.min(served_pool.max(1))
            } else {
                launch.context
            },
            ..launch.clone()
        };
        if squeezed {
            tracing::info!(model = %model.id, pool_tokens = served_pool, wanted = launch.pool_tokens, "the KV pool is cut to keep every layer on the GPU beside the loaded models");
        }
        let replaced = !victims.is_empty();
        let swapping = self
            .swap_out_detached(victims, why, Some(&model.id), swapping)
            .await?;
        lock(&self.waits).insert(holder.to_string(), LocalWait::Loading);
        // The prompt cache takes what the setting asks of the RAM left after the model, and
        // counts against the budget like the model does.
        let cache_ram_mb = if backend.slot_aware() {
            prompt_cache_mb(self.settings.cache_ram_mb, &placement, cache_room)
        } else {
            0
        };
        let cache_bytes = cache_ram_mb * MIB;
        placement.ram_bytes += cache_bytes;
        let files = self.slot_files.as_ref().filter(|_| backend.slot_aware());
        let key = files.map(|_| slot_files::model_key(model, &launch));
        // The runtime's own arguments, then the model's, as the plan captured them: a settings
        // change while this loads marks the new server stale instead of half-applying.
        let mut extra_args = backend.extra_args();
        extra_args.extend(
            launch
                .extra_args
                .strip_prefix(self.settings.extra_args.as_slice())
                .unwrap_or(&launch.extra_args)
                .iter()
                .cloned(),
        );
        let mut draft_notice = drafting.notice().map(str::to_string);
        if let Some(notice) = &draft_notice {
            tracing::warn!(model = %model.id, notice, "the configured draft model is not used");
        }
        let mut spec = LaunchSpec {
            backend: Arc::clone(&backend),
            format: model.format,
            gpu_memory_utilization: backend.gpu_memory_utilization(&placement, self.budget()),
            draft: drafting.draft().map(|draft| DraftSpec {
                path: draft.model.path.clone(),
                max: draft.max,
                min: draft.min,
                gpu_layers: draft.gpu_layers,
            }),
            model_id: model.id.clone(),
            model_path: model.path.clone(),
            mmproj: model.mmproj.clone(),
            context: served.context,
            pool_tokens: served.pool_tokens,
            slots: launch.slots,
            gpu_layers: placement.gpu_layers,
            cpu_only: cpu_pinned,
            cache_type_k: launch.cache_type_k.clone(),
            cache_type_v: launch.cache_type_v.clone(),
            flash_attn: launch.flash_attn.clone(),
            cache_ram_mb,
            slot_save_path: files.map(|files| lock(files).dir().to_path_buf()),
            extra_args,
        };
        tracing::info!(model = %model.id, gpu_layers = placement.gpu_layers, cpu_pinned, slots = launch.slots, pool_tokens = served.pool_tokens, context = served.context, squeezed, cache_ram_mb, "loading local model");
        if launch.pool_clamped {
            tracing::warn!(model = %model.id, pool_tokens = launch.pool_tokens, "the model's fixed KV pool does not fit beside its weights on the GPU; it is cut to what fits so the layers stay on the GPU");
        }
        if !cpu_pinned && !placement.all_layers_on_gpu && self.budget().vram_bytes > 0 {
            tracing::warn!(model = %model.id, gpu_layers = placement.gpu_layers, "the model's weights do not fit on the GPU beside what is loaded; some layers run on the CPU");
        }
        let started = Instant::now();
        // A quick failure, such as the process exiting while it starts, is tried once more; one
        // that ran out the launcher's time is not. With a draft, a second quick failure is
        // tried once without it (the draft may be what the server refused); only if that start
        // works is the draft held to blame and left out until the settings or models change.
        let mut launched = self.launcher.launch(&spec).await;
        if let Err(error) = &launched
            && !error.timed_out
        {
            tracing::warn!(model = %model.id, %error, "could not start the model; trying once more");
            launched = self.launcher.launch(&spec).await;
        }
        let mut draft_dropped = false;
        if let Err(error) = &launched
            && !error.timed_out
            && let Some(draft) = drafting.draft()
            && spec.draft.is_some()
        {
            let message = error.message.clone();
            tracing::warn!(model = %model.id, error = %message, draft = %draft.model.id, "could not start the model with its draft; trying without it");
            spec.draft = None;
            launched = self.launcher.launch(&spec).await;
            if launched.is_ok() {
                let reason = speculative::skipped(
                    &draft.model.id,
                    &format!("the server did not start with it ({message})"),
                );
                lock(&self.draft_failed)
                    .insert(model.id.clone(), (draft.model.id.clone(), reason.clone()));
                draft_notice = Some(reason);
                draft_dropped = true;
            }
        }
        let mut draft_used = spec
            .draft
            .as_ref()
            .and(drafting.draft())
            .map(|draft| draft.model.id.clone());
        let load_action = Action::Load {
            model: model.id.clone(),
        };
        let server: Arc<dyn Server> = match launched {
            Ok(server) => Arc::from(server),
            Err(error) => {
                drop(swapping);
                self.changed.notify_waiters();
                self.record(
                    Move::failed(load_action, cause, error.message.clone())
                        .took(started.elapsed().as_secs_f64(), 0),
                );
                return Err(error.message);
            }
        };
        // llama-server survives a draft it cannot use (a vocabulary that does not match, a model
        // that cannot roll its cache back): it logs the failure and runs without speculation.
        // Ask the running server, and hold the draft to blame when it is not in use.
        if draft_used.is_some()
            && let Some(draft) = drafting.draft()
            && server.speculating().await == Some(false)
        {
            let reason = speculative::skipped(
                &draft.model.id,
                "llama-server started without speculative decoding (the draft's vocabulary does \
                 not match the model's, or the model cannot roll its cache back; see its log)",
            );
            tracing::warn!(model = %model.id, notice = %reason, "the draft model is not in use");
            lock(&self.draft_failed)
                .insert(model.id.clone(), (draft.model.id.clone(), reason.clone()));
            draft_notice = Some(reason);
            draft_used = None;
            draft_dropped = true;
        }
        // A draft that is not loaded holds no memory: release what the plan reserved for it.
        if draft_dropped && let Some(draft) = drafting.draft() {
            release_draft(&mut placement, draft.memory(served.pool_tokens, cpu_pinned));
        }
        let seconds = started.elapsed().as_secs_f64();
        self.record(
            Move::done(load_action, cause)
                .took(seconds, placement.vram_bytes + placement.ram_bytes)
                .detail(format!(
                    "{} of {} layers on the GPU, KV pool {} tokens{}",
                    placement.gpu_layers,
                    model
                        .info
                        .block_count
                        .map_or_else(|| "?".to_string(), |n| n.to_string()),
                    served.pool_tokens,
                    match (&draft_used, &draft_notice) {
                        (Some(draft), _) => format!(", draft model {draft}"),
                        (None, Some(notice)) => format!(". {notice}"),
                        (None, None) => String::new(),
                    }
                )),
        );
        let pool = SlotPool::new(
            server.base_url().to_string(),
            &served,
            backend.slot_aware(),
            Arc::clone(&self.changed),
        );
        let restorable = key.map_or(0, |key| self.reserve_saved_slots(&pool, key));
        let reservation = pool.reservation(request);
        let tokens = match &pool.tokens {
            Some(tokens) => Some(
                Arc::clone(tokens)
                    .try_acquire_many_owned(reservation.tokens)
                    .map_err(|_| "a new server has no free tokens".to_string())?,
            ),
            None => None,
        };
        let slot = Arc::clone(&pool.permits)
            .try_acquire_owned()
            .map_err(|_| "a new server has no free slot".to_string())?;
        let granted = Granted {
            slot,
            tokens,
            max_tokens: reservation.max_tokens,
        };
        let lease = pool.lease(granted, holder, priority)?;
        // Settings changed while it loaded: it serves this call, then restarts once free.
        let now = self.launch(&model.id);
        let stale = launch.restarts_for(&now);
        launch.max_output_tokens = now.max_output_tokens;
        pool.max_output_tokens
            .store(now.max_output_tokens, Ordering::SeqCst);
        {
            let mut state = lock(&self.state);
            state.load_seconds.insert(model.id.clone(), seconds);
            state.set_admitted(call, true);
            state.loaded.insert(
                model.id.clone(),
                Loaded {
                    server: Arc::clone(&server),
                    runtime: backend.id().to_string(),
                    pool,
                    placement,
                    cache_bytes,
                    cpu_pinned,
                    key,
                    loaded_at: Instant::now(),
                    batch: HashSet::from([holder.to_string()]),
                    launch,
                    stale,
                    growing: false,
                    squeezed,
                    promoting: false,
                    crashed: false,
                    draft: draft_used,
                    draft_notice,
                },
            );
        }
        drop(swapping);
        self.changed.notify_waiters();
        if replaced || restorable > 0 {
            self.emit(SwapEvent {
                direction: SwapDirection::In,
                model_id: model.id.clone(),
                seconds,
                bytes: placement.vram_bytes + placement.ram_bytes,
                slots: restorable,
                reason: if replaced { "swap" } else { "restore" },
            });
        }
        let restore = Restore { server, key };
        Ok(self.restore(&model.id, holder, lease, restore, cause).await)
    }

    /// Gives the holders of slots saved from a model with `key` their slots back on its new
    /// server; returns how many.
    fn reserve_saved_slots(&self, pool: &SlotPool, key: u64) -> u32 {
        let Some(files) = &self.slot_files else {
            return 0;
        };
        let files = lock(files);
        let mut table = lock(&pool.table);
        let reserved = files
            .saved_for(key)
            .filter(|saved| table.reserve(saved.slot, &saved.holder))
            .count();
        u32::try_from(reserved).unwrap_or(u32::MAX)
    }

    /// Swaps out `victims` in a task of its own that holds loads (`swapping`) until their
    /// servers have exited, then hands loads back. A caller cancelled meanwhile does not cut it
    /// short: the slots are still saved and the servers still stopped, and the next load waits
    /// for them rather than starting while their memory is held.
    async fn swap_out_detached(
        &self,
        victims: Vec<(String, Loaded)>,
        reason: Reason,
        made_room_for: Option<&str>,
        swapping: OwnedMutexGuard<()>,
    ) -> Result<OwnedMutexGuard<()>, String> {
        if victims.is_empty() {
            return Ok(swapping);
        }
        let made_room_for = made_room_for.map(str::to_string);
        // Whoever calls holds an `Arc` to it, so it is always there.
        let Some(supervisor) = self.me.upgrade() else {
            self.swap_out(victims, reason, made_room_for.as_deref())
                .await;
            return Ok(swapping);
        };
        tokio::spawn(async move {
            supervisor
                .swap_out(victims, reason, made_room_for.as_deref())
                .await;
            swapping
        })
        .await
        .map_err(|error| format!("could not swap out a model: {error}"))
    }

    /// Saves the occupied slots of models taken out of the loaded set, then stops them.
    async fn swap_out(
        &self,
        victims: Vec<(String, Loaded)>,
        reason: Reason,
        made_room_for: Option<&str>,
    ) {
        if victims.is_empty() {
            return;
        }
        for (id, loaded) in victims {
            let started = Instant::now();
            let pool_tokens = loaded.pool.pool_tokens;
            let (bytes, slots) = self.save_slots(&id, &loaded, reason).await;
            // Wait for the process to exit, so the next model finds its memory free.
            loaded.server.stop().await;
            drop(loaded);
            let seconds = started.elapsed().as_secs_f64();
            tracing::info!(model = %id, reason = reason.name(), made_room_for, slots, bytes, seconds, "swapped out local model");
            let action = if reason == Reason::Grow {
                Action::ResizePool {
                    model: id.clone(),
                    tokens: lock(&self.grown).get(&id).copied().unwrap_or(pool_tokens),
                }
            } else {
                Action::Unload { model: id.clone() }
            };
            let mut moved = Move::done(action, reason).took(seconds, bytes);
            if let Some(newcomer) = made_room_for {
                moved = moved.detail(format!("made room for {newcomer}"));
            }
            self.record(moved);
            self.emit(SwapEvent {
                direction: SwapDirection::Out,
                model_id: id,
                seconds,
                bytes,
                slots,
                reason: reason.swap_label(),
            });
        }
        self.changed.notify_waiters();
    }

    /// Saves each slot holding the prompt of a holder that can still come back (one mid-turn or
    /// waiting for subagents, not one whose turn is over); returns the bytes written and slots
    /// saved.
    async fn save_slots(&self, id: &str, loaded: &Loaded, reason: Reason) -> (u64, u32) {
        let (Some(key), Some(files)) = (loaded.key, &self.slot_files) else {
            return (0, 0);
        };
        let present: HashSet<String> = lock(&self.presence.users).keys().cloned().collect();
        let occupied: Vec<_> = lock(&loaded.pool.table)
            .occupied()
            .into_iter()
            .filter(|(_, holder)| present.contains(holder))
            .collect();
        let (mut bytes, mut slots) = (0, 0);
        for (slot, holder) in occupied {
            let file = SlotFiles::file_name(key, &holder);
            // Until it is recorded, a file the save wrote is removed if the save fails or is
            // cut short, so nothing on disk escapes `swap_cache_mb`.
            let unrecorded = Forget {
                files,
                holder: &holder,
                key,
            };
            let action = Action::SaveSlot {
                model: id.to_string(),
                holder: holder.clone(),
            };
            match loaded.server.save_slot(slot, &file).await {
                Ok(saved) => {
                    bytes += saved.bytes;
                    slots += 1;
                    self.record(Move::done(action, reason).took(0.0, saved.bytes));
                    let saved = Saved {
                        holder: holder.clone(),
                        key,
                        slot,
                        file,
                        bytes: saved.bytes,
                    };
                    lock(files).record(saved, |holder| present.contains(holder));
                    unrecorded.keep();
                }
                Err(error) => {
                    tracing::warn!(%holder, slot, %error, "could not save a slot before unloading its model");
                    self.record(Move::failed(action, reason, error));
                }
            }
        }
        (bytes, slots)
    }

    /// Suspends, in the background, the slots of the agents waiting (for subagents, an
    /// approval, an answer) whose prompts sit on `model`, now that `holder` uses it: they
    /// hold cells nobody uses until they resume.
    ///
    /// Which of them give way is the policy's to say ([`ResidencyPolicy::yield_slots`]).
    fn yield_waiters(&self, model: &str, holder: &str) {
        if self.slot_files.is_none() {
            return;
        }
        let Some(me) = self.me.upgrade() else {
            return;
        };
        let waiting: Vec<String> = lock(&self.presence.users)
            .iter()
            .filter(|(other, user)| {
                other.as_str() != holder && user.model == model && user.waiting > 0
            })
            .map(|(other, _)| other.clone())
            .collect();
        if waiting.is_empty() {
            return;
        }
        let policy = Arc::clone(&*lock(&self.policy));
        for other in policy.yield_slots(model, holder, waiting) {
            let (me, model) = (Arc::clone(&me), model.to_string());
            tokio::spawn(async move { me.suspend_for(&model, &other, Reason::Yield).await });
        }
    }

    /// Suspends `holder`'s slot on the loaded model `model` while it waits (for subagents, an
    /// approval, an answer): saves its prompt to the slot files and frees its cells, so the
    /// other agents on the model use the pool. Its next call restores the prompt into whatever
    /// slot it gets ([`Supervisor::restore`]). Nothing happens when slots are not saved, the
    /// model is not loaded, or the holder holds no idle slot on it.
    pub async fn suspend_slot(&self, model: &str, holder: &str) {
        self.suspend_for(model, holder, Reason::Stopped).await;
    }

    /// [`Supervisor::suspend_slot`], reporting the move with `reason`; returns whether the slot
    /// was saved.
    async fn suspend_for(&self, model: &str, holder: &str, reason: Reason) -> bool {
        let Some(files) = &self.slot_files else {
            return false;
        };
        // The slot is taken like a call takes one, so nobody uses it while it is saved, and the
        // holder's next call on the model waits for the save (see `State::saving`).
        let found = {
            let mut state = lock(&self.state);
            // A yield planned while the holder waited, which has gone on since: its slot is in
            // use again, or about to be.
            if reason == Reason::Yield
                && !lock(&self.presence.users)
                    .get(holder)
                    .is_some_and(|user| user.model == model && user.waiting > 0)
            {
                return false;
            }
            let found = state.loaded.get(model).and_then(|entry| {
                let key = entry.key?;
                let permit = Arc::clone(&entry.pool.permits).try_acquire_owned().ok()?;
                let slot = lock(&entry.pool.table).take_own(holder)?;
                Some((
                    Arc::clone(&entry.server),
                    Arc::clone(&entry.pool),
                    key,
                    slot,
                    permit,
                ))
            });
            if found.is_some() {
                state.saving.insert((model.to_string(), holder.to_string()));
            }
            found
        };
        let Some((server, pool, key, slot, _permit)) = found else {
            return false;
        };
        // Cleared however the save ends, cancellation included.
        let _saving = SavingMark {
            supervisor: self,
            model,
            holder,
        };
        let action = Action::SaveSlot {
            model: model.to_string(),
            holder: holder.to_string(),
        };
        let started = Instant::now();
        let present: HashSet<String> = lock(&self.presence.users).keys().cloned().collect();
        let present = |holder: &str| present.contains(holder);
        let file = SlotFiles::file_name(key, holder);
        let unrecorded = Forget { files, holder, key };
        let kept = match server.save_slot(slot, &file).await {
            Ok(saved) => {
                lock(files).record(
                    Saved {
                        holder: holder.to_string(),
                        key,
                        slot,
                        file,
                        bytes: saved.bytes,
                    },
                    present,
                );
                unrecorded.keep();
                let erased = server.erase_slot(slot).await;
                let moved =
                    Move::done(action, reason).took(started.elapsed().as_secs_f64(), saved.bytes);
                match erased {
                    Ok(_) => {
                        tracing::info!(%holder, model, slot, tokens = saved.tokens, reason = reason.name(), "suspended a waiting agent's slot");
                        self.record(moved);
                        true
                    }
                    Err(error) => {
                        tracing::debug!(%holder, slot, %error, "could not free a suspended slot's cells");
                        self.record(moved.detail(format!("saved; its cells stay: {error}")));
                        false
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%holder, slot, %error, "could not save a waiting agent's slot");
                self.record(Move::failed(action, reason, error));
                false
            }
        };
        // Erased, the slot holds nobody's prompt: anyone may take it first.
        lock(&pool.table).give_back_suspended(slot, holder, kept);
        self.changed.notify_waiters();
        kept
    }

    /// Restores `holder`'s saved slot, if it has one for this server's model, into the slot it
    /// was just leased, before the lease is used.
    async fn restore(
        &self,
        model: &str,
        holder: &str,
        lease: LocalLease,
        restore: Restore,
        cause: Reason,
    ) -> LocalLease {
        let (Some(slot), Some(key), Some(files)) = (lease.slot, restore.key, &self.slot_files)
        else {
            return lease;
        };
        let Some(saved) = lock(files).find(holder, key) else {
            return lease;
        };
        // Cleared when the call's wait ends (see `Waiting`).
        lock(&self.waits).insert(holder.to_string(), LocalWait::RestoringContext);
        // It stays recorded while it is read, and is forgotten with its file once read, failed,
        // or cancelled, so no file is left on disk outside the index.
        let _forget = Forget { files, holder, key };
        let action = Action::RestoreSlot {
            model: model.to_string(),
            holder: holder.to_string(),
        };
        let started = Instant::now();
        match restore.server.restore_slot(slot, &saved.file).await {
            Ok(restored) => {
                tracing::info!(%holder, slot, tokens = restored.tokens, bytes = restored.bytes, "restored a saved slot");
                self.record(
                    Move::done(action, cause).took(started.elapsed().as_secs_f64(), restored.bytes),
                );
            }
            Err(error) => {
                tracing::warn!(%holder, slot, %error, "could not restore a saved slot; its prompt will be read again");
                self.record(Move::failed(action, cause, error));
            }
        }
        lease
    }

    fn emit(&self, event: SwapEvent) {
        // No receivers just means nobody is listening.
        let _ = self.events.send(event);
    }

    /// Reports a move to whoever listens ([`Supervisor::moves`]).
    fn record(&self, moved: Move) {
        let _ = self.moves.send(moved);
    }

    /// Unloads models nobody is using and nobody has used for `idle_for`, saving their slots. A
    /// model an agent is mid-turn on (even between its calls) stays. Used by the daemon's tick
    /// and by tests.
    pub async fn unload_idle_now(&self, idle_for: Duration) {
        // A swap in progress has the models' memory in hand; look again next time.
        let Ok(swapping) = Arc::clone(&self.swapping).try_lock_owned() else {
            return;
        };
        let victims: Vec<_> = {
            let mut state = lock(&self.state);
            let users = lock(&self.presence.users);
            let idle: Vec<String> = state
                .loaded
                .iter()
                .filter(|(id, entry)| {
                    !entry.busy()
                        && lock(&entry.pool.last_used).elapsed() >= idle_for
                        && Presence::claims(&users, id, &entry.batch, None) == Claim::None
                })
                .map(|(id, _)| id.clone())
                .collect();
            idle.iter()
                .filter_map(|id| state.loaded.remove_entry(id))
                .collect()
        };
        // Its sessions are done with it: it starts with a small pool again.
        for (id, _) in &victims {
            self.shrink(id);
        }
        if let Err(error) = self
            .swap_out_detached(victims, Reason::Idle, None, swapping)
            .await
        {
            tracing::warn!(%error, "could not unload idle models");
        }
    }

    /// Unloads the loaded models whose settings changed and that nobody is using: no call lease,
    /// no queued call, no agent of any session mid-turn on them. A model whose pool must grow
    /// goes as soon as no call runs on it, between its agents' calls: they are mid-turn by
    /// definition, one of them waiting for it. Waits its turn behind a load or unload in flight.
    /// Used by the daemon's tick and by tests.
    pub async fn unload_stale_now(&self) {
        let swapping = Arc::clone(&self.swapping).lock_owned().await;
        self.restart_where(swapping, |_| true).await;
    }

    /// Restarts `id` if it waits to grow its pool or to move to the GPU and no call runs on it:
    /// the call waiting for it does this ([`Step::Restart`]).
    async fn restart_flagged(&self, id: &str, swapping: OwnedMutexGuard<()>) {
        self.restart_where(swapping, |candidate| {
            candidate.0 == id && (candidate.1.growing || candidate.1.promoting)
        })
        .await;
    }

    /// Restarts the stale models `chosen` picks that nobody is using (see
    /// [`Supervisor::unload_stale_now`]), holding loads (`swapping`) while they stop.
    async fn restart_where(
        &self,
        swapping: OwnedMutexGuard<()>,
        chosen: impl Fn((&String, &Loaded)) -> bool,
    ) {
        let victims: Vec<_> = {
            let mut state = lock(&self.state);
            let users = lock(&self.presence.users);
            let stale: Vec<String> = state
                .loaded
                .iter()
                .filter(|(id, entry)| {
                    entry.stale
                        && !entry.busy()
                        && (entry.growing
                            || entry.promoting
                            || Presence::claims(&users, id, &entry.batch, None) == Claim::None)
                })
                .filter(|&(id, entry)| chosen((id, entry)))
                .map(|(id, _)| id.clone())
                .collect();
            stale
                .iter()
                .filter_map(|id| state.loaded.remove_entry(id))
                .collect()
        };
        // Slots saved under a key the restarted server will not have could never be restored:
        // they are not saved.
        let victims = victims
            .into_iter()
            .map(|(id, mut entry)| {
                let restorable = self.model(&id).is_some_and(|model| {
                    entry.key == Some(slot_files::model_key(&model, &self.launch(&id)))
                });
                if !restorable {
                    entry.key = None;
                }
                (id, entry)
            })
            .collect::<Vec<_>>();
        let (promote, victims): (Vec<_>, Vec<_>) =
            victims.into_iter().partition(|(_, entry)| entry.promoting);
        let (grow, config): (Vec<_>, Vec<_>) =
            victims.into_iter().partition(|(_, entry)| entry.growing);
        let swapping = match self
            .swap_out_detached(promote, Reason::Promote, None, swapping)
            .await
        {
            Ok(swapping) => swapping,
            Err(error) => {
                tracing::warn!(%error, "could not restart models to place them on the GPU");
                return;
            }
        };
        let swapping = match self
            .swap_out_detached(grow, Reason::Grow, None, swapping)
            .await
        {
            Ok(swapping) => swapping,
            Err(error) => {
                tracing::warn!(%error, "could not restart models to grow their KV pool");
                return;
            }
        };
        if let Err(error) = self
            .swap_out_detached(config, Reason::Config, None, swapping)
            .await
        {
            tracing::warn!(%error, "could not restart models after a settings change");
        }
        self.changed.notify_waiters();
    }

    /// Why a call for `id` that must wait its turn waits.
    fn why_waiting(&self, id: &str) -> LocalWait {
        match lock(&self.state).loaded.get(id) {
            // Its server exited: it is loaded again once the controller has unloaded it.
            Some(entry) if entry.crashed => LocalWait::Loading,
            Some(entry) if entry.growing => LocalWait::GrowingPool,
            Some(entry) if entry.promoting => LocalWait::MakingRoom,
            // Stale: it restarts with new settings once free.
            Some(entry) if entry.stale => LocalWait::Loading,
            _ => LocalWait::OtherModel,
        }
    }
}

/// Whether the CPU-lane model a call of `session` asks for (the critic) may have the GPU: an
/// agent of its own session is waiting on it (for its review), and no other session is working.
fn critic_may_have_gpu(users: &HashMap<String, User>, session: &str) -> bool {
    let waited_on = users
        .iter()
        .any(|(holder, user)| holder_session(holder) == session && user.waiting > 0);
    waited_on && !other_session_working(users, session)
}

/// Whether an agent of a session other than `session` is mid-turn and not waiting.
fn other_session_working(users: &HashMap<String, User>, session: &str) -> bool {
    users.iter().any(|(holder, user)| {
        holder_session(holder) != session && user.working > 0 && user.waiting == 0
    })
}

/// What a holder's call waits for, while it waits; dropping it says it no longer does.
struct Waiting<'a> {
    waits: &'a Mutex<HashMap<String, LocalWait>>,
    holder: &'a str,
}

impl Waiting<'_> {
    fn set(&self, reason: LocalWait) {
        lock(self.waits).insert(self.holder.to_string(), reason);
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        lock(self.waits).remove(self.holder);
    }
}

/// Removes a holder's saved slot from the index and the disk when dropped, unless kept.
struct Forget<'a> {
    files: &'a Mutex<SlotFiles>,
    holder: &'a str,
    key: u64,
}

impl Forget<'_> {
    fn keep(self) {
        std::mem::forget(self);
    }
}

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        lock(self.files).forget(self.holder, self.key);
    }
}

/// Marks a holder's slot on a model as being saved while held; dropping it lets the holder's
/// next call go on.
struct SavingMark<'a> {
    supervisor: &'a Supervisor,
    model: &'a str,
    holder: &'a str,
}

impl Drop for SavingMark<'_> {
    fn drop(&mut self) {
        lock(&self.supervisor.state)
            .saving
            .remove(&(self.model.to_string(), self.holder.to_string()));
        self.supervisor.changed.notify_waiters();
    }
}

/// A held slot; releasing it frees the slot for the next caller and records when the model
/// was last used.
struct Slot {
    pool: Arc<SlotPool>,
    index: u32,
    holder: String,
    /// Held by one of the user's own calls.
    interactive: bool,
    /// Released after `drop` has put the slot back, so a waiter that wakes finds it free.
    _permit: OwnedSemaphorePermit,
    /// The call's reservation from the model's token pool, released with the slot.
    _tokens: Option<OwnedSemaphorePermit>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        lock(&self.pool.table).give_back(self.index, &self.holder);
        *lock(&self.pool.last_used) = Instant::now();
        if self.interactive {
            *lock(&self.pool.last_interactive) = Some(Instant::now());
            self.pool.held_interactive.fetch_sub(1, Ordering::SeqCst);
        }
        self.pool.changed.notify_waiters();
    }
}

impl Supervisor {
    /// Where `id` stands for a requester of `session` (`None`: every claim is another session's).
    fn availability_of(&self, id: &str, session: Option<&str>) -> Availability {
        let Some(model) = self.model(id) else {
            return Availability::Unavailable;
        };
        let Some(backend) = self.backend(model.format) else {
            return Availability::Unavailable;
        };
        let place = self.placer(&model, Arc::clone(backend));
        let mut state = lock(&self.state);
        if let Some(entry) = state.loaded.get(id) {
            return if entry.pool.permits.available_permits() > 0 {
                Availability::Loaded
            } else {
                // Busy: it can take the work once a slot frees.
                Availability::Fits
            };
        }
        let users = lock(&self.presence.users);
        let (residents, _) = self.residents(&state, &users, None, session);
        drop(users);
        let policy = self.policy(&mut state);
        let outlook = arbiter::outlook(place, self.budget(), &residents, policy);
        match outlook {
            Outlook::Fits => Availability::Fits,
            Outlook::Swap => Availability::Swap {
                seconds: self.swap_seconds(&state, &model),
            },
            Outlook::Unavailable => Availability::Unavailable,
        }
    }
}

impl LocalModels for Supervisor {
    fn availability(&self, id: &str) -> Availability {
        self.availability_of(id, None)
    }

    fn availability_for(&self, id: &str, session: &str) -> Availability {
        self.availability_of(id, Some(session))
    }

    fn has_model(&self, id: &str) -> bool {
        lock(&self.models).iter().any(|model| model.id == id)
    }

    fn reasons(&self, id: &str) -> Option<bool> {
        let set = lock(&self.model_settings)
            .get(id)
            .and_then(|settings| settings.reasoning);
        set.or_else(|| {
            lock(&self.models)
                .iter()
                .find(|model| model.id == id)?
                .info
                .template_reasons
        })
    }

    fn planned_context(&self, id: &str) -> Option<u64> {
        if !self.has_model(id) {
            return None;
        }
        // A loaded server's context, else what the model would be started with.
        let context = self
            .loaded()
            .into_iter()
            .find(|loaded| loaded.model_id == id)
            .map_or_else(|| self.launch(id).context, |loaded| loaded.context);
        (context > 0).then_some(u64::from(context))
    }

    fn wait_reason(&self, holder: &str) -> Option<LocalWait> {
        lock(&self.waits).get(holder).copied()
    }

    fn acquire<'a>(&'a self, id: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        self.acquire_with(id, holder, LocalRequest::default())
    }

    fn acquire_as<'a>(
        &'a self,
        id: &'a str,
        holder: &'a str,
        priority: Priority,
    ) -> AcquireFuture<'a> {
        self.acquire_with(
            id,
            holder,
            LocalRequest {
                priority,
                ..LocalRequest::default()
            },
        )
    }

    fn acquire_with<'a>(
        &'a self,
        id: &'a str,
        holder: &'a str,
        request: LocalRequest,
    ) -> AcquireFuture<'a> {
        Box::pin(async move {
            self.acquire_slot(id, holder, request, Reason::Call, true)
                .await
                .map_err(|error| error.to_string())
        })
    }

    /// Never loads, restarts, grows or moves a model: decided where slots are handed out, so a
    /// swap-out between a caller's check and its call is answered with
    /// [`AcquireError::NotLoaded`], not a load.
    fn acquire_loaded<'a>(
        &'a self,
        id: &'a str,
        holder: &'a str,
        request: LocalRequest,
    ) -> LoadedFuture<'a> {
        Box::pin(self.acquire_slot(id, holder, request, Reason::Call, false))
    }

    fn attach(&self, model: &str, holder: &str) -> LocalPresence {
        self.presence
            .enter(&self.changed, model, holder, Mark::Working)
    }

    /// While it waits, `holder`'s slot on `model` is suspended as soon as another agent takes a
    /// slot on the same model ([`Supervisor::suspend`]); its next call restores it.
    fn waiting(&self, model: &str, holder: &str) -> LocalPresence {
        self.presence
            .enter(&self.changed, model, holder, Mark::Waiting)
    }

    fn suspended(&self, holder: &str) -> bool {
        self.slot_files
            .as_ref()
            .is_some_and(|files| lock(files).holds(holder))
    }

    fn suspend<'a>(&'a self, model: &'a str, holder: &'a str) -> SuspendFuture<'a> {
        Box::pin(self.suspend_slot(model, holder))
    }

    /// Brings `holder`'s context back ahead of its next call, reported as [`Reason::Resume`].
    fn resume<'a>(&'a self, model: &'a str, holder: &'a str) -> ResumeFuture<'a> {
        Box::pin(self.bring_back(model, holder, Reason::Resume))
    }

    fn record_throughput(&self, model: &str, completion_tokens: u64, generating: Duration) {
        if completion_tokens == 0 || generating.is_zero() {
            return;
        }
        lock(&self.state)
            .throughput
            .entry(model.to_string())
            .or_default()
            .add(completion_tokens as f64, generating.as_secs_f64());
    }

    fn tokens_per_s(&self, model: &str) -> Option<f64> {
        Supervisor::tokens_per_s(self, model)
    }

    fn queue_wait_s(&self, model: &str) -> u32 {
        let state = lock(&self.state);
        let Some(loaded) = state.loaded.get(model) else {
            return 0;
        };
        if loaded.pool.permits.available_permits() > 0 {
            return 0;
        }
        // Every call queued ahead of this one, and this one, share the slots.
        let ahead = f64::from(loaded.pool.waiting.load(Ordering::SeqCst)) + 1.0;
        let slots = f64::from(loaded.pool.slots.max(1));
        let per_call = state
            .throughput
            .get(model)
            .and_then(Throughput::seconds_per_call)
            .unwrap_or(DEFAULT_CALL_SECONDS);
        (ahead / slots * per_call).ceil() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{LlamaCpp, Vllm};
    use crate::gguf::GgufInfo;
    use crate::hardware::Gpu;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const GIB: u64 = 1024 * 1024 * 1024;

    #[derive(Debug, Default)]
    struct FakeLauncher {
        launches: AtomicUsize,
        running: Arc<AtomicUsize>,
        /// The runtime id and arguments of each launch.
        launched: Mutex<Vec<(String, Vec<String>)>>,
        /// A server given a draft model refuses to start (an incompatible draft).
        refuse_drafts: std::sync::atomic::AtomicBool,
        /// Every start fails.
        fail_all: std::sync::atomic::AtomicBool,
        /// The next this many starts fail (a transient failure).
        fail_first: AtomicUsize,
        /// Every start runs out of time.
        time_out: std::sync::atomic::AtomicBool,
        /// The server starts but reports that it does not speculate.
        no_speculation: std::sync::atomic::AtomicBool,
    }

    struct FakeServer {
        url: String,
        running: Arc<AtomicUsize>,
        speculating: Option<bool>,
    }

    impl Server for FakeServer {
        fn base_url(&self) -> &str {
            &self.url
        }

        fn speculating(&self) -> SpeculatingFuture<'_> {
            let answer = self.speculating;
            Box::pin(async move { answer })
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.running.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Launcher for FakeLauncher {
        fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
            Box::pin(async move {
                self.launches.fetch_add(1, Ordering::SeqCst);
                lock(&self.launched).push((spec.backend.id().to_string(), spec.args()));
                if self.time_out.load(Ordering::SeqCst) {
                    return Err(LaunchError::timed_out(
                        "the model did not load within 600 s".to_string(),
                    ));
                }
                if self.fail_all.load(Ordering::SeqCst) {
                    return Err(LaunchError::from("the server refused to start"));
                }
                let transient =
                    self.fail_first
                        .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                            left.checked_sub(1)
                        });
                if transient.is_ok() {
                    return Err(LaunchError::from("the server exited while starting"));
                }
                if spec.draft.is_some() && self.refuse_drafts.load(Ordering::SeqCst) {
                    return Err(LaunchError::from("draft vocabulary does not match"));
                }
                self.running.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(Box::new(FakeServer {
                    url: format!("http://fake/{}/v1", spec.model_id),
                    running: Arc::clone(&self.running),
                    speculating: (spec.draft.is_some()
                        && self.no_speculation.load(Ordering::SeqCst))
                    .then_some(false),
                }) as Box<dyn Server>)
            })
        }
    }

    /// A model of `gib` GiB of weights with 32 layers and a small KV cache.
    fn model(id: &str, gib: u64) -> LocalModel {
        LocalModel {
            id: id.into(),
            path: PathBuf::from(format!("{id}.gguf")),
            parts: 1,
            size_bytes: gib * GIB,
            info: GgufInfo {
                block_count: Some(32),
                embedding_length: Some(4096),
                head_count: Some(32),
                head_count_kv: Some(8),
                ..GgufInfo::default()
            },
            mmproj: None,
            repo: None,
            format: Format::Gguf,
        }
    }

    fn settings() -> LlamaCppSettings {
        LlamaCppSettings {
            backend: "auto".into(),
            custom_path: None,
            find_existing: true,
            auto_install: false,
            update_external: "off".into(),
            idle_unload_s: 900,
            context: 4096.into(),
            parallel: 2,
            cache_type: "f16".into(),
            slots: Some(2),
            pool_tokens: None,
            flash_attn: "auto".into(),
            max_output_tokens: 8192,
            models: Default::default(),
            ram_headroom_mb: 0,
            cache_ram_mb: 0,
            swap_cache_mb: 0,
            min_residency_s: 30,
            max_swaps_per_min: 6,
            swap_gain: 2.0,
            max_wait_s: 600,
            extra_args: Vec::new(),
        }
    }

    /// 10 GiB of VRAM, no spare system memory.
    fn hardware() -> Hardware {
        Hardware {
            gpus: vec![Gpu {
                name: "test".into(),
                vendor: "nvidia".into(),
                vram_total_bytes: 10 * GIB,
                vram_free_bytes: 10 * GIB,
                driver: None,
            }],
            ram_total_bytes: 0,
            ram_available_bytes: 0,
            cpu_cores: 8,
            unified_memory: false,
            overridden: true,
        }
    }

    /// Room for one 6 GiB model at a time.
    fn supervisor(launcher: Arc<FakeLauncher>) -> Arc<Supervisor> {
        Supervisor::start(
            vec![model("a", 6), model("b", 6), model("huge", 40)],
            settings(),
            Runtimes::default(),
            &hardware(),
            launcher,
        )
    }

    #[tokio::test]
    async fn a_measured_peak_raises_what_the_model_is_planned_with() {
        let supervisor = supervisor(Arc::new(FakeLauncher::default()));
        let calibration = Arc::new(Calibration::in_memory("test"));
        // Nothing is recorded before a calibration is set.
        assert_eq!(
            supervisor.record_measured_memory("a", Some(1), Some(1)),
            None
        );
        supervisor.set_calibration(Arc::clone(&calibration));
        let lease = supervisor.acquire("a", "holder").await.unwrap();
        let model = supervisor.model("a").unwrap();
        let before = supervisor.footprint(&model).required_bytes();
        let loaded = supervisor.loaded().remove(0);
        // Wherever it runs, 20% over what was planned for it.
        let over = |planned: u64| planned / 10 * 12;
        let factor = supervisor
            .record_measured_memory(
                "a",
                Some(over(loaded.ram_bytes)),
                Some(over(loaded.vram_bytes)),
            )
            .expect("a load with memory planned is calibrated");
        assert!((1190..=1200).contains(&factor), "{factor}");
        assert!(supervisor.footprint(&model).required_bytes() > before);
        // A model that is not loaded has no peak to compare.
        assert_eq!(
            supervisor.record_measured_memory("b", Some(1), Some(1)),
            None
        );
        drop(lease);
    }

    #[tokio::test]
    async fn loaded_models_are_reused_and_concurrent_requests_load_once() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        assert_eq!(supervisor.availability("a"), Availability::Fits);
        assert_eq!(supervisor.availability("huge"), Availability::Unavailable);
        assert_eq!(
            supervisor.availability("missing"),
            Availability::Unavailable
        );

        let (first, second) = tokio::join!(
            supervisor.acquire("a", "holder"),
            supervisor.acquire("a", "holder")
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!(first.base_url, "http://fake/a/v1");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
        // Both slots are taken, so it reports busy-but-usable; the other model does not fit
        // beside a model in use.
        assert_eq!(supervisor.availability("a"), Availability::Fits);
        assert_eq!(supervisor.availability("b"), Availability::Unavailable);
        drop(second);
        assert_eq!(supervisor.availability("a"), Availability::Loaded);
        // "b" waits for room instead of failing, and "a" is not swapped out while in use.
        let waiting = tokio::time::timeout(
            Duration::from_millis(100),
            supervisor.acquire("b", "holder"),
        )
        .await;
        assert!(waiting.is_err(), "b waits while a is in use");
        assert_eq!(supervisor.loaded()[0].model_id, "a");
        let error = supervisor.acquire("huge", "holder").await.unwrap_err();
        assert!(error.contains("not enough memory"), "{error}");
        drop(first);
        assert_eq!(supervisor.loaded()[0].slots_in_use, 0);
    }

    #[tokio::test]
    async fn idle_models_make_room_and_unload_after_the_timeout() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        drop(supervisor.acquire("a", "holder").await.unwrap());
        // "a" is idle, so "b" may swap it out.
        assert!(matches!(
            supervisor.availability("b"),
            Availability::Swap { .. }
        ));
        let b = supervisor.acquire("b", "holder").await.unwrap();
        assert_eq!(launcher.running.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.loaded()[0].model_id, "b");
        supervisor.unload_idle_now(Duration::ZERO).await;
        assert_eq!(supervisor.loaded().len(), 1, "a model in use stays loaded");
        drop(b);
        supervisor.unload_idle_now(Duration::ZERO).await;
        assert!(supervisor.loaded().is_empty());
        assert_eq!(launcher.running.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_model_attached_to_a_turn_stays_loaded_between_its_calls() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        // Session A's turn on "a" is between calls: it holds no slot but is still attached, so
        // "a" is claimed.
        let attached = supervisor.attach("a", "session-a/root");
        drop(supervisor.acquire("a", "session-a/root").await.unwrap());
        assert_eq!(supervisor.loaded()[0].slots_in_use, 0);
        // "b" is not refused: it waits for A's turn (up to `min_residency_s`), and "a" is not
        // swapped out for it meanwhile.
        let b = {
            let supervisor = Arc::clone(&supervisor);
            tokio::spawn(async move { supervisor.acquire("b", "session-b/root").await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!b.is_finished(), "b waits while a's turn is attached");
        assert_eq!(
            supervisor.loaded()[0].model_id,
            "a",
            "an attached model stays loaded"
        );
        // A's turn ends, so "a" is free and "b" swaps it out.
        drop(attached);
        let b = b.await.unwrap().unwrap();
        assert_eq!(b.base_url, "http://fake/b/v1");
        assert_eq!(launcher.running.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.loaded()[0].model_id, "b");
    }

    #[tokio::test]
    async fn the_queue_wait_leaves_out_loading() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(launcher);
        let first = supervisor.acquire("a", "first").await.unwrap();
        assert_eq!(
            first.waited,
            Duration::ZERO,
            "a load is not a wait for a slot"
        );
        let second = supervisor.acquire("a", "second").await.unwrap();
        let waiter = {
            let supervisor = Arc::clone(&supervisor);
            tokio::spawn(async move { supervisor.acquire("a", "third").await.unwrap().waited })
        };
        while supervisor.loaded()[0].queued == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(first);
        assert!(waiter.await.unwrap() >= Duration::from_millis(20));
        drop(second);
    }

    #[tokio::test]
    async fn status_reports_idle_countdown_and_unload_spares_models_in_use() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        let lease = supervisor.acquire("a", "session-a/root").await.unwrap();
        let status = &supervisor.loaded()[0];
        assert_eq!(status.runtime, "llamacpp");
        assert_eq!(status.slots_in_use, 1);
        assert_eq!((status.idle_for, status.unload_in), (None, None));
        assert_eq!(status.pid, None);
        assert_eq!(
            supervisor.unload("a").await,
            Err(UnloadError::InUse("a".into(), 1))
        );
        drop(lease);
        // An agent mid-turn on it, between its calls, keeps it too.
        let attached = supervisor.attach("a", "session-a/root");
        let status = &supervisor.loaded()[0];
        assert!(status.claimed);
        assert_eq!(status.idle_for, None);
        assert_eq!(
            supervisor.unload("a").await,
            Err(UnloadError::Claimed("a".into()))
        );
        drop(attached);
        let status = &supervisor.loaded()[0];
        assert!(!status.claimed);
        assert!(status.idle_for.is_some());
        assert!(status.unload_in.unwrap() <= Duration::from_secs(900));
        assert_eq!(supervisor.unload("a").await, Ok(()));
        assert_eq!(launcher.running.load(Ordering::SeqCst), 0);
        assert_eq!(
            supervisor.unload("a").await,
            Err(UnloadError::NotLoaded("a".into()))
        );
    }

    #[test]
    fn launch_arguments_carry_the_plan() {
        let spec = LaunchSpec {
            draft: None,
            backend: Arc::new(LlamaCpp::default()),
            format: Format::Gguf,
            gpu_memory_utilization: 0.9,
            model_id: "m".into(),
            model_path: PathBuf::from("m.gguf"),
            mmproj: Some(PathBuf::from("proj.gguf")),
            context: 8192,
            pool_tokens: 16384,
            slots: 2,
            gpu_layers: u32::MAX,
            cpu_only: false,
            cache_type_k: "q8_0".into(),
            cache_type_v: "f16".into(),
            flash_attn: "on".into(),
            cache_ram_mb: 2048,
            slot_save_path: Some(PathBuf::from("slots")),
            extra_args: vec!["--n-cpu-moe".into(), "4".into()],
        };
        let args = spec.args().join(" ");
        assert!(
            args.contains("--n-gpu-layers 999 --cache-type-k q8_0"),
            "{args}"
        );
        assert!(!args.contains("--device"), "{args}");
        let lane = LaunchSpec {
            gpu_layers: 0,
            cpu_only: true,
            ..spec.clone()
        };
        let lane_args = lane.args().join(" ");
        assert!(lane_args.contains("--n-gpu-layers 0"), "{lane_args}");
        assert!(lane_args.contains("--device none"), "{lane_args}");
        // The lane's projector stays off the GPU too; a GPU row's is offloaded as usual.
        assert!(
            lane_args.contains("--mmproj proj.gguf --no-mmproj-offload"),
            "{lane_args}"
        );
        assert!(!args.contains("--no-mmproj-offload"), "{args}");
        assert!(args.contains("--cache-type-k q8_0"), "{args}");
        assert!(args.contains("--cache-ram 2048"), "{args}");
        assert!(args.contains("--slot-save-path slots"), "{args}");
        assert!(args.ends_with("--mmproj proj.gguf --n-cpu-moe 4"), "{args}");
        let vllm = LaunchSpec {
            backend: Arc::new(Vllm::default()),
            format: Format::Safetensors,
            model_path: PathBuf::from("/models/Qwen3-8B"),
            model_id: "Qwen3-8B".into(),
            gpu_memory_utilization: 0.456,
            extra_args: Vec::new(),
            ..spec
        };
        assert_eq!(
            vllm.args().join(" "),
            "serve /models/Qwen3-8B --host 127.0.0.1 --served-model-name Qwen3-8B \
             --max-model-len 8192 --max-num-seqs 2 --gpu-memory-utilization 0.46"
        );
    }

    #[tokio::test]
    async fn claims_of_another_session_count_as_busy() {
        let supervisor = supervisor(Arc::new(FakeLauncher::default()));
        let _turn = supervisor.attach("a", "first/root");
        drop(supervisor.acquire("a", "first/root").await.unwrap());
        let use_of = |requester: Option<&str>| {
            let state = lock(&supervisor.state);
            let users = lock(&supervisor.presence.users);
            let (residents, _) = supervisor.residents(&state, &users, None, requester);
            residents.into_iter().find(|r| r.id == "a").unwrap().use_
        };
        // The session's own claim can be outwaited; another session's never.
        assert_eq!(use_of(Some("first")), Use::Claimed);
        assert_eq!(use_of(Some("second")), Use::Busy);
        assert_eq!(use_of(None), Use::Busy);
        assert!(matches!(
            supervisor.availability_for("b", "first"),
            Availability::Swap { .. }
        ));
        assert_eq!(
            supervisor.availability_for("b", "second"),
            Availability::Unavailable
        );
        assert_eq!(supervisor.availability("b"), Availability::Unavailable);
    }

    #[test]
    fn llama_server_shares_one_kv_pool() {
        let spec = LaunchSpec {
            draft: None,
            backend: Arc::new(LlamaCpp::default()),
            format: Format::Gguf,
            gpu_memory_utilization: 0.9,
            model_id: "m".into(),
            model_path: PathBuf::from("m.gguf"),
            mmproj: None,
            context: 8192,
            pool_tokens: 32768,
            slots: 4,
            gpu_layers: 20,
            cpu_only: false,
            cache_type_k: "q8_0".into(),
            cache_type_v: "q4_0".into(),
            flash_attn: "on".into(),
            cache_ram_mb: 1024,
            slot_save_path: None,
            extra_args: Vec::new(),
        };
        // The pool, the unified flag, the per-slot cap, the slots, and the idle-slot flag come
        // in this order, then the layers, the cache types, flash attention, and the prompt cache.
        assert_eq!(
            spec.args().join(" "),
            "--model m.gguf --host 127.0.0.1 --ctx-size 32768 --kv-unified \
             --kv-unified-per-slot 8192 --parallel 4 --no-cache-idle-slots --n-gpu-layers 20 \
             --cache-type-k q8_0 --cache-type-v q4_0 --flash-attn on --cache-ram 1024 --jinja"
        );
        // With the whole pool as the context the cap cannot bind, so it is left out.
        let whole = LaunchSpec {
            context: 32768,
            ..spec.clone()
        };
        let args = whole.args().join(" ");
        assert!(!args.contains("--kv-unified-per-slot"), "{args}");
        assert!(
            args.contains("--ctx-size 32768 --kv-unified --parallel 4"),
            "{args}"
        );
        // vLLM takes the per-session context and the slots; it has no pool.
        let vllm = LaunchSpec {
            backend: Arc::new(Vllm::default()),
            format: Format::Safetensors,
            ..spec
        };
        let args = vllm.args().join(" ");
        assert!(
            args.contains("--max-model-len 8192 --max-num-seqs 4"),
            "{args}"
        );
    }

    #[tokio::test]
    async fn hugging_face_models_need_vllm() {
        let launcher = Arc::new(FakeLauncher::default());
        let mut hf = model("hf", 2);
        hf.format = Format::Safetensors;
        let hardware = hardware();
        let without = Supervisor::start(
            vec![hf.clone()],
            settings(),
            Runtimes::default(),
            &hardware,
            launcher.clone(),
        );
        assert_eq!(without.availability("hf"), Availability::Unavailable);
        let with = Supervisor::start(
            vec![hf],
            settings(),
            Runtimes {
                vllm_available: true,
                ..Runtimes::default()
            },
            &hardware,
            launcher.clone(),
        );
        assert_eq!(with.availability("hf"), Availability::Fits);
        let lease = with.acquire("hf", "holder").await.unwrap();
        assert_eq!(lease.base_url, "http://fake/hf/v1");
        let launched = lock(&launcher.launched);
        assert_eq!(launched[0].0, "vllm");
        assert_eq!(launched[0].1[0], "serve");
    }

    /// A runtime that serves Hugging Face folders anywhere, splitting them like llama.cpp.
    #[derive(Debug)]
    struct Custom;

    impl RuntimeBackend for Custom {
        fn id(&self) -> &str {
            "custom"
        }

        fn accepts(&self, format: Format) -> bool {
            format == Format::Safetensors
        }

        fn available(&self) -> bool {
            true
        }

        fn place(&self, footprint: Footprint, budget: Budget) -> Option<Placement> {
            crate::planner::place(footprint, budget)
        }

        fn gpu_memory_utilization(&self, _placement: &Placement, _budget: Budget) -> f64 {
            0.0
        }

        fn extra_args(&self) -> Vec<String> {
            vec!["--custom".into()]
        }

        fn launch_args(&self, spec: &LaunchSpec) -> Vec<String> {
            let mut args = vec!["run".into(), spec.model_id.clone()];
            args.extend(spec.extra_args.iter().cloned());
            args
        }
    }

    #[tokio::test]
    async fn a_registered_backend_serves_the_formats_it_accepts() {
        let launcher = Arc::new(FakeLauncher::default());
        let mut hf = model("hf", 2);
        hf.format = Format::Safetensors;
        // Ahead of the built-ins, where vLLM is unavailable.
        let mut backends: Vec<Arc<dyn RuntimeBackend>> = vec![Arc::new(Custom)];
        backends.extend(backend::builtin(&settings(), Runtimes::default()));
        let supervisor = Supervisor::with_backends(
            vec![hf, model("gguf", 2)],
            settings(),
            backends,
            &hardware(),
            launcher.clone(),
        );
        assert_eq!(supervisor.availability("hf"), Availability::Fits);
        drop(supervisor.acquire("hf", "holder").await.unwrap());
        drop(supervisor.acquire("gguf", "holder").await.unwrap());
        let launched = lock(&launcher.launched);
        assert_eq!(
            launched[0],
            (
                "custom".into(),
                vec!["run".into(), "hf".into(), "--custom".into()]
            )
        );
        assert_eq!(launched[1].0, "llamacpp");
    }

    #[tokio::test]
    async fn an_agent_gets_back_the_slot_it_used_last() {
        let supervisor = supervisor(Arc::new(FakeLauncher::default()));
        let (a, b) = tokio::join!(
            supervisor.acquire("a", "agent-a"),
            supervisor.acquire("a", "agent-b")
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        let (slot_a, slot_b) = (a.slot.unwrap(), b.slot.unwrap());
        assert_ne!(slot_a, slot_b);
        // b's slot has been free the longest, yet a gets its own back.
        drop(b);
        drop(a);
        let again = supervisor.acquire("a", "agent-a").await.unwrap();
        assert_eq!(again.slot, Some(slot_a));
        drop(again);
        let again = supervisor.acquire("a", "agent-b").await.unwrap();
        assert_eq!(again.slot, Some(slot_b));
    }

    #[tokio::test]
    async fn a_newcomer_takes_the_slot_free_the_longest() {
        let supervisor = supervisor(Arc::new(FakeLauncher::default()));
        let first = supervisor.acquire("a", "agent-a").await.unwrap();
        let second = supervisor.acquire("a", "agent-b").await.unwrap();
        let (slot_a, slot_b) = (first.slot.unwrap(), second.slot.unwrap());
        // a is free longer than b, so the newcomer overwrites a's prompt and keeps b's.
        drop(first);
        drop(second);
        let newcomer = supervisor.acquire("a", "agent-c").await.unwrap();
        assert_eq!(newcomer.slot, Some(slot_a));
        let other = supervisor.acquire("a", "agent-b").await.unwrap();
        assert_eq!(other.slot, Some(slot_b));
    }

    #[tokio::test]
    async fn waiters_get_slots_first_come_first_served_and_show_in_the_queue() {
        let supervisor = supervisor(Arc::new(FakeLauncher::default()));
        let held = vec![
            supervisor.acquire("a", "h0").await.unwrap(),
            supervisor.acquire("a", "h1").await.unwrap(),
        ];
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for name in ["w1", "w2", "w3"] {
            let supervisor = Arc::clone(&supervisor);
            let tx = tx.clone();
            tokio::spawn(async move {
                let lease = supervisor.acquire("a", name).await.unwrap();
                tx.send((name, lease)).unwrap();
            });
            // Let the waiter join the queue before the next one starts.
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_eq!(supervisor.loaded()[0].queued, 3);
        assert_eq!(supervisor.loaded()[0].slots_in_use, 2);
        let mut served = Vec::new();
        let mut holding = Vec::new();
        for lease in held {
            drop(lease);
            let (name, lease) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            served.push(name);
            holding.push(lease);
        }
        assert_eq!(served, ["w1", "w2"]);
        assert_eq!(supervisor.loaded()[0].queued, 1);
        holding.pop();
        let (name, _lease) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(name, "w3");
        assert_eq!(supervisor.loaded()[0].queued, 0);
    }

    #[tokio::test]
    async fn the_expected_queue_wait_follows_busy_slots_and_queued_calls() {
        let supervisor = supervisor(Arc::new(FakeLauncher::default()));
        // Not loaded: nothing to wait for.
        assert_eq!(LocalModels::queue_wait_s(supervisor.as_ref(), "a"), 0);
        let first = supervisor.acquire("a", "h0").await.unwrap();
        // A slot is free.
        assert_eq!(LocalModels::queue_wait_s(supervisor.as_ref(), "a"), 0);
        let second = supervisor.acquire("a", "h1").await.unwrap();
        // Both slots busy, nobody queued: this call is next, half a default call per slot.
        assert_eq!(LocalModels::queue_wait_s(supervisor.as_ref(), "a"), 15);
        for name in ["w1", "w2", "w3"] {
            let supervisor = Arc::clone(&supervisor);
            tokio::spawn(async move {
                let _lease = supervisor.acquire("a", name).await;
            });
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        // Three queued ahead: four calls over two slots.
        assert_eq!(LocalModels::queue_wait_s(supervisor.as_ref(), "a"), 60);
        drop((first, second));
    }

    #[tokio::test]
    async fn a_cancelled_wait_leaves_the_queue() {
        let supervisor = supervisor(Arc::new(FakeLauncher::default()));
        let _held = [
            supervisor.acquire("a", "h0").await.unwrap(),
            supervisor.acquire("a", "h1").await.unwrap(),
        ];
        let waiting =
            tokio::time::timeout(Duration::from_millis(50), supervisor.acquire("a", "late")).await;
        assert!(waiting.is_err(), "both slots are busy");
        assert_eq!(supervisor.loaded()[0].queued, 0);
    }

    #[tokio::test]
    async fn only_runtimes_that_address_slots_hand_out_slot_indexes() {
        let launcher = Arc::new(FakeLauncher::default());
        let mut hf = model("hf", 2);
        hf.format = Format::Safetensors;
        let supervisor = Supervisor::start(
            vec![hf, model("gguf", 2)],
            settings(),
            Runtimes {
                vllm_available: true,
                ..Runtimes::default()
            },
            &hardware(),
            launcher,
        );
        assert!(
            supervisor
                .acquire("gguf", "h")
                .await
                .unwrap()
                .slot
                .is_some()
        );
        assert_eq!(supervisor.acquire("hf", "h").await.unwrap().slot, None);
    }

    #[tokio::test]
    async fn the_prompt_cache_is_sized_from_the_ram_left_and_counted() {
        let with_ram = |ram_gib: u64| Hardware {
            ram_available_bytes: ram_gib * GIB,
            ..hardware()
        };
        let start = |ram_gib: u64, requested_mb: u64| {
            let launcher = Arc::new(FakeLauncher::default());
            let launch: Arc<dyn Launcher> = launcher.clone();
            let settings = LlamaCppSettings {
                cache_ram_mb: requested_mb,
                ..settings()
            };
            let supervisor = Supervisor::start(
                vec![model("a", 6)],
                settings,
                Runtimes::default(),
                &with_ram(ram_gib),
                launch,
            );
            (supervisor, launcher)
        };
        let cache_arg = |launcher: &FakeLauncher| {
            let launched = lock(&launcher.launched);
            let args = &launched[0].1;
            let at = args.iter().position(|arg| arg == "--cache-ram").unwrap();
            args[at + 1].clone()
        };
        // Plenty of RAM: the setting applies.
        let (roomy, launcher) = start(32, 8192);
        drop(roomy.acquire("a", "h").await.unwrap());
        assert_eq!(cache_arg(&launcher), "8192");
        assert_eq!(roomy.loaded()[0].ram_bytes, 8192 * 1024 * 1024);
        // Four GiB of RAM: capped to it.
        let (tight, launcher) = start(4, 8192);
        drop(tight.acquire("a", "h").await.unwrap());
        assert_eq!(cache_arg(&launcher), "4096");
        // The setting can turn it off.
        let (off, launcher) = start(32, 0);
        drop(off.acquire("a", "h").await.unwrap());
        assert_eq!(cache_arg(&launcher), "0");
    }

    #[tokio::test]
    async fn the_prompt_cache_leaves_room_for_idle_models_that_stay_loaded() {
        // 12 GiB of RAM; two small models fit beside each other on the 10 GiB GPU.
        let hardware = Hardware {
            ram_available_bytes: 12 * GIB,
            ..hardware()
        };
        let launcher = Arc::new(FakeLauncher::default());
        let launch: Arc<dyn Launcher> = launcher.clone();
        let settings = LlamaCppSettings {
            cache_ram_mb: 8192,
            ..settings()
        };
        let supervisor = Supervisor::start(
            vec![model("first", 2), model("second", 2)],
            settings,
            Runtimes::default(),
            &hardware,
            launch,
        );
        // The first model's cache takes 8 GiB and it goes idle, but stays loaded.
        drop(supervisor.acquire("first", "h").await.unwrap());
        let lease = supervisor.acquire("second", "h").await.unwrap();
        let loaded = supervisor.loaded();
        assert_eq!(loaded.len(), 2, "the idle model was not unloaded");
        // Only 4 GiB of the 12 are left for the second model's cache, not the 8 GiB asked for.
        let launched = lock(&launcher.launched);
        let args = &launched[1].1;
        let at = args.iter().position(|arg| arg == "--cache-ram").unwrap();
        assert_eq!(args[at + 1], "4096");
        let committed: u64 = loaded.iter().map(|model| model.ram_bytes).sum();
        assert!(committed <= 12 * GIB, "{committed} bytes of RAM committed");
        drop(lease);
    }

    #[tokio::test]
    async fn an_idle_model_gives_way_rather_than_a_newcomer_running_on_the_cpu() {
        // 32 GiB of RAM: beside idle "a", "b" would run part of its layers on the CPU, so "a"
        // is swapped out and "b" gets every layer on the GPU.
        let hardware = Hardware {
            ram_available_bytes: 32 * GIB,
            ..hardware()
        };
        let supervisor = Supervisor::start(
            vec![model("a", 4), model("b", 6)],
            settings(),
            Runtimes::default(),
            &hardware,
            Arc::new(FakeLauncher::default()),
        );
        drop(supervisor.acquire("a", "h").await.unwrap());
        let _b = supervisor.acquire("b", "h").await.unwrap();
        let loaded = supervisor.loaded();
        let vram: u64 = loaded.iter().map(|model| model.vram_bytes).sum();
        assert!(vram <= 10 * GIB, "{vram} bytes of VRAM committed");
        assert_eq!(loaded.len(), 1, "a was swapped out");
        assert_eq!(loaded[0].model_id, "b");
        assert!(loaded[0].all_layers_on_gpu);
        assert_eq!(loaded[0].ram_bytes, 0);
    }

    #[tokio::test]
    async fn the_prompt_cache_shrinks_so_two_models_share_the_ram() {
        // No GPU and 12 GiB of RAM: two 4 GiB models fit together, but not beside an 8 GiB
        // prompt cache.
        let hardware = Hardware {
            gpus: Vec::new(),
            ram_available_bytes: 12 * GIB,
            ..hardware()
        };
        let launcher = Arc::new(FakeLauncher::default());
        let settings = LlamaCppSettings {
            cache_ram_mb: 8192,
            ..settings()
        };
        let supervisor = Supervisor::start(
            vec![model("first", 3), model("second", 3)],
            settings,
            Runtimes::default(),
            &hardware,
            launcher.clone(),
        );
        drop(supervisor.acquire("first", "h").await.unwrap());
        // The first model's cache takes the RAM the second needs, so it is restarted.
        drop(supervisor.acquire("second", "h").await.unwrap());
        drop(supervisor.acquire("first", "h").await.unwrap());
        let loaded = supervisor.loaded();
        assert_eq!(
            loaded.len(),
            2,
            "both models stay loaded with smaller caches"
        );
        let committed: u64 = loaded.iter().map(|model| model.ram_bytes).sum();
        assert!(committed <= 12 * GIB, "{committed} bytes of RAM committed");
        // Settled after one restart: using either again loads nothing.
        drop(supervisor.acquire("second", "h").await.unwrap());
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn the_runtime_never_unloads_on_its_own() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = Supervisor::start(
            vec![model("a", 6)],
            LlamaCppSettings {
                idle_unload_s: 0,
                ..settings()
            },
            Runtimes::default(),
            &hardware(),
            Arc::clone(&launcher) as Arc<dyn Launcher>,
        );
        drop(supervisor.acquire("a", "holder").await.unwrap());
        // Idle past `idle_unload_s` many times over: only the residency controller may unload
        // it, and none runs here.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(supervisor.loaded().len(), 1);
        // Told to, it unloads, and says why.
        let mut moves = supervisor.moves();
        supervisor
            .execute(Action::Unload { model: "a".into() }, Reason::Idle)
            .await
            .unwrap();
        assert!(supervisor.loaded().is_empty());
        let moved = moves.try_recv().unwrap();
        assert_eq!(
            (moved.action, moved.reason, moved.ok),
            (Action::Unload { model: "a".into() }, Reason::Idle, true)
        );
    }

    /// Counts the decisions it is asked for, and decides as the arbiter does.
    #[derive(Debug, Default)]
    struct Counting {
        decisions: AtomicUsize,
    }

    impl ResidencyPolicy for Counting {
        fn decide(&self, ask: &LoadAsk<'_>) -> Decision {
            self.decisions.fetch_add(1, Ordering::SeqCst);
            Standard.decide(ask)
        }
    }

    #[tokio::test]
    async fn every_load_asks_the_installed_policy_and_reports_its_reason() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        let policy = Arc::new(Counting::default());
        supervisor.set_policy(Arc::clone(&policy) as Arc<dyn ResidencyPolicy>);
        let mut moves = supervisor.moves();
        drop(supervisor.acquire("a", "holder").await.unwrap());
        drop(supervisor.acquire("b", "holder").await.unwrap());
        assert_eq!(policy.decisions.load(Ordering::SeqCst), 2);
        let mut seen = Vec::new();
        while let Ok(moved) = moves.try_recv() {
            seen.push((
                moved.action.name(),
                moved.action.model().map(str::to_string),
                moved.reason,
            ));
        }
        assert_eq!(
            seen,
            [
                ("load", Some("a".to_string()), Reason::Call),
                // "b" does not fit beside "a" (no RAM to spill into).
                ("unload", Some("a".to_string()), Reason::Room),
                ("load", Some("b".to_string()), Reason::Call),
            ]
        );
    }

    #[tokio::test]
    async fn the_budget_is_replaced_when_told_and_reported() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        let mut moves = supervisor.moves();
        let smaller = Budget {
            vram_bytes: 4 * GIB,
            ..supervisor.budget()
        };
        assert!(supervisor.set_budget(smaller, Reason::Budget));
        assert!(!supervisor.set_budget(smaller, Reason::Budget), "no change");
        assert_eq!(supervisor.budget(), smaller);
        // The ceilings stay sized against the memory measured at start.
        assert_eq!(supervisor.start_budget().vram_bytes, 10 * GIB);
        // A 6 GiB model no longer fits the GPU; with no RAM it does not fit at all.
        let error = supervisor.acquire("a", "holder").await.unwrap_err();
        assert!(error.contains("not enough memory"), "{error}");
        let moved = moves.try_recv().unwrap();
        assert_eq!(moved.action.name(), "set_budget");
        assert_eq!(moved.reason, Reason::Budget);
    }

    /// `main` (6 GiB) with `small` (1 GiB) configured as its draft, on 10 GiB of VRAM.
    fn drafting_supervisor(
        launcher: Arc<FakeLauncher>,
        models: Vec<LocalModel>,
        draft: &str,
    ) -> Arc<Supervisor> {
        let mut settings = settings();
        settings.models.insert(
            "main".into(),
            LocalModelSettings {
                draft_model: Some(draft.into()),
                draft_max: Some(8),
                draft_min: Some(2),
                draft_gpu_layers: Some(99),
                ..LocalModelSettings::default()
            },
        );
        Supervisor::start(models, settings, Runtimes::default(), &hardware(), launcher)
    }

    fn last_args(launcher: &FakeLauncher) -> String {
        let launched = lock(&launcher.launched);
        launched
            .last()
            .map(|(_, args)| args.join(" "))
            .unwrap_or_default()
    }

    #[test]
    fn draft_flags_are_passed_only_when_a_draft_is_configured() {
        let spec = LaunchSpec {
            backend: Arc::new(LlamaCpp::default()),
            format: Format::Gguf,
            gpu_memory_utilization: 0.0,
            model_id: "m".into(),
            model_path: PathBuf::from("m.gguf"),
            mmproj: None,
            context: 4096,
            pool_tokens: 4096,
            slots: 1,
            gpu_layers: 99,
            cpu_only: false,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            flash_attn: "auto".into(),
            cache_ram_mb: 0,
            slot_save_path: None,
            draft: None,
            extra_args: vec!["--x".into()],
        };
        let plain = spec.args().join(" ");
        assert!(
            !plain.contains("draft") && !plain.contains("--spec-type"),
            "{plain}"
        );
        let drafted = LaunchSpec {
            draft: Some(DraftSpec {
                path: PathBuf::from("d.gguf"),
                max: Some(8),
                min: Some(2),
                gpu_layers: Some(0),
            }),
            ..spec.clone()
        };
        let args = drafted.args().join(" ");
        assert!(
            args.ends_with(
                "--spec-draft-model d.gguf --spec-type draft-simple --spec-draft-n-max 8 \
                 --spec-draft-n-min 2 --spec-draft-ngl 0 --x"
            ),
            "{args}"
        );
        // The environment cannot name a draft behind Imperium's back: llama.cpp's draft
        // variables are removed from every llama.cpp server, `LLAMA_ARG_SPEC_TYPE` too while
        // Imperium passes `--spec-type`.
        let drafted_env = backend::scrubbed_env(&drafted);
        assert!(
            drafted_env
                .iter()
                .any(|n| n == "LLAMA_ARG_SPEC_DRAFT_MODEL")
        );
        assert!(drafted_env.iter().any(|n| n == "LLAMA_ARG_SPEC_TYPE"));
        let plain_env = backend::scrubbed_env(&spec);
        assert!(
            plain_env
                .iter()
                .any(|n| n == "LLAMA_ARG_N_GPU_LAYERS_DRAFT")
        );
        assert!(!plain_env.iter().any(|n| n == "LLAMA_ARG_SPEC_TYPE"));
        // Any `LLAMA_ARG_SPEC_DRAFT_*` present goes, known to this build or not; others stay.
        let present = [
            "LLAMA_ARG_SPEC_DRAFT_SOMETHING_NEW",
            "LLAMA_ARG_HOST",
            "LLAMA_ARG_SPEC_NGRAM",
        ]
        .map(String::from);
        let names = backend::scrub_names(present, false);
        assert!(
            names
                .iter()
                .any(|n| n == "LLAMA_ARG_SPEC_DRAFT_SOMETHING_NEW")
        );
        assert!(
            !names
                .iter()
                .any(|n| n == "LLAMA_ARG_HOST" || n == "LLAMA_ARG_SPEC_NGRAM")
        );
        let vllm = LaunchSpec {
            backend: Arc::new(Vllm::default()),
            ..spec.clone()
        };
        assert!(backend::scrubbed_env(&vllm).is_empty());
        // Unset parameters leave llama.cpp's own defaults.
        let bare = LaunchSpec {
            draft: Some(DraftSpec {
                path: PathBuf::from("d.gguf"),
                max: None,
                min: None,
                gpu_layers: None,
            }),
            ..spec
        };
        let args = bare.args().join(" ");
        assert!(
            args.ends_with("--spec-draft-model d.gguf --spec-type draft-simple --x"),
            "{args}"
        );
    }

    #[tokio::test]
    async fn a_usable_draft_is_launched_and_counted_in_the_memory_plan() {
        let launcher = Arc::new(FakeLauncher::default());
        let models = vec![model("main", 6), model("small", 1)];
        let drafted = drafting_supervisor(Arc::clone(&launcher), models.clone(), "small");
        let plain_launcher = Arc::new(FakeLauncher::default());
        let plain = Supervisor::start(
            models,
            settings(),
            Runtimes::default(),
            &hardware(),
            plain_launcher.clone(),
        );
        let main = drafted
            .models()
            .into_iter()
            .find(|m| m.id == "main")
            .unwrap();
        // The draft's weights, its KV cache and its buffers are part of what the model needs.
        let extra =
            drafted.footprint(&main).required_bytes() - plain.footprint(&main).required_bytes();
        assert!(extra > GIB && extra < 3 * GIB, "{extra}");

        let _lease = drafted.acquire("main", "holder").await.unwrap();
        let args = last_args(&launcher);
        assert!(
            args.contains(
                "--spec-draft-model small.gguf --spec-type draft-simple --spec-draft-n-max 8 \
                 --spec-draft-n-min 2 --spec-draft-ngl 99"
            ),
            "{args}"
        );
        let status = drafted.loaded().remove(0);
        assert_eq!(status.draft.as_deref(), Some("small"));
        assert_eq!(status.draft_notice, None);
        // The residency controller sees the draft's memory as the model's.
        let _plain_lease = plain.acquire("main", "holder").await.unwrap();
        assert!(!last_args(&plain_launcher).contains("--spec-draft-model"));
        let without = plain.loaded().remove(0);
        assert_eq!(
            status.vram_bytes - without.vram_bytes,
            extra,
            "loaded memory counts the draft"
        );
    }

    #[tokio::test]
    async fn a_missing_or_incompatible_draft_loads_the_model_without_it_and_says_why() {
        for (draft, models, why) in [
            ("absent", vec![model("main", 6)], "not an installed model"),
            (
                "other",
                {
                    let mut other = model("other", 1);
                    other.info.tokenizer_model = Some("llama".into());
                    let mut main = model("main", 6);
                    main.info.tokenizer_model = Some("gpt2".into());
                    vec![main, other]
                },
                "tokenizer",
            ),
            (
                "big-draft",
                // 3 GiB with its KV cache fits nowhere beside the 6 GiB model (no RAM to spill to).
                vec![model("main", 6), model("big-draft", 3)],
                "does not fit in memory beside the model",
            ),
        ] {
            let launcher = Arc::new(FakeLauncher::default());
            let supervisor = drafting_supervisor(Arc::clone(&launcher), models, draft);
            let main = supervisor
                .models()
                .into_iter()
                .find(|m| m.id == "main")
                .unwrap();
            // The notice is there before the model loads, for the settings view.
            let (used, notice) = supervisor.draft_state(&main, &supervisor.launch("main"));
            assert_eq!(used, None, "{draft}");
            assert!(
                notice.as_deref().unwrap().contains(why),
                "{draft}: {notice:?}"
            );
            let _lease = supervisor.acquire("main", "holder").await.unwrap();
            let args = last_args(&launcher);
            assert!(
                !args.contains("--spec-draft-model") && !args.contains("--spec-type"),
                "{draft}: {args}"
            );
            let status = supervisor.loaded().remove(0);
            assert_eq!(status.draft, None);
            assert!(status.draft_notice.unwrap().contains(why), "{draft}");
        }
    }

    fn big_machine() -> Hardware {
        Hardware {
            ram_total_bytes: 64 * GIB,
            ram_available_bytes: 64 * GIB,
            ..hardware()
        }
    }

    /// Like [`drafting_supervisor`], on `hardware`, with the draft settings changed by `tweak`.
    fn drafting_on(
        hardware: &Hardware,
        launcher: Arc<FakeLauncher>,
        models: Vec<LocalModel>,
        draft_settings: LocalModelSettings,
        context_auto: bool,
    ) -> Arc<Supervisor> {
        let mut settings = settings();
        if context_auto {
            settings.context = imperium_config::ContextLimit::Auto;
        }
        settings.models.insert("main".into(), draft_settings);
        Supervisor::start(models, settings, Runtimes::default(), hardware, launcher)
    }

    fn draft_of(id: &str) -> LocalModelSettings {
        LocalModelSettings {
            draft_model: Some(id.into()),
            ..LocalModelSettings::default()
        }
    }

    fn pair() -> Vec<LocalModel> {
        vec![model("main", 6), model("small", 1)]
    }

    fn plain_supervisor(hardware: &Hardware, models: Vec<LocalModel>) -> Arc<Supervisor> {
        Supervisor::start(
            models,
            settings(),
            Runtimes::default(),
            hardware,
            Arc::new(FakeLauncher::default()),
        )
    }

    fn the_main(supervisor: &Supervisor) -> LocalModel {
        supervisor
            .models()
            .into_iter()
            .find(|m| m.id == "main")
            .unwrap()
    }

    #[tokio::test]
    async fn a_draft_the_server_refuses_twice_is_retried_without_and_then_left_out() {
        let launcher = Arc::new(FakeLauncher::default());
        launcher.refuse_drafts.store(true, Ordering::SeqCst);
        let supervisor = drafting_supervisor(Arc::clone(&launcher), pair(), "small");
        let lease = supervisor.acquire("main", "holder").await.unwrap();
        // The draft is tried twice (the first failure may be transient), then once without.
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 3);
        let args = last_args(&launcher);
        assert!(!args.contains("--spec-draft-model"), "{args}");
        let status = supervisor.loaded().remove(0);
        assert_eq!(status.draft, None);
        let notice = status.draft_notice.unwrap();
        assert!(
            notice.contains("small") && notice.contains("did not start"),
            "{notice}"
        );
        // The next load goes straight to the launch without the draft ...
        drop(lease);
        supervisor.unload("main").await.unwrap();
        let _again = supervisor.acquire("main", "holder").await.unwrap();
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 4);
        drop(_again);
        supervisor.unload("main").await.unwrap();
        // ... until the settings change, which forgets the blame; so does a rescan.
        let main = the_main(&supervisor);
        let settings = |id: &str| BTreeMap::from([("main".to_string(), draft_of(id))]);
        supervisor.set_model_settings(settings("small"));
        let launch = supervisor.launch("main");
        assert_eq!(
            supervisor.draft_state(&main, &launch).0.as_deref(),
            Some("small")
        );
        launcher.refuse_drafts.store(true, Ordering::SeqCst);
        drop(supervisor.acquire("main", "holder").await.unwrap());
        assert_eq!(supervisor.draft_state(&main, &launch).0, None);
        supervisor.unload("main").await.unwrap();
        supervisor.set_models(pair());
        assert_eq!(
            supervisor.draft_state(&main, &launch).0.as_deref(),
            Some("small")
        );
    }

    #[tokio::test]
    async fn a_transient_failure_with_a_draft_is_retried_with_the_draft() {
        let launcher = Arc::new(FakeLauncher::default());
        launcher.fail_first.store(1, Ordering::SeqCst);
        let supervisor = drafting_supervisor(Arc::clone(&launcher), pair(), "small");
        let lease = supervisor.acquire("main", "holder").await.unwrap();
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        assert!(last_args(&launcher).contains("--spec-draft-model small.gguf"));
        let status = supervisor.loaded().remove(0);
        assert_eq!(status.draft.as_deref(), Some("small"));
        assert_eq!(status.draft_notice, None);
        // The draft was not blamed: the next load uses it again.
        drop(lease);
        supervisor.unload("main").await.unwrap();
        drop(supervisor.acquire("main", "holder").await.unwrap());
        assert!(last_args(&launcher).contains("--spec-draft-model small.gguf"));
    }

    #[tokio::test]
    async fn a_model_that_fails_without_its_draft_too_does_not_blame_the_draft() {
        let launcher = Arc::new(FakeLauncher::default());
        launcher.fail_all.store(true, Ordering::SeqCst);
        let supervisor = drafting_supervisor(Arc::clone(&launcher), pair(), "small");
        let error = supervisor.acquire("main", "holder").await.unwrap_err();
        assert!(error.contains("refused"), "{error}");
        // The draft twice, then without it; all failed.
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 3);
        launcher.fail_all.store(false, Ordering::SeqCst);
        drop(supervisor.acquire("main", "holder").await.unwrap());
        assert!(last_args(&launcher).contains("--spec-draft-model small.gguf"));
        assert_eq!(supervisor.loaded()[0].draft.as_deref(), Some("small"));
    }

    #[tokio::test]
    async fn a_start_that_times_out_is_not_retried_and_does_not_blame_the_draft() {
        let launcher = Arc::new(FakeLauncher::default());
        launcher.time_out.store(true, Ordering::SeqCst);
        let supervisor = drafting_supervisor(Arc::clone(&launcher), pair(), "small");
        let error = supervisor.acquire("main", "holder").await.unwrap_err();
        assert!(error.contains("did not load"), "{error}");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
        launcher.time_out.store(false, Ordering::SeqCst);
        drop(supervisor.acquire("main", "holder").await.unwrap());
        assert!(last_args(&launcher).contains("--spec-draft-model small.gguf"));
    }

    #[tokio::test]
    async fn a_server_that_runs_without_its_draft_is_noticed_and_its_memory_released() {
        let launcher = Arc::new(FakeLauncher::default());
        launcher.no_speculation.store(true, Ordering::SeqCst);
        let drafted = drafting_supervisor(Arc::clone(&launcher), pair(), "small");
        let plain = plain_supervisor(&hardware(), pair());
        let mut moves = drafted.moves();
        let lease = drafted.acquire("main", "holder").await.unwrap();
        // One start: llama-server came up, and said it does not speculate.
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
        let status = drafted.loaded().remove(0);
        assert_eq!(status.draft, None);
        let notice = status.draft_notice.unwrap();
        assert!(notice.contains("without speculative decoding"), "{notice}");
        // What the plan reserved for the draft is given back.
        drop(plain.acquire("main", "holder").await.unwrap());
        let without = plain.loaded().remove(0);
        assert_eq!(
            (status.vram_bytes, status.ram_bytes),
            (without.vram_bytes, without.ram_bytes)
        );
        // The load's entry in the residency log says so.
        let load = moves.try_recv().unwrap();
        assert_eq!(load.action.name(), "load");
        assert!(
            load.detail
                .unwrap()
                .contains("without speculative decoding"),
            "the log says why"
        );
        // The next load leaves the draft out until the settings change.
        drop(lease);
        drafted.unload("main").await.unwrap();
        drop(drafted.acquire("main", "holder").await.unwrap());
        assert!(!last_args(&launcher).contains("--spec-draft-model"));
    }

    #[tokio::test]
    async fn a_running_draft_is_named_in_the_residency_log() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = drafting_supervisor(Arc::clone(&launcher), pair(), "small");
        let mut moves = supervisor.moves();
        drop(supervisor.acquire("main", "holder").await.unwrap());
        let load = moves.try_recv().unwrap();
        assert!(load.detail.unwrap().contains("draft model small"));
    }

    #[test]
    fn where_the_draft_sits_follows_its_gpu_layers_and_the_cpu_lane() {
        let hardware = big_machine();
        let plan = |settings: LocalModelSettings| {
            let supervisor = drafting_on(
                &hardware,
                Arc::new(FakeLauncher::default()),
                pair(),
                settings,
                false,
            );
            let main = the_main(&supervisor);
            let footprint = supervisor.footprint(&main);
            let (_, placement) = supervisor.plan_with(&main, &supervisor.launch("main"));
            (footprint, placement.unwrap(), supervisor)
        };
        // Unset layers: llama.cpp's auto, the whole draft on the GPU.
        let (footprint, placement, _) = plan(draft_of("small"));
        assert!(footprint.draft.bytes > GIB);
        assert_eq!(footprint.draft.ram_bytes, 0);
        assert!(placement.all_layers_on_gpu);
        let on_gpu = placement.vram_bytes;
        // 0 layers: the draft is in RAM and the GPU holds only the model.
        let (footprint, placement, _) = plan(LocalModelSettings {
            draft_gpu_layers: Some(0),
            ..draft_of("small")
        });
        assert_eq!(footprint.draft.ram_bytes, footprint.draft.bytes);
        assert_eq!(placement.ram_bytes, footprint.draft.bytes);
        assert_eq!(placement.vram_bytes, on_gpu - footprint.draft.bytes);
        // 8 of the draft's 32 layers: a quarter on the GPU, the rest of its weights and KV in RAM.
        let (footprint, placement, _) = plan(LocalModelSettings {
            draft_gpu_layers: Some(8),
            ..draft_of("small")
        });
        assert!(footprint.draft.ram_bytes > 0 && footprint.draft.ram_bytes < footprint.draft.bytes);
        assert_eq!(placement.ram_bytes, footprint.draft.ram_bytes);
        // On the CPU lane the draft follows the model into RAM.
        let (_, _, supervisor) = plan(draft_of("small"));
        supervisor.set_cpu_pinned(HashMap::from([("main".to_string(), CpuLane::default())]));
        let main = the_main(&supervisor);
        let footprint = supervisor.footprint(&main);
        assert_eq!(footprint.draft.ram_bytes, footprint.draft.bytes);
        let (_, placement) = supervisor.plan_with(&main, &supervisor.launch("main"));
        let placement = placement.unwrap();
        assert_eq!((placement.gpu_layers, placement.vram_bytes), (0, 0));
        assert!(placement.ram_bytes >= footprint.draft.bytes);
    }

    #[test]
    fn the_context_ceiling_leaves_room_for_the_draft() {
        let drafted = drafting_on(
            &hardware(),
            Arc::new(FakeLauncher::default()),
            pair(),
            draft_of("small"),
            true,
        );
        let mut settings = settings();
        settings.context = imperium_config::ContextLimit::Auto;
        let plain = Supervisor::start(
            pair(),
            settings,
            Runtimes::default(),
            &hardware(),
            Arc::new(FakeLauncher::default()),
        );
        let (with, without) = (drafted.launch("main"), plain.launch("main"));
        assert!(with.context < without.context, "{with:?} {without:?}");
        assert!(with.context > 0);
    }

    #[test]
    fn a_draft_that_would_push_the_models_layers_off_the_gpu_is_skipped() {
        // 8 GiB of weights and a 1 GiB pool fit the 10 GiB GPU alone; a draft beside them does
        // not, and there is RAM to spill to, so it would be the model's layers that move.
        let supervisor = drafting_on(
            &big_machine(),
            Arc::new(FakeLauncher::default()),
            vec![model("main", 8), model("small", 1)],
            draft_of("small"),
            false,
        );
        let main = the_main(&supervisor);
        let (used, notice) = supervisor.draft_state(&main, &supervisor.launch("main"));
        assert_eq!(used, None);
        assert!(
            notice.unwrap().contains("no longer fit on the GPU"),
            "the reason names the layers"
        );
        // Without the draft the model is wholly on the GPU.
        let (_, placement) = supervisor.plan_with(&main, &supervisor.launch("main"));
        assert!(placement.unwrap().all_layers_on_gpu);
    }
}
