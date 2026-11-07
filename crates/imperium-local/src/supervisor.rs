//! Loads local models into supervised `llama-server` processes and hands out slots on them.
//!
//! Each loaded model is one server started with `--parallel <slots>`; an agent holds a slot
//! (a semaphore permit) for its turn. Memory is reserved per loaded model against the budget
//! measured when the supervisor started. A model that does not fit may replace models nobody is
//! using, but never one in use. Idle models are unloaded after `idle_unload_s`.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use imperium_config::{LlamaCppSettings, VllmSettings};
use imperium_core::{AcquireFuture, Availability, LocalLease, LocalModels};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::hardware::Hardware;
use crate::planner::{Budget, Footprint, Placement, place, place_on_gpu};
use crate::scan::{Format, LocalModel};

/// How often idle models are checked for unloading.
const IDLE_CHECK: Duration = Duration::from_secs(15);

/// What a server is started with.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchSpec {
    pub format: Format,
    pub model_id: String,
    pub model_path: PathBuf,
    pub mmproj: Option<PathBuf>,
    /// Total context (`-c`): the per-slot context times the slots.
    pub context: u32,
    pub slots: u32,
    pub gpu_layers: u32,
    pub cache_type: String,
    /// vLLM's `--gpu-memory-utilization` (the share of GPU memory it may take).
    pub gpu_memory_utilization: f64,
    pub extra_args: Vec<String>,
}

impl LaunchSpec {
    /// The runtime's arguments, without the port.
    pub fn args(&self) -> Vec<String> {
        match self.format {
            Format::Gguf => self.llama_args(),
            Format::Safetensors => self.vllm_args(),
        }
    }

    /// `vllm serve` arguments; the model is served under its id.
    fn vllm_args(&self) -> Vec<String> {
        let mut args = vec![
            "serve".into(),
            self.model_path.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--served-model-name".into(),
            self.model_id.clone(),
            "--max-model-len".into(),
            (self.context / self.slots.max(1)).to_string(),
            "--max-num-seqs".into(),
            self.slots.to_string(),
            "--gpu-memory-utilization".into(),
            format!("{:.2}", self.gpu_memory_utilization),
        ];
        args.extend(self.extra_args.iter().cloned());
        args
    }

    /// `llama-server` arguments.
    fn llama_args(&self) -> Vec<String> {
        let mut args = vec![
            "--model".into(),
            self.model_path.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--ctx-size".into(),
            self.context.to_string(),
            "--parallel".into(),
            self.slots.to_string(),
            "--n-gpu-layers".into(),
            // llama.cpp clamps a large count to the model's layers.
            self.gpu_layers.min(999).to_string(),
            "--cache-type-k".into(),
            self.cache_type.clone(),
            "--cache-type-v".into(),
            self.cache_type.clone(),
            "--jinja".into(),
        ];
        if let Some(mmproj) = &self.mmproj {
            args.extend(["--mmproj".into(), mmproj.display().to_string()]);
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// A running model server; dropping it stops the server.
pub trait Server: Send + Sync {
    /// OpenAI-compatible base URL, ending in `/v1`.
    fn base_url(&self) -> &str;
}

pub type LaunchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn Server>, String>> + Send + 'a>>;

/// Starts servers; [`ProcessLauncher`] runs `llama-server`, tests use in-process fakes.
pub trait Launcher: Send + Sync + fmt::Debug {
    /// Starts a server and returns once it is ready to serve requests.
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a>;
}

/// Runtimes other than llama.cpp.
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
    pub gpu_layers: u32,
    pub vram_bytes: u64,
    pub ram_bytes: u64,
}

struct Loaded {
    /// Held so the server keeps running; dropping it stops the server.
    _server: Box<dyn Server>,
    base_url: String,
    slots: u32,
    permits: Arc<Semaphore>,
    placement: Placement,
    last_used: Arc<Mutex<Instant>>,
}

impl Loaded {
    fn in_use(&self) -> u32 {
        self.slots - u32::try_from(self.permits.available_permits()).unwrap_or(self.slots)
    }
}

/// Supervises local model servers. Cheap to share: hand out `Arc<Supervisor>`.
pub struct Supervisor {
    models: Vec<LocalModel>,
    settings: LlamaCppSettings,
    vllm: VllmSettings,
    /// Whether vLLM can run here; Hugging Face models are unavailable otherwise.
    vllm_available: bool,
    launcher: Arc<dyn Launcher>,
    /// Memory free when the supervisor started, less the RAM headroom.
    budget: Budget,
    loaded: Mutex<HashMap<String, Loaded>>,
    /// Loads happen one at a time, so two agents asking for the same model load it once.
    loading: tokio::sync::Mutex<()>,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("models", &self.models.len())
            .field("budget", &self.budget)
            .field("launcher", &self.launcher)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The maps stay consistent across a panic: every update is a single insert or remove.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Supervisor {
    /// Creates a supervisor and starts its idle-unload task on the current Tokio runtime.
    pub fn start(
        models: Vec<LocalModel>,
        settings: LlamaCppSettings,
        runtimes: Runtimes,
        hardware: &Hardware,
        launcher: Arc<dyn Launcher>,
    ) -> Arc<Self> {
        let headroom = settings.ram_headroom_mb * 1024 * 1024;
        let budget = Budget {
            vram_bytes: hardware.vram_free_bytes(),
            ram_bytes: hardware.ram_available_bytes.saturating_sub(headroom),
            unified: hardware.unified_memory,
        };
        let vllm_available = runtimes.vllm_available;
        let supervisor = Arc::new(Supervisor {
            models,
            settings,
            vllm: runtimes.vllm,
            vllm_available,
            launcher,
            budget,
            loaded: Mutex::new(HashMap::new()),
            loading: tokio::sync::Mutex::new(()),
        });
        let weak = Arc::downgrade(&supervisor);
        tokio::spawn(unload_idle(weak));
        supervisor
    }

    pub fn models(&self) -> &[LocalModel] {
        &self.models
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

    /// llama.cpp can split a model between GPU and RAM; vLLM needs it all on the GPU, and
    /// cannot run where vLLM is unavailable.
    fn place(&self, model: &LocalModel, budget: Budget) -> Option<Placement> {
        let footprint = self.footprint(model);
        match model.format {
            Format::Gguf => place(footprint, budget),
            Format::Safetensors if self.vllm_available => place_on_gpu(footprint, budget),
            Format::Safetensors => None,
        }
    }

    pub fn loaded(&self) -> Vec<LoadedStatus> {
        let mut loaded: Vec<_> = lock(&self.loaded)
            .iter()
            .map(|(id, loaded)| LoadedStatus {
                model_id: id.clone(),
                base_url: loaded.base_url.clone(),
                slots: loaded.slots,
                slots_in_use: loaded.in_use(),
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
        let held = loaded
            .values()
            .filter(|loaded| !reclaim_idle || loaded.in_use() > 0);
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

    fn model(&self, id: &str) -> Option<&LocalModel> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Takes a free slot if `id` is loaded and has one.
    fn try_slot(&self, id: &str) -> Option<LocalLease> {
        let loaded = lock(&self.loaded);
        let entry = loaded.get(id)?;
        let permit = Arc::clone(&entry.permits).try_acquire_owned().ok()?;
        Some(lease(entry, permit))
    }

    async fn acquire_slot(&self, id: &str) -> Result<LocalLease, String> {
        if let Some(lease) = self.try_slot(id) {
            return Ok(lease);
        }
        let _loading = self.loading.lock().await;
        // Loaded while we waited, or loaded with every slot busy: wait for a slot.
        let busy = lock(&self.loaded)
            .get(id)
            .map(|entry| Arc::clone(&entry.permits));
        if let Some(permits) = busy {
            drop(_loading);
            let permit = permits
                .acquire_owned()
                .await
                .map_err(|_| "the model was unloaded".to_string())?;
            let loaded = lock(&self.loaded);
            let entry = loaded.get(id).ok_or("the model was unloaded")?;
            return Ok(lease(entry, permit));
        }
        let model = self
            .model(id)
            .ok_or_else(|| format!("no local model {id:?} in models.folders"))?
            .clone();
        let placement = {
            let mut loaded = lock(&self.loaded);
            let footprint = self.footprint(&model);
            let placement = self
                .place(&model, self.free_budget(&loaded, true))
                .ok_or_else(|| {
                    format!(
                        "not enough free memory for {id} (needs {} MiB)",
                        footprint.required_bytes() / (1024 * 1024)
                    )
                })?;
            // Make room by unloading idle models only when the model does not fit beside them.
            if self
                .place(&model, self.free_budget(&loaded, false))
                .is_none()
            {
                loaded.retain(|other, entry| {
                    let keep = entry.in_use() > 0;
                    if !keep {
                        tracing::info!(model = %other, "unloading idle model to make room");
                    }
                    keep
                });
            }
            placement
        };
        let vllm = &self.vllm;
        let spec = LaunchSpec {
            format: model.format,
            gpu_memory_utilization: if self.budget.vram_bytes == 0 {
                vllm.max_gpu_memory_utilization
            } else {
                (placement.vram_bytes as f64 / self.budget.vram_bytes as f64 + 0.05)
                    .min(vllm.max_gpu_memory_utilization)
            },
            model_id: id.to_string(),
            model_path: model.path.clone(),
            mmproj: model.mmproj.clone(),
            context: self.settings.context * self.settings.parallel.max(1),
            slots: self.settings.parallel.max(1),
            gpu_layers: placement.gpu_layers,
            cache_type: self.settings.cache_type.clone(),
            extra_args: match model.format {
                Format::Gguf => self.settings.extra_args.clone(),
                Format::Safetensors => vllm.extra_args.clone(),
            },
        };
        tracing::info!(model = %id, gpu_layers = placement.gpu_layers, "loading local model");
        let server = self.launcher.launch(&spec).await?;
        let base_url = server.base_url().to_string();
        let permits = Arc::new(Semaphore::new(spec.slots as usize));
        let permit = Arc::clone(&permits)
            .try_acquire_owned()
            .map_err(|_| "a new server has no free slot".to_string())?;
        let entry = Loaded {
            _server: server,
            base_url,
            slots: spec.slots,
            permits,
            placement,
            last_used: Arc::new(Mutex::new(Instant::now())),
        };
        let lease = lease(&entry, permit);
        lock(&self.loaded).insert(id.to_string(), entry);
        Ok(lease)
    }

    fn unload_idle_now(&self, idle_for: Duration) {
        lock(&self.loaded).retain(|id, entry| {
            let idle = entry.in_use() == 0 && lock(&entry.last_used).elapsed() >= idle_for;
            if idle {
                tracing::info!(model = %id, "unloading idle model");
            }
            !idle
        });
    }
}

fn lease(entry: &Loaded, permit: OwnedSemaphorePermit) -> LocalLease {
    LocalLease::new(
        entry.base_url.clone(),
        Slot {
            _permit: permit,
            last_used: Arc::clone(&entry.last_used),
        },
    )
}

/// A held slot; releasing it records when the model was last used.
struct Slot {
    _permit: OwnedSemaphorePermit,
    last_used: Arc<Mutex<Instant>>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        *lock(&self.last_used) = Instant::now();
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
                return if entry.permits.available_permits() > 0 {
                    Availability::Loaded
                } else {
                    // Busy: it can take the work once a slot frees.
                    Availability::Fits
                };
            }
        }
        match self.placement(model) {
            Some(_) => Availability::Fits,
            None => Availability::Unavailable,
        }
    }

    fn acquire<'a>(&'a self, id: &'a str) -> AcquireFuture<'a> {
        Box::pin(self.acquire_slot(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::GgufInfo;
    use crate::hardware::Gpu;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const GIB: u64 = 1024 * 1024 * 1024;

    #[derive(Debug, Default)]
    struct FakeLauncher {
        launches: AtomicUsize,
        running: Arc<AtomicUsize>,
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
            extra_args: Vec::new(),
        }
    }

    /// 10 GiB of VRAM, no spare system memory: room for one 6 GiB model at a time.
    fn supervisor(launcher: Arc<FakeLauncher>) -> Arc<Supervisor> {
        let hardware = Hardware {
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
        };
        Supervisor::start(
            vec![model("a", 6), model("b", 6), model("huge", 40)],
            settings(),
            Runtimes::default(),
            &hardware,
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

        let (first, second) = tokio::join!(supervisor.acquire("a"), supervisor.acquire("a"));
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!(first.base_url, "http://fake/a/v1");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
        // Both slots are taken, so it reports busy-but-usable; the other model does not fit
        // beside a model in use.
        assert_eq!(supervisor.availability("a"), Availability::Fits);
        assert_eq!(supervisor.availability("b"), Availability::Unavailable);
        drop(second);
        assert_eq!(supervisor.availability("a"), Availability::Loaded);
        let error = supervisor.acquire("b").await.unwrap_err();
        assert!(error.contains("not enough free memory"), "{error}");
        drop(first);
        assert_eq!(supervisor.loaded()[0].slots_in_use, 0);
    }

    #[tokio::test]
    async fn idle_models_make_room_and_unload_after_the_timeout() {
        let launcher = Arc::new(FakeLauncher::default());
        let supervisor = supervisor(Arc::clone(&launcher));
        drop(supervisor.acquire("a").await.unwrap());
        // "a" is idle, so "b" may replace it.
        assert_eq!(supervisor.availability("b"), Availability::Fits);
        let b = supervisor.acquire("b").await.unwrap();
        assert_eq!(launcher.running.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.loaded()[0].model_id, "b");
        supervisor.unload_idle_now(Duration::ZERO);
        assert_eq!(supervisor.loaded().len(), 1, "a model in use stays loaded");
        drop(b);
        supervisor.unload_idle_now(Duration::ZERO);
        assert!(supervisor.loaded().is_empty());
        assert_eq!(launcher.running.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn launch_arguments_carry_the_plan() {
        let spec = LaunchSpec {
            format: Format::Gguf,
            gpu_memory_utilization: 0.9,
            model_id: "m".into(),
            model_path: PathBuf::from("m.gguf"),
            mmproj: Some(PathBuf::from("proj.gguf")),
            context: 16384,
            slots: 2,
            gpu_layers: u32::MAX,
            cache_type: "q8_0".into(),
            extra_args: vec!["--flash-attn".into(), "on".into()],
        };
        let args = spec.args().join(" ");
        assert!(
            args.contains("--ctx-size 16384 --parallel 2 --n-gpu-layers 999"),
            "{args}"
        );
        assert!(args.contains("--cache-type-k q8_0"), "{args}");
        assert!(
            args.ends_with("--mmproj proj.gguf --flash-attn on"),
            "{args}"
        );
        let vllm = LaunchSpec {
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
        let hardware = Hardware {
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
        };
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
            launcher,
        );
        assert_eq!(with.availability("hf"), Availability::Fits);
        let lease = with.acquire("hf").await.unwrap();
        assert_eq!(lease.base_url, "http://fake/hf/v1");
    }
}
