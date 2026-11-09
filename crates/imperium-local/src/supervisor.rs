//! Loads local models into supervised runtime servers and hands out slots on them.
//!
//! Each model is served by the first available [`RuntimeBackend`] that accepts its format
//! (`llama-server` for GGUF, `vllm serve` for Hugging Face folders by default).
//! Each loaded model is one server started with `--parallel <slots>`. An agent takes a slot (a
//! semaphore permit and a slot index) for one model call and releases it when the call ends, so
//! agents that are running tools or waiting hold none; callers queue first-come, first-served
//! while every slot is busy, and an agent gets back the slot it used last when that slot is
//! free. Memory is reserved per loaded model against the budget measured when the supervisor
//! started. A model that does not fit may replace models nobody is using, but never one in use.
//! Idle models are unloaded after `idle_unload_s`.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use imperium_api::{AcquireFuture, Availability, LocalLease, LocalModels, LocalPresence};
use imperium_config::{LlamaCppSettings, VllmSettings};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::backend::{self, RuntimeBackend};
use crate::hardware::Hardware;
use crate::planner::{Budget, Footprint, Placement};
use crate::scan::{Format, LocalModel};

/// How often idle models are checked for unloading.
const IDLE_CHECK: Duration = Duration::from_secs(15);

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
    pub cache_type: String,
    /// MiB of host memory for llama.cpp's prompt cache (`--cache-ram`), which keeps the prompts
    /// of slots that were reused by another agent so they need not be read again; 0 disables it.
    pub cache_ram_mb: u64,
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
            && self.cache_type == other.cache_type
            && self.cache_ram_mb == other.cache_ram_mb
            && self.gpu_memory_utilization == other.gpu_memory_utilization
            && self.extra_args == other.extra_args
    }
}

/// A running model server; dropping it stops the server.
pub trait Server: Send + Sync {
    /// OpenAI-compatible base URL, ending in `/v1`.
    fn base_url(&self) -> &str;
}

pub type LaunchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn Server>, String>> + Send + 'a>>;

/// Starts servers; [`ProcessLauncher`] runs the spec's runtime as a process, tests use
/// in-process fakes.
///
/// [`ProcessLauncher`]: crate::process::ProcessLauncher
pub trait Launcher: Send + Sync + fmt::Debug {
    /// Starts a server and returns once it is ready to serve requests.
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a>;
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
}

/// Which slots of a server are free, and who used each last.
#[derive(Debug)]
struct SlotTable {
    /// Free slots, the one free the longest first.
    free: VecDeque<u32>,
    /// Who held each slot last; its cached prompt is that holder's until someone else takes it.
    last_holder: Vec<Option<String>>,
}

impl SlotTable {
    fn new(slots: u32) -> Self {
        SlotTable {
            free: (0..slots).collect(),
            last_holder: vec![None; slots as usize],
        }
    }

    /// The free slot `holder` used last, if nobody has used it since; otherwise the one that
    /// has been free the longest, so the prompts most recently used survive the longest.
    fn take(&mut self, holder: &str) -> Option<u32> {
        let own = self
            .free
            .iter()
            .position(|&slot| self.last_holder[slot as usize].as_deref() == Some(holder));
        self.free.remove(own.unwrap_or(0))
    }

    fn give_back(&mut self, slot: u32, holder: &str) {
        self.last_holder[slot as usize] = Some(holder.to_string());
        self.free.push_back(slot);
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
    /// Whether leases carry a slot index the runtime accepts in requests.
    pins_slots: bool,
    last_used: Mutex<Instant>,
}

impl SlotPool {
    fn new(base_url: String, slots: u32, pins_slots: bool) -> Arc<Self> {
        Arc::new(SlotPool {
            base_url,
            slots,
            permits: Arc::new(Semaphore::new(slots as usize)),
            table: Mutex::new(SlotTable::new(slots)),
            waiting: AtomicU32::new(0),
            pins_slots,
            last_used: Mutex::new(Instant::now()),
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
    ) -> Result<LocalLease, String> {
        let index = lock(&self.table)
            .take(holder)
            .ok_or("a slot permit was granted with no free slot")?;
        Ok(LocalLease::new(
            self.base_url.clone(),
            self.pins_slots.then_some(index),
            Slot {
                pool: Arc::clone(self),
                index,
                holder: holder.to_string(),
                _permit: permit,
            },
        ))
    }
}

/// Counts a call as queued while it waits for a slot, also when the wait is cancelled.
struct Queued<'a>(&'a AtomicU32);

impl<'a> Queued<'a> {
    fn new(waiting: &'a AtomicU32) -> Self {
        waiting.fetch_add(1, Ordering::SeqCst);
        Queued(waiting)
    }
}

impl Drop for Queued<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Loaded {
    /// Held so the server keeps running; dropping it stops the server.
    _server: Box<dyn Server>,
    pool: Arc<SlotPool>,
    placement: Placement,
}

impl Loaded {
    fn in_use(&self) -> u32 {
        self.pool.in_use()
    }
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
    loaded: Mutex<HashMap<String, Loaded>>,
    /// Turns working on each model, by model id: an attached model holds no slot between calls
    /// but is not unloaded. Locked after `loaded` when both are needed.
    attached: Arc<Mutex<HashMap<String, u32>>>,
    /// Loads happen one at a time, so two agents asking for the same model load it once.
    loading: tokio::sync::Mutex<()>,
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
    // The maps stay consistent across a panic: every update is a single insert or remove.
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
    /// its idle-unload task on the current Tokio runtime.
    pub fn with_backends(
        models: Vec<LocalModel>,
        settings: LlamaCppSettings,
        backends: Vec<Arc<dyn RuntimeBackend>>,
        hardware: &Hardware,
        launcher: Arc<dyn Launcher>,
    ) -> Arc<Self> {
        let budget = Budget::of(hardware, settings.ram_headroom_mb * 1024 * 1024);
        let supervisor = Arc::new(Supervisor {
            models: Mutex::new(models),
            settings,
            backends,
            launcher,
            budget,
            loaded: Mutex::new(HashMap::new()),
            attached: Arc::new(Mutex::new(HashMap::new())),
            loading: tokio::sync::Mutex::new(()),
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

    /// What loading `model` needs at the configured context and slots.
    pub fn footprint(&self, model: &LocalModel) -> Footprint {
        Footprint::of(
            model,
            self.settings.context,
            self.settings.parallel,
            &self.settings.cache_type,
        )
    }

    /// Where `model` would go if loaded now, counting memory held by models in use (idle ones
    /// can be unloaded to make room).
    pub fn placement(&self, model: &LocalModel) -> Option<Placement> {
        let loaded = lock(&self.loaded);
        self.place(model, self.free_budget(&loaded, true))
    }

    /// Where `model` would go on an empty machine, for listings.
    pub fn placement_when_idle(&self, model: &LocalModel) -> Option<Placement> {
        self.place(model, self.budget)
    }

    /// The runtime that serves `format`: the first available one accepting it.
    fn backend(&self, format: Format) -> Option<&Arc<dyn RuntimeBackend>> {
        self.backends
            .iter()
            .find(|backend| backend.accepts(format) && backend.available())
    }

    /// Where the model's runtime would put it; `None` when it does not fit or no runtime here
    /// serves it.
    fn place(&self, model: &LocalModel, budget: Budget) -> Option<Placement> {
        self.backend(model.format)?
            .place(self.footprint(model), budget)
    }

    pub fn loaded(&self) -> Vec<LoadedStatus> {
        let mut loaded: Vec<_> = lock(&self.loaded)
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
            })
            .collect();
        loaded.sort_by(|a, b| a.model_id.cmp(&b.model_id));
        loaded
    }

    /// The budget minus what loaded models hold; with `reclaim_idle`, idle models count as
    /// free because they can be unloaded.
    fn free_budget(&self, loaded: &HashMap<String, Loaded>, reclaim_idle: bool) -> Budget {
        let attached = lock(&self.attached);
        let held = loaded
            .iter()
            .filter(|(id, entry)| !reclaim_idle || !is_idle(id, entry, &attached))
            .map(|(_, entry)| entry);
        let (vram, ram) = held.fold((0, 0), |(vram, ram), loaded| {
            (
                vram + loaded.placement.vram_bytes,
                ram + loaded.placement.ram_bytes,
            )
        });
        Budget {
            vram_bytes: self.budget.vram_bytes.saturating_sub(vram),
            ram_bytes: self.budget.ram_bytes.saturating_sub(ram),
            unified: self.budget.unified,
        }
    }

    fn model(&self, id: &str) -> Option<LocalModel> {
        lock(&self.models)
            .iter()
            .find(|model| model.id == id)
            .cloned()
    }

    /// A slot on `id` if it is loaded, waiting for one while every slot is busy; `None` when
    /// the model is not loaded (or was unloaded while waiting).
    async fn slot_on_loaded(&self, id: &str, holder: &str) -> Result<Option<LocalLease>, String> {
        let pool = {
            // The permit is taken under the lock so an idle model cannot be unloaded between
            // finding it and using it.
            let loaded = lock(&self.loaded);
            let Some(entry) = loaded.get(id) else {
                return Ok(None);
            };
            if let Ok(permit) = Arc::clone(&entry.pool.permits).try_acquire_owned() {
                return entry.pool.lease(permit, holder).map(Some);
            }
            Arc::clone(&entry.pool)
        };
        let waiting = Instant::now();
        let permit = {
            let _queued = Queued::new(&pool.waiting);
            Arc::clone(&pool.permits)
                .acquire_owned()
                .await
                .map_err(|_| "the model was unloaded".to_string())?
        };
        let waited = waiting.elapsed();
        let same_server = lock(&self.loaded)
            .get(id)
            .is_some_and(|entry| Arc::ptr_eq(&entry.pool, &pool));
        if !same_server {
            return Ok(None);
        }
        pool.lease(permit, holder)
            .map(|lease| Some(lease.with_wait(waited)))
    }

    async fn acquire_slot(&self, id: &str, holder: &str) -> Result<LocalLease, String> {
        loop {
            if let Some(lease) = self.slot_on_loaded(id, holder).await? {
                return Ok(lease);
            }
            let loading = self.loading.lock().await;
            // Loaded while we waited: take a slot on it instead.
            if lock(&self.loaded).contains_key(id) {
                continue;
            }
            return self.load(id, holder, loading).await;
        }
    }

    /// Loads `id` and takes its first slot. `_loading` keeps other loads out until the model is
    /// registered.
    async fn load(
        &self,
        id: &str,
        holder: &str,
        _loading: tokio::sync::MutexGuard<'_, ()>,
    ) -> Result<LocalLease, String> {
        let model = self
            .model(id)
            .ok_or_else(|| format!("no local model {id:?} in models.folders"))?;
        let footprint = self.footprint(&model);
        let no_room = || {
            format!(
                "not enough free memory for {id} (needs {} MiB)",
                footprint.required_bytes() / (1024 * 1024)
            )
        };
        let backend = self.backend(model.format).ok_or_else(no_room)?;
        let (mut placement, room) = {
            let mut loaded = lock(&self.loaded);
            let placement = backend
                .place(footprint, self.free_budget(&loaded, true))
                .ok_or_else(no_room)?;
            // Make room by unloading idle models only when the model does not fit beside them.
            if backend
                .place(footprint, self.free_budget(&loaded, false))
                .is_none()
            {
                let attached = lock(&self.attached);
                loaded.retain(|other, entry| {
                    let keep = !is_idle(other, entry, &attached);
                    if !keep {
                        tracing::info!(model = %other, "unloading idle model to make room");
                    }
                    keep
                });
            }
            // What is left once the models that stay loaded are counted: idle models that fit
            // beside this one keep their memory, so the cache must not be sized from it.
            (placement, self.free_budget(&loaded, false))
        };
        // The prompt cache takes what the setting asks of the RAM left after the model, and
        // counts against the budget like the model does.
        let cache_ram_mb = if backend.slot_aware() {
            crate::planner::prompt_cache_mb(self.settings.cache_ram_mb, &placement, room)
        } else {
            0
        };
        placement.ram_bytes += cache_ram_mb * 1024 * 1024;
        let spec = LaunchSpec {
            backend: Arc::clone(backend),
            format: model.format,
            gpu_memory_utilization: backend.gpu_memory_utilization(&placement, self.budget),
            model_id: id.to_string(),
            model_path: model.path.clone(),
            mmproj: model.mmproj.clone(),
            context: self.settings.context * self.settings.parallel.max(1),
            slots: self.settings.parallel.max(1),
            gpu_layers: placement.gpu_layers,
            cache_type: self.settings.cache_type.clone(),
            cache_ram_mb,
            extra_args: backend.extra_args(),
        };
        tracing::info!(model = %id, gpu_layers = placement.gpu_layers, cache_ram_mb, "loading local model");
        let server = self.launcher.launch(&spec).await?;
        let pool = SlotPool::new(
            server.base_url().to_string(),
            spec.slots,
            backend.slot_aware(),
        );
        let permit = Arc::clone(&pool.permits)
            .try_acquire_owned()
            .map_err(|_| "a new server has no free slot".to_string())?;
        let lease = pool.lease(permit, holder)?;
        lock(&self.loaded).insert(
            id.to_string(),
            Loaded {
                _server: server,
                pool,
                placement,
            },
        );
        Ok(lease)
    }

    fn unload_idle_now(&self, idle_for: Duration) {
        let mut loaded = lock(&self.loaded);
        let attached = lock(&self.attached);
        loaded.retain(|id, entry| {
            let idle =
                is_idle(id, entry, &attached) && lock(&entry.pool.last_used).elapsed() >= idle_for;
            if idle {
                tracing::info!(model = %id, "unloading idle model");
            }
            !idle
        });
    }
}

/// Whether nothing holds `id` loaded: no call has a slot on it and no turn is attached to it.
fn is_idle(id: &str, entry: &Loaded, attached: &HashMap<String, u32>) -> bool {
    entry.in_use() == 0 && !attached.contains_key(id)
}

/// A turn attached to a model; dropping it detaches the turn.
struct Attached {
    counts: Arc<Mutex<HashMap<String, u32>>>,
    model: String,
}

impl Drop for Attached {
    fn drop(&mut self) {
        let mut counts = lock(&self.counts);
        if let Some(count) = counts.get_mut(&self.model) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.model);
            }
        }
    }
}

/// A held slot; releasing it frees the slot for the next caller and records when the model
/// was last used.
struct Slot {
    pool: Arc<SlotPool>,
    index: u32,
    holder: String,
    /// Released after `drop` has put the slot back, so a waiter that wakes finds it free.
    _permit: OwnedSemaphorePermit,
}

impl Drop for Slot {
    fn drop(&mut self) {
        lock(&self.pool.table).give_back(self.index, &self.holder);
        *lock(&self.pool.last_used) = Instant::now();
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
        supervisor.unload_idle_now(idle_for);
    }
}

impl LocalModels for Supervisor {
    fn availability(&self, id: &str) -> Availability {
        let Some(model) = self.model(id) else {
            return Availability::Unavailable;
        };
        {
            let loaded = lock(&self.loaded);
            if let Some(entry) = loaded.get(id) {
                return if entry.pool.permits.available_permits() > 0 {
                    Availability::Loaded
                } else {
                    // Busy: it can take the work once a slot frees.
                    Availability::Fits
                };
            }
        }
        match self.placement(&model) {
            Some(_) => Availability::Fits,
            None => Availability::Unavailable,
        }
    }

    fn acquire<'a>(&'a self, id: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        Box::pin(self.acquire_slot(id, holder))
    }

    fn attach(&self, id: &str, _holder: &str) -> LocalPresence {
        *lock(&self.attached).entry(id.to_string()).or_default() += 1;
        LocalPresence::new(Attached {
            counts: Arc::clone(&self.attached),
            model: id.to_string(),
        })
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
        let error = supervisor.acquire("b", "holder").await.unwrap_err();
        assert!(error.contains("not enough free memory"), "{error}");
        drop(first);
        assert_eq!(supervisor.loaded()[0].slots_in_use, 0);
    }

    #[tokio::test]
    async fn idle_models_make_room_and_unload_after_the_timeout() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        drop(supervisor.acquire("a", "holder").await.unwrap());
        // "a" is idle, so "b" may replace it.
        assert_eq!(supervisor.availability("b"), Availability::Fits);
        let b = supervisor.acquire("b", "holder").await.unwrap();
        assert_eq!(launcher.running.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.loaded()[0].model_id, "b");
        supervisor.unload_idle_now(Duration::ZERO);
        assert_eq!(supervisor.loaded().len(), 1, "a model in use stays loaded");
        drop(b);
        supervisor.unload_idle_now(Duration::ZERO);
        assert!(supervisor.loaded().is_empty());
        assert_eq!(launcher.running.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_model_attached_to_a_turn_stays_loaded_between_its_calls() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        // Session A's turn on "a" is between calls: it holds no slot but is still attached.
        let attached = supervisor.attach("a", "session-a/root");
        drop(supervisor.acquire("a", "session-a/root").await.unwrap());
        assert_eq!(supervisor.loaded()[0].slots_in_use, 0);
        assert_eq!(supervisor.availability("b"), Availability::Unavailable);
        let error = supervisor.acquire("b", "session-b/root").await.unwrap_err();
        assert!(error.contains("not enough free memory"), "{error}");
        supervisor.unload_idle_now(Duration::ZERO);
        assert_eq!(
            supervisor.loaded()[0].model_id,
            "a",
            "an attached model stays loaded"
        );
        // A's turn ends, so "a" is idle and "b" may replace it.
        drop(attached);
        let b = supervisor.acquire("b", "session-b/root").await.unwrap();
        assert_eq!(b.base_url, "http://fake/b/v1");
        assert_eq!(launcher.running.load(Ordering::SeqCst), 1);
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
            cache_type: "q8_0".into(),
            cache_ram_mb: 2048,
            extra_args: vec!["--flash-attn".into(), "on".into()],
        };
        let args = spec.args().join(" ");
        assert!(
            args.contains("--ctx-size 16384 --parallel 2 --n-gpu-layers 999"),
            "{args}"
        );
        assert!(args.contains("--cache-type-k q8_0"), "{args}");
        assert!(args.contains("--cache-ram 2048"), "{args}");
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
}
