//! Loads local models into supervised runtime servers, hands out slots on them, and swaps them
//! in and out of memory.
//!
//! Each model is served by the first available [`RuntimeBackend`] that accepts its format
//! (`llama-server` for GGUF, `vllm serve` for Hugging Face folders by default).
//! Each loaded model is one server started with `--parallel <slots>`. An agent takes a slot (a
//! semaphore permit and a slot index) for one model call and releases it when the call ends, so
//! agents that are running tools or waiting hold none; callers queue first-come, first-served
//! while every slot is busy, and an agent gets back the slot it used last when that slot is
//! free.
//!
//! The supervisor is the one memory arbiter for every session. Memory is reserved per loaded
//! model against the budget measured when it started. A model that does not fit beside the
//! loaded ones swaps out models nobody has a slot on ([`crate::arbiter`] decides which, and when
//! to wait instead), saving to disk the slots of their agents that can still come back; an
//! agent whose slot was saved gets it restored into its model before its next call. Calls for a
//! model that is not loaded wait in arrival order, and a loaded model takes on new agents only
//! while no earlier call for another model is waiting, so work is batched by model and nothing
//! waits longer than the batch ahead of it. Loads and unloads run one at a time, swaps are
//! limited per minute (past the limit a call that needs one waits for the minute), no call waits
//! longer than `max_wait_s`, and idle models are unloaded after `idle_unload_s`.
//!
//! A model can be pinned to the CPU lane ([`Supervisor::set_cpu_pinned`]): it is served with no
//! GPU layers, its weights, KV cache and prompt cache count against the RAM budget alone, and it
//! is never a victim of a swap made for VRAM (see [`crate::arbiter`]); it serves one call at a
//! time unless configured otherwise. The speed each model has been measured generating at is
//! kept for the listing and for routing ([`LocalModels::tokens_per_s`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use imperium_api::{
    AcquireFuture, Availability, Load as MachineLoad, LoadView, LocalLease, LocalModels,
    LocalPresence, Priority,
};
use imperium_config::{LlamaCppSettings, VllmSettings};
use tokio::sync::{Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore, broadcast};

use crate::arbiter::{self, Decision, Outlook, Policy, Resident, Swaps, Use};
use crate::backend::{self, RuntimeBackend};
use crate::hardware::Hardware;
use crate::planner::{Budget, Footprint, Placement, prompt_cache_mb};
use crate::scan::{Format, LocalModel};
use crate::slot_files::{self, Saved, SlotFiles};

const MIB: u64 = 1024 * 1024;
/// How often idle models are checked for unloading.
const IDLE_CHECK: Duration = Duration::from_secs(15);
/// How often a call waiting for room looks again when nothing has changed, for waits that end
/// with time: another model's residency, the swap limit's window.
const RECHECK: Duration = Duration::from_millis(250);
/// How long after an interactive call last used a model background work may not swap it out,
/// unless [`Supervisor::set_background_policy`] says otherwise.
const DEFAULT_INTERACTIVE_HOLD: Duration = Duration::from_secs(5 * 60);
/// The window `max_swaps_per_min` counts over.
const SWAP_WINDOW: Duration = Duration::from_secs(60);
/// Load speed assumed for a model that has not been loaded yet, to estimate a swap.
const ASSUMED_LOAD_BYTES_PER_S: u64 = 2 * 1024 * MIB;
/// Slots of a model pinned to the CPU lane unless its row sets `parallel`: the cores are slow
/// and shared, so calls take turns.
pub const CPU_LANE_SLOTS: u32 = 1;
/// Longest a call waits for a slot on a CPU-lane model unless its row sets `max_wait_s` (or the
/// global `max_wait_s` is longer): calls queue behind tasks that take minutes.
pub const CPU_LANE_MAX_WAIT_S: u64 = 3600;

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
/// How much of the earlier calls' tokens and time a new call's measurement keeps: the speed
/// follows the model's recent calls, and long generations weigh more than short ones.
const THROUGHPUT_KEEP: f64 = 0.8;

/// What a server is started with.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// The runtime that serves the model.
    pub backend: Arc<dyn RuntimeBackend>,
    pub format: Format,
    pub model_id: String,
    pub model_path: PathBuf,
    pub mmproj: Option<PathBuf>,
    /// Total context (`-c`): the per-slot context times the slots.
    pub context: u32,
    pub slots: u32,
    pub gpu_layers: u32,
    /// The model is served on the CPU alone: no GPU device is used at all, not even for
    /// compute buffers or offloaded operations (llama.cpp's `--device none`).
    pub cpu_only: bool,
    pub cache_type: String,
    /// MiB of host memory for llama.cpp's prompt cache (`--cache-ram`), which keeps the prompts
    /// of slots that were reused by another agent so they need not be read again; 0 disables it.
    pub cache_ram_mb: u64,
    /// The folder slots are saved to before the model is swapped out (`--slot-save-path`);
    /// `None` when they are not saved.
    pub slot_save_path: Option<PathBuf>,
    /// vLLM's `--gpu-memory-utilization` (the share of GPU memory it may take).
    pub gpu_memory_utilization: f64,
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
            && self.slots == other.slots
            && self.gpu_layers == other.gpu_layers
            && self.cpu_only == other.cpu_only
            && self.cache_type == other.cache_type
            && self.cache_ram_mb == other.cache_ram_mb
            && self.slot_save_path == other.slot_save_path
            && self.gpu_memory_utilization == other.gpu_memory_utilization
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

/// A running model server; dropping it stops the server.
pub trait Server: Send + Sync {
    /// OpenAI-compatible base URL, ending in `/v1`.
    fn base_url(&self) -> &str;

    /// Saves `slot`'s cached prompt to `file` in the server's [`LaunchSpec::slot_save_path`].
    fn save_slot<'a>(&'a self, _slot: u32, _file: &'a str) -> SlotFuture<'a> {
        Box::pin(async { Err("this runtime cannot save slots".to_string()) })
    }

    /// Loads `file` from the server's [`LaunchSpec::slot_save_path`] into `slot`.
    fn restore_slot<'a>(&'a self, _slot: u32, _file: &'a str) -> SlotFuture<'a> {
        Box::pin(async { Err("this runtime cannot restore slots".to_string()) })
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
    pub base_url: String,
    pub slots: u32,
    pub slots_in_use: u32,
    /// Calls waiting for a slot.
    pub queued: u32,
    pub gpu_layers: u32,
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    /// Pinned to the CPU lane.
    pub cpu_pinned: bool,
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
    /// Out: `room` or `idle`. In: `swap` (it replaced a model) or `restore` (it came back to
    /// saved slots).
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
    fn new(base_url: String, slots: u32, pins_slots: bool, changed: Arc<Notify>) -> Arc<Self> {
        Arc::new(SlotPool {
            base_url,
            slots,
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

    /// Wraps a permit in a lease on the slot `holder` should get.
    fn lease(
        self: &Arc<Self>,
        permit: OwnedSemaphorePermit,
        holder: &str,
        priority: Priority,
    ) -> Result<LocalLease, String> {
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
            Slot {
                pool: Arc::clone(self),
                index,
                holder: holder.to_string(),
                interactive,
                _permit: permit,
            },
        ))
    }

    /// Whether one of the user's own calls held a slot within `within`, or holds one now.
    fn used_interactively(&self, within: Duration) -> bool {
        self.held_interactive.load(Ordering::SeqCst) > 0
            || lock(&self.last_interactive).is_some_and(|at| at.elapsed() < within)
    }
}

/// Counts a call as queued for a slot while it waits, also when the wait is cancelled. A model
/// with a call queued is not swapped out.
struct Queued(Arc<SlotPool>);

impl Queued {
    /// Only the user's own calls wait in the queue; background calls wait for the queue to empty.
    fn new(pool: Arc<SlotPool>) -> Self {
        pool.waiting.fetch_add(1, Ordering::SeqCst);
        pool.waiting_interactive.fetch_add(1, Ordering::SeqCst);
        Queued(pool)
    }
}

impl Drop for Queued {
    fn drop(&mut self) {
        self.0.waiting.fetch_sub(1, Ordering::SeqCst);
        self.0.waiting_interactive.fetch_sub(1, Ordering::SeqCst);
        self.0.changed.notify_waiters();
    }
}

struct Loaded {
    /// Held so the server keeps running; dropping the last reference stops it.
    server: Arc<dyn Server>,
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
}

impl Loaded {
    fn in_use(&self) -> u32 {
        self.pool.in_use()
    }

    /// A slot is in use or a call is queued for one.
    fn busy(&self) -> bool {
        self.in_use() > 0 || self.pool.waiting.load(Ordering::SeqCst) > 0
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
}

/// Tokens generated over the seconds spent generating them, over a model's recent calls.
#[derive(Debug, Clone, Copy, Default)]
struct Throughput {
    tokens: f64,
    seconds: f64,
}

impl Throughput {
    fn add(&mut self, tokens: f64, seconds: f64) {
        self.tokens = self.tokens * THROUGHPUT_KEEP + tokens;
        self.seconds = self.seconds * THROUGHPUT_KEEP + seconds;
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

    /// Whether an agent of `batch` is mid-turn on `model`: working on it, not waiting.
    fn claims(users: &HashMap<String, User>, model: &str, batch: &HashSet<String>) -> bool {
        batch.iter().any(|holder| {
            users
                .get(holder)
                .is_some_and(|user| user.model == model && user.working > 0 && user.waiting == 0)
        })
    }
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
    /// A background call gives way to the user's calls, or to a saturated machine; waiting
    /// this way does not count against `max_wait_s`.
    Yield,
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
    Load(Load),
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
    /// The context per slot and the slots it is started with.
    context: u32,
    slots: u32,
    /// It is pinned to the CPU lane.
    cpu_pinned: bool,
}

/// Supervises local model servers. Cheap to share: hand out `Arc<Supervisor>`.
pub struct Supervisor {
    /// Replaced when the folders are scanned again, such as after a download.
    models: Mutex<Vec<LocalModel>>,
    settings: LlamaCppSettings,
    /// In order of preference; a model is served by the first available one accepting it.
    backends: Vec<Arc<dyn RuntimeBackend>>,
    launcher: Arc<dyn Launcher>,
    /// Memory free when the supervisor started, less the RAM headroom.
    budget: Budget,
    state: Mutex<State>,
    presence: Arc<Presence>,
    /// Raised whenever a slot, a call, an agent's presence, or the loaded models change.
    changed: Arc<Notify>,
    /// Loads and unloads happen one at a time: they contend for the disk and the PCIe bus, and
    /// two agents asking for the same model load it once. A swap-out holds it until its
    /// servers have exited, even when the call that started it was cancelled.
    swapping: Arc<tokio::sync::Mutex<()>>,
    /// Saved slots; `None` when slots are not saved.
    slot_files: Option<Mutex<SlotFiles>>,
    /// Models pinned to the CPU lane, with their slots.
    cpu_pinned: Mutex<HashMap<String, CpuLane>>,
    events: broadcast::Sender<SwapEvent>,
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
    /// The user's own calls waiting for a slot.
    pub interactive_queued: u32,
    /// Slots held by background calls.
    pub background_in_use: u32,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("models", &lock(&self.models).len())
            .field("budget", &self.budget)
            .field("backends", &self.backends)
            .field("launcher", &self.launcher)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The state stays consistent across a panic: every update is a single insert or remove.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Supervisor {
    /// Creates a supervisor with the built-in runtimes ([`backend::builtin`]) and starts its
    /// idle-unload task on the current Tokio runtime.
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

    /// Creates a supervisor serving models with `backends`, in order of preference, and starts
    /// its idle-unload task on the current Tokio runtime. Slots are saved to the launcher's
    /// [`Launcher::slot_dir`] unless `swap_cache_mb` is 0.
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
        let supervisor = Arc::new_cyclic(|me| Supervisor {
            models: Mutex::new(models),
            settings,
            backends,
            launcher,
            budget,
            state: Mutex::new(State::default()),
            presence: Arc::default(),
            changed: Arc::new(Notify::new()),
            swapping: Arc::default(),
            slot_files,
            cpu_pinned: Mutex::default(),
            events: broadcast::channel(64).0,
            background: Mutex::new(BackgroundPolicy {
                view: None,
                hold: DEFAULT_INTERACTIVE_HOLD,
            }),
            me: Weak::clone(me),
        });
        let weak = Arc::downgrade(&supervisor);
        tokio::spawn(unload_idle(weak));
        supervisor
    }

    pub fn models(&self) -> Vec<LocalModel> {
        lock(&self.models).clone()
    }

    /// Replaces the models after a new scan. Loaded models keep running; one that is gone
    /// from disk can no longer be loaded again.
    pub fn set_models(&self, models: Vec<LocalModel>) {
        *lock(&self.models) = models;
    }

    pub fn budget(&self) -> Budget {
        self.budget
    }

    pub fn settings(&self) -> &LlamaCppSettings {
        &self.settings
    }

    /// Models swapped out and in, as they happen.
    pub fn swap_events(&self) -> broadcast::Receiver<SwapEvent> {
        self.events.subscribe()
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
    /// with.
    pub fn set_cpu_pinned(&self, pins: HashMap<String, CpuLane>) {
        *lock(&self.cpu_pinned) = pins;
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

    /// The context per slot and the slots `id` is started with: a CPU-lane model's own, else the
    /// settings'.
    fn shape(&self, id: &str) -> (u32, u32) {
        let lane = self.cpu_lane(id);
        (
            lane.and_then(|lane| lane.context)
                .unwrap_or(self.settings.context),
            lane.map_or(self.settings.parallel, |lane| lane.slots),
        )
    }

    /// What loading `model` needs at the context and slots it is started with.
    pub fn footprint(&self, model: &LocalModel) -> Footprint {
        let (context, slots) = self.shape(&model.id);
        Footprint::of(model, context, slots, &self.settings.cache_type)
    }

    /// Where `model` goes within a budget when served by `backend`: on the CPU lane if it is
    /// pinned there, else wherever the runtime places it.
    fn placer(
        &self,
        model: &LocalModel,
        backend: Arc<dyn RuntimeBackend>,
    ) -> impl Fn(Budget) -> Option<Placement> {
        let footprint = self.footprint(model);
        let cpu = self.cpu_slots(&model.id).is_some();
        move |budget| {
            if cpu {
                backend.place_on_cpu(footprint, budget)
            } else {
                backend.place(footprint, budget)
            }
        }
    }

    /// Where `model` would go on an empty machine, for listings.
    pub fn placement_when_idle(&self, model: &LocalModel) -> Option<Placement> {
        let backend = Arc::clone(self.backend(model.format)?);
        self.placer(model, backend)(self.budget)
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
        let mut loaded: Vec<_> = lock(&self.state)
            .loaded
            .iter()
            .map(|(id, loaded)| LoadedStatus {
                model_id: id.clone(),
                base_url: loaded.pool.base_url.clone(),
                slots: loaded.pool.slots,
                slots_in_use: loaded.in_use(),
                queued: loaded.pool.waiting.load(Ordering::SeqCst),
                gpu_layers: loaded.placement.gpu_layers,
                vram_bytes: loaded.placement.vram_bytes,
                ram_bytes: loaded.placement.ram_bytes,
                cpu_pinned: loaded.cpu_pinned,
            })
            .collect();
        loaded.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        loaded
    }

    fn model(&self, id: &str) -> Option<LocalModel> {
        lock(&self.models)
            .iter()
            .find(|model| model.id == id)
            .cloned()
    }

    /// The loaded models as the arbiter sees them. With `protect`, a model the user's own calls
    /// used within that long counts as in use, so it is not swapped out (and whether any model
    /// was protected is returned).
    fn residents(&self, state: &State, protect: Option<Duration>) -> (Vec<Resident>, bool) {
        let users = lock(&self.presence.users);
        let mut protected = false;
        let residents = state
            .loaded
            .iter()
            .map(|(id, loaded)| Resident {
                id: id.clone(),
                vram_bytes: loaded.placement.vram_bytes,
                ram_bytes: loaded.placement.ram_bytes,
                cache_bytes: loaded.cache_bytes,
                cpu_pinned: loaded.cpu_pinned,
                use_: if loaded.busy() {
                    Use::Busy
                } else if protect.is_some_and(|within| loaded.pool.used_interactively(within)) {
                    protected = true;
                    Use::Busy
                } else if Presence::claims(&users, id, &loaded.batch) {
                    Use::Claimed
                } else {
                    Use::Free
                },
                resident_for: loaded.loaded_at.elapsed(),
                idle_for: lock(&loaded.pool.last_used).elapsed(),
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

    async fn acquire_slot(
        &self,
        id: &str,
        holder: &str,
        priority: Priority,
    ) -> Result<LocalLease, String> {
        let model = self
            .model(id)
            .ok_or_else(|| format!("no local model {id:?} in models.folders"))?;
        let ticket = self.ticket(id, priority);
        let lane = self.cpu_lane(id);
        let max_wait = Duration::from_secs(match lane {
            Some(lane) => lane
                .max_wait_s
                .unwrap_or(CPU_LANE_MAX_WAIT_S.max(self.settings.max_wait_s)),
            None => self.settings.max_wait_s,
        });
        let mut deadline = tokio::time::Instant::now() + max_wait;
        let setting = if lane.is_some() {
            "max_wait_s of its model-table row, or runtime.llamacpp.max_wait_s"
        } else {
            "runtime.llamacpp.max_wait_s"
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
        loop {
            // Listen before looking, so a change between the two is not missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            // A background call does nothing while the machine is saturated.
            let step = if priority == Priority::Background && self.saturated() {
                Step::Yield
            } else {
                self.step(id, holder, priority, &ticket)?
            };
            match step {
                Step::Leased(lease, restore) => {
                    let lease = lease.with_wait(waited);
                    return Ok(self.restore(holder, lease, restore).await);
                }
                Step::Queued(queued) => {
                    let pool = Arc::clone(&queued.0);
                    let queued_at = Instant::now();
                    let permit = tokio::time::timeout_at(
                        deadline,
                        Arc::clone(&pool.permits).acquire_owned(),
                    )
                    .await
                    .map_err(|_| too_long())?
                    .map_err(|_| "the model was unloaded".to_string())?;
                    drop(queued);
                    waited += queued_at.elapsed();
                    if let Some((lease, restore)) =
                        self.lease_if_current(id, &pool, permit, holder, priority, ticket.seq)?
                    {
                        let lease = lease.with_wait(waited);
                        return Ok(self.restore(holder, lease, restore).await);
                    }
                    // Unloaded meanwhile: wait for it again, keeping the place in line.
                    continue;
                }
                Step::Load => {
                    let swapping = Arc::clone(&self.swapping).lock_owned();
                    let swapping = tokio::time::timeout_at(deadline, swapping)
                        .await
                        .map_err(|_| too_long())?;
                    match self.plan(&model, priority, &ticket)? {
                        Plan::Retry => continue,
                        Plan::Load(load) => {
                            return self
                                .swap_in(&model, holder, load, swapping)
                                .await
                                .map(|lease| lease.with_wait(waited));
                        }
                        Plan::Wait { yielded } => {
                            drop(swapping);
                            if yielded {
                                deadline = tokio::time::Instant::now() + max_wait;
                            }
                        }
                    }
                }
                Step::Wait => {}
                // Giving way to the user is not a failure to get room: the wait starts over.
                Step::Yield => deadline = tokio::time::Instant::now() + max_wait,
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(too_long());
            }
            let asleep = Instant::now();
            let _ = tokio::time::timeout(RECHECK.min(deadline - now), changed.as_mut()).await;
            waited += asleep.elapsed();
        }
    }

    /// Takes a slot on `id` if it is loaded and takes this call, queues for one, or says
    /// whether the call may load the model or must wait its turn.
    fn step(
        &self,
        id: &str,
        holder: &str,
        priority: Priority,
        ticket: &Ticket<'_>,
    ) -> Result<Step, String> {
        let mut state = lock(&self.state);
        let behind = state.behind_another_model(ticket.seq, id, priority);
        if !state.loaded.contains_key(id) {
            state.set_admitted(ticket.seq, false);
            return Ok(if behind { Step::Wait } else { Step::Load });
        }
        let Some(entry) = state.loaded.get_mut(id) else {
            return Ok(Step::Wait);
        };
        // An agent already in the model's batch carries on; a newcomer joins only while no
        // earlier call for another model is waiting for the room.
        if behind && !entry.batch.contains(holder) {
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
        // The permit is taken (or the call queued) under the lock, so the model cannot be
        // swapped out between finding it and using it.
        let step = match Arc::clone(&pool.permits).try_acquire_owned() {
            Ok(permit) => Step::Leased(pool.lease(permit, holder, priority)?, restore),
            Err(_) if priority == Priority::Interactive => Step::Queued(Queued::new(pool)),
            Err(_) => {
                state.set_slot_wait(ticket.seq);
                return Ok(Step::Yield);
            }
        };
        state.set_admitted(ticket.seq, true);
        Ok(step)
    }

    /// A lease from a permit granted after queueing, if the model is still the one queued for.
    fn lease_if_current(
        &self,
        id: &str,
        pool: &Arc<SlotPool>,
        permit: OwnedSemaphorePermit,
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
        Ok(Some((pool.lease(permit, holder, priority)?, restore)))
    }

    /// Decides, with loads held for this call, whether to load `model` now and what to swap
    /// out for it.
    fn plan(
        &self,
        model: &LocalModel,
        priority: Priority,
        ticket: &Ticket<'_>,
    ) -> Result<Plan, String> {
        let footprint = self.footprint(model);
        let no_room = || {
            format!(
                "not enough memory for {} (needs {} MiB)",
                model.id,
                footprint.required_bytes() / MIB
            )
        };
        let backend = Arc::clone(self.backend(model.format).ok_or_else(no_room)?);
        let mut state = lock(&self.state);
        if state.loaded.contains_key(&model.id)
            || state.behind_another_model(ticket.seq, &model.id, priority)
        {
            return Ok(Plan::Retry);
        }
        // A background call lets the user's calls load their models first, and does not
        // swap out a model the user used lately.
        let protect = (priority == Priority::Background).then(|| lock(&self.background).hold);
        if protect.is_some() && state.interactive_load_pending() {
            return Ok(Plan::Wait { yielded: true });
        }
        let (residents, protected) = self.residents(&state, protect);
        let policy = self.policy(&mut state);
        let decision = arbiter::decide(
            self.placer(model, Arc::clone(&backend)),
            self.budget,
            &residents,
            ticket.since.elapsed(),
            policy,
        );
        let (evict, placement) = match decision {
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
                    let (unprotected, _) = self.residents(&state, None);
                    !matches!(
                        arbiter::decide(
                            self.placer(model, Arc::clone(&backend)),
                            self.budget,
                            &unprotected,
                            ticket.since.elapsed(),
                            policy,
                        ),
                        Decision::Wait
                    )
                };
                return Ok(Plan::Wait { yielded });
            }
            Decision::Load { evict, placement } => (evict, placement),
        };
        let cache_room = arbiter::cache_room(self.budget, &residents, &evict, &placement);
        // Out of the loaded set at once, so nobody takes a slot on them while they are saved.
        let victims: Vec<_> = evict
            .iter()
            .filter_map(|id| state.loaded.remove_entry(id))
            .collect();
        if !victims.is_empty() {
            state.swaps.push_back(tokio::time::Instant::now());
        }
        Ok(Plan::Load(Load {
            backend,
            placement,
            cache_room,
            victims,
            call: ticket.seq,
            priority,
            context: self.shape(&model.id).0,
            slots: self.shape(&model.id).1.max(1),
            cpu_pinned: self.cpu_lane(&model.id).is_some(),
        }))
    }

    /// Swaps out the plan's victims, loads `model`, and gives `holder` its first slot.
    async fn swap_in(
        &self,
        model: &LocalModel,
        holder: &str,
        load: Load,
        swapping: OwnedMutexGuard<()>,
    ) -> Result<LocalLease, String> {
        let Load {
            backend,
            mut placement,
            cache_room,
            victims,
            call,
            priority,
            context,
            slots,
            cpu_pinned,
        } = load;
        let replaced = !victims.is_empty();
        let swapping = self.swap_out_detached(victims, "room", swapping).await?;
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
        let key = files.map(|_| slot_files::model_key(model, context, &self.settings.cache_type));
        let spec = LaunchSpec {
            backend: Arc::clone(&backend),
            format: model.format,
            gpu_memory_utilization: backend.gpu_memory_utilization(&placement, self.budget),
            model_id: model.id.clone(),
            model_path: model.path.clone(),
            mmproj: model.mmproj.clone(),
            context: context * slots,
            slots,
            gpu_layers: placement.gpu_layers,
            cpu_only: cpu_pinned,
            cache_type: self.settings.cache_type.clone(),
            cache_ram_mb,
            slot_save_path: files.map(|files| lock(files).dir().to_path_buf()),
            extra_args: backend.extra_args(),
        };
        tracing::info!(model = %model.id, gpu_layers = placement.gpu_layers, cpu_pinned, slots, cache_ram_mb, "loading local model");
        let started = Instant::now();
        // A quick failure, such as the process exiting while it starts, is tried once more; one
        // that ran out the launcher's time is not.
        let launched = match self.launcher.launch(&spec).await {
            Err(error) if !error.timed_out => {
                tracing::warn!(model = %model.id, %error, "could not start the model; trying once more");
                self.launcher.launch(&spec).await
            }
            launched => launched,
        };
        let server: Arc<dyn Server> = match launched {
            Ok(server) => Arc::from(server),
            Err(error) => {
                drop(swapping);
                self.changed.notify_waiters();
                return Err(error.message);
            }
        };
        let seconds = started.elapsed().as_secs_f64();
        let pool = SlotPool::new(
            server.base_url().to_string(),
            slots,
            backend.slot_aware(),
            Arc::clone(&self.changed),
        );
        let restorable = key.map_or(0, |key| self.reserve_saved_slots(&pool, key));
        let permit = Arc::clone(&pool.permits)
            .try_acquire_owned()
            .map_err(|_| "a new server has no free slot".to_string())?;
        let lease = pool.lease(permit, holder, priority)?;
        {
            let mut state = lock(&self.state);
            state.load_seconds.insert(model.id.clone(), seconds);
            state.set_admitted(call, true);
            state.loaded.insert(
                model.id.clone(),
                Loaded {
                    server: Arc::clone(&server),
                    pool,
                    placement,
                    cache_bytes,
                    cpu_pinned,
                    key,
                    loaded_at: Instant::now(),
                    batch: HashSet::from([holder.to_string()]),
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
        Ok(self.restore(holder, lease, restore).await)
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
        reason: &'static str,
        swapping: OwnedMutexGuard<()>,
    ) -> Result<OwnedMutexGuard<()>, String> {
        if victims.is_empty() {
            return Ok(swapping);
        }
        // Whoever calls holds an `Arc` to it, so it is always there.
        let Some(supervisor) = self.me.upgrade() else {
            self.swap_out(victims, reason).await;
            return Ok(swapping);
        };
        tokio::spawn(async move {
            supervisor.swap_out(victims, reason).await;
            swapping
        })
        .await
        .map_err(|error| format!("could not swap out a model: {error}"))
    }

    /// Saves the occupied slots of models taken out of the loaded set, then stops them.
    async fn swap_out(&self, victims: Vec<(String, Loaded)>, reason: &'static str) {
        if victims.is_empty() {
            return;
        }
        for (id, loaded) in victims {
            let started = Instant::now();
            let (bytes, slots) = self.save_slots(&loaded).await;
            // Wait for the process to exit, so the next model finds its memory free.
            loaded.server.stop().await;
            drop(loaded);
            let seconds = started.elapsed().as_secs_f64();
            tracing::info!(model = %id, reason, slots, bytes, seconds, "swapped out local model");
            self.emit(SwapEvent {
                direction: SwapDirection::Out,
                model_id: id,
                seconds,
                bytes,
                slots,
                reason,
            });
        }
        self.changed.notify_waiters();
    }

    /// Saves each slot holding the prompt of a holder that can still come back (one mid-turn or
    /// waiting for subagents, not one whose turn is over); returns the bytes written and slots
    /// saved.
    async fn save_slots(&self, loaded: &Loaded) -> (u64, u32) {
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
            match loaded.server.save_slot(slot, &file).await {
                Ok(saved) => {
                    bytes += saved.bytes;
                    slots += 1;
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
                }
            }
        }
        (bytes, slots)
    }

    /// Restores `holder`'s saved slot, if it has one for this server's model, into the slot it
    /// was just leased, before the lease is used.
    async fn restore(&self, holder: &str, lease: LocalLease, restore: Restore) -> LocalLease {
        let (Some(slot), Some(key), Some(files)) = (lease.slot, restore.key, &self.slot_files)
        else {
            return lease;
        };
        let Some(saved) = lock(files).find(holder, key) else {
            return lease;
        };
        // It stays recorded while it is read, and is forgotten with its file once read, failed,
        // or cancelled, so no file is left on disk outside the index.
        let _forget = Forget { files, holder, key };
        match restore.server.restore_slot(slot, &saved.file).await {
            Ok(restored) => {
                tracing::info!(%holder, slot, tokens = restored.tokens, bytes = restored.bytes, "restored a saved slot");
            }
            Err(error) => {
                tracing::warn!(%holder, slot, %error, "could not restore a saved slot; its prompt will be read again");
            }
        }
        lease
    }

    fn emit(&self, event: SwapEvent) {
        // No receivers just means nobody is listening.
        let _ = self.events.send(event);
    }

    /// Unloads models nobody has used for `idle_for`, saving their slots.
    async fn unload_idle_now(&self, idle_for: Duration) {
        // A swap in progress has the models' memory in hand; look again next time.
        let Ok(swapping) = Arc::clone(&self.swapping).try_lock_owned() else {
            return;
        };
        let victims: Vec<_> = {
            let mut state = lock(&self.state);
            let idle: Vec<String> = state
                .loaded
                .iter()
                .filter(|(_, entry)| {
                    !entry.busy() && lock(&entry.pool.last_used).elapsed() >= idle_for
                })
                .map(|(id, _)| id.clone())
                .collect();
            idle.iter()
                .filter_map(|id| state.loaded.remove_entry(id))
                .collect()
        };
        if let Err(error) = self.swap_out_detached(victims, "idle", swapping).await {
            tracing::warn!(%error, "could not unload idle models");
        }
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

async fn unload_idle(supervisor: Weak<Supervisor>) {
    let mut ticks = tokio::time::interval(IDLE_CHECK);
    loop {
        ticks.tick().await;
        let Some(supervisor) = supervisor.upgrade() else {
            return;
        };
        let idle_for = Duration::from_secs(supervisor.settings.idle_unload_s);
        supervisor.unload_idle_now(idle_for).await;
    }
}

impl LocalModels for Supervisor {
    fn availability(&self, id: &str) -> Availability {
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
        let (residents, _) = self.residents(&state, None);
        let policy = self.policy(&mut state);
        let outlook = arbiter::outlook(place, self.budget, &residents, policy);
        match outlook {
            Outlook::Fits => Availability::Fits,
            Outlook::Swap => Availability::Swap {
                seconds: self.swap_seconds(&state, &model),
            },
            Outlook::Unavailable => Availability::Unavailable,
        }
    }

    fn acquire<'a>(&'a self, id: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        Box::pin(self.acquire_slot(id, holder, Priority::Interactive))
    }

    fn acquire_as<'a>(
        &'a self,
        id: &'a str,
        holder: &'a str,
        priority: Priority,
    ) -> AcquireFuture<'a> {
        Box::pin(self.acquire_slot(id, holder, priority))
    }

    fn attach(&self, model: &str, holder: &str) -> LocalPresence {
        self.presence
            .enter(&self.changed, model, holder, Mark::Working)
    }

    fn waiting(&self, model: &str, holder: &str) -> LocalPresence {
        self.presence
            .enter(&self.changed, model, holder, Mark::Waiting)
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
    }

    struct FakeServer {
        url: String,
        running: Arc<AtomicUsize>,
    }

    impl Server for FakeServer {
        fn base_url(&self) -> &str {
            &self.url
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
                self.running.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok(Box::new(FakeServer {
                    url: format!("http://fake/{}/v1", spec.model_id),
                    running: Arc::clone(&self.running),
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
            format: Format::Gguf,
        }
    }

    fn settings() -> LlamaCppSettings {
        LlamaCppSettings {
            backend: "auto".into(),
            custom_path: None,
            idle_unload_s: 900,
            context: 4096,
            parallel: 2,
            cache_type: "f16".into(),
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

    #[test]
    fn launch_arguments_carry_the_plan() {
        let spec = LaunchSpec {
            backend: Arc::new(LlamaCpp::default()),
            format: Format::Gguf,
            gpu_memory_utilization: 0.9,
            model_id: "m".into(),
            model_path: PathBuf::from("m.gguf"),
            mmproj: Some(PathBuf::from("proj.gguf")),
            context: 16384,
            slots: 2,
            gpu_layers: u32::MAX,
            cpu_only: false,
            cache_type: "q8_0".into(),
            cache_ram_mb: 2048,
            slot_save_path: Some(PathBuf::from("slots")),
            extra_args: vec!["--flash-attn".into(), "on".into()],
        };
        let args = spec.args().join(" ");
        assert!(
            args.contains("--ctx-size 16384 --parallel 2 --n-gpu-layers 999"),
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
        assert!(
            args.ends_with("--mmproj proj.gguf --flash-attn on"),
            "{args}"
        );
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
    async fn a_model_beside_an_idle_one_is_placed_in_the_memory_actually_left() {
        // 32 GiB of RAM: "b" keeps over half its layers on the GPU beside idle "a", so "a" is
        // not swapped out, and "b" is placed in the VRAM "a" leaves.
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
        assert_eq!(loaded.len(), 2, "a stays loaded");
        let b = loaded.iter().find(|model| model.model_id == "b").unwrap();
        assert!(
            b.gpu_layers > 0 && b.gpu_layers < 32,
            "{} layers",
            b.gpu_layers
        );
        assert!(b.ram_bytes > 0, "the rest of b is in RAM");
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
}
