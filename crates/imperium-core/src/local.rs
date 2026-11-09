//! The daemon's local model runtime: scanned folders, the hardware probe, and the supervisor.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use imperium_config::{ModelPlacement, Paths, Settings};
use imperium_local::hardware::{Hardware, probe};
use imperium_local::hf::Weights;
use imperium_local::planner::{Budget, Placement};
use imperium_local::process::ProcessLauncher;
use imperium_local::runtime::{
    SOURCE_CUSTOM, SOURCE_DETECTED, llama_cpp_ref, resolve, server_path,
};
use imperium_local::runtime_check::home_dir;
use imperium_local::scan::{Format, Skipped, scan};
use imperium_local::supervisor::{CPU_LANE_SLOTS, CpuLane, Runtimes, Supervisor};
use imperium_protocol::methods::{
    GpuInfo, HardwareInfo, LocalModelListResult, LocalModelRow, RuntimeInfo, SkippedModel,
};

/// vLLM runs on Linux (including WSL2) only; elsewhere its models are listed but cannot load.
const VLLM_AVAILABLE: bool = cfg!(target_os = "linux");

#[derive(Debug)]
pub struct LocalRuntime {
    pub supervisor: Arc<Supervisor>,
    /// The folders and what the last scan skipped; both change when they are scanned again.
    scanned: Mutex<(Vec<PathBuf>, Vec<Skipped>)>,
    hardware: Hardware,
    /// The runtime pack (or `custom`) and the `llama-server` models are loaded with; they
    /// change when the user picks another server or installs a pack.
    server: Arc<Mutex<(String, PathBuf)>>,
    /// Starts the servers; `None` in tests that use a fake.
    launcher: Option<Arc<ProcessLauncher>>,
}

fn set_server(
    slot: &Mutex<(String, PathBuf)>,
    launcher: Option<&ProcessLauncher>,
    label: String,
    server: PathBuf,
) {
    tracing::info!(source = %label, server = %server.display(), "llama-server for new model loads");
    if let Some(launcher) = launcher {
        launcher.set_program(imperium_local::backend::LLAMA_CPP, server.clone());
    }
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = (label, server);
}

/// Folders to scan: `models.folders`, plus the download folder when it exists and is not
/// already inside one of them.
pub fn scan_folders(paths: &Paths, settings: &Settings) -> Vec<PathBuf> {
    let mut folders = settings.models.folders.clone();
    let downloads = settings.models.download_dir(paths.data_dir());
    if downloads.is_dir() && !folders.iter().any(|folder| downloads.starts_with(folder)) {
        folders.push(downloads);
    }
    folders
}

/// The local models pinned to the CPU lane by `placement = "cpu"` rows of the model table, with
/// how each is served.
pub fn cpu_pins(settings: &Settings) -> HashMap<String, CpuLane> {
    settings
        .models
        .catalog
        .iter()
        .filter(|entry| entry.placement == ModelPlacement::Cpu)
        .filter_map(|entry| {
            Some((
                entry.llamacpp_model()?.to_string(),
                CpuLane {
                    slots: entry.parallel.unwrap_or(CPU_LANE_SLOTS),
                    context: entry.context,
                    max_wait_s: entry.max_wait_s,
                },
            ))
        })
        .collect()
}

impl LocalRuntime {
    /// Scans the model folders ([`scan_folders`]), probes the hardware, and starts the
    /// supervisor. `None` when there is nothing to scan. Must run inside the Tokio runtime.
    pub fn start(paths: &Paths, settings: &Settings) -> Option<Self> {
        let folders = scan_folders(paths, settings);
        if folders.is_empty() {
            return None;
        }
        let found = scan(&folders);
        let hardware = probe(&settings.hardware);
        let llamacpp = &settings.runtime.llamacpp;
        let (pack, server) = server_path(llamacpp, paths.data_dir(), &hardware);
        tracing::info!(
            models = found.models.len(),
            skipped = found.skipped.len(),
            %pack,
            vram_free_mib = hardware.vram_free_bytes() / (1024 * 1024),
            "local models scanned"
        );
        for skipped in &found.skipped {
            tracing::warn!(path = %skipped.path.display(), reason = %skipped.reason, "skipped model file");
        }
        let vllm = &settings.runtime.vllm;
        let launcher = Arc::new(
            ProcessLauncher::new(
                server.clone(),
                PathBuf::from(&vllm.command),
                paths.data_dir().join("logs"),
            )
            .with_slot_dir(paths.data_dir().join("swap")),
        );
        let runtimes = Runtimes {
            vllm: vllm.clone(),
            vllm_available: VLLM_AVAILABLE,
        };
        let supervisor = Supervisor::start(
            found.models,
            llamacpp.clone(),
            runtimes,
            &hardware,
            Arc::clone(&launcher) as Arc<dyn imperium_local::supervisor::Launcher>,
        );
        supervisor.set_cpu_pinned(cpu_pins(settings));
        let local = LocalRuntime {
            supervisor,
            scanned: Mutex::new((folders, found.skipped)),
            hardware,
            server: Arc::new(Mutex::new((pack, server))),
            launcher: Some(launcher),
        };
        local.detect_if_missing(paths, settings);
        Some(local)
    }

    /// The hardware the models are planned against.
    pub fn hardware(&self) -> &Hardware {
        &self.hardware
    }

    /// The runtime pack (or `custom`) and `llama-server` in use.
    pub fn server(&self) -> (String, PathBuf) {
        self.server
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Loads models from now on with the `llama-server` `settings` name (a custom path or the
    /// runtime pack); models already loaded keep theirs until they are unloaded.
    pub fn use_server(&self, paths: &Paths, settings: &Settings) {
        let (pack, server) =
            server_path(&settings.runtime.llamacpp, paths.data_dir(), &self.hardware);
        self.set_server(pack, server);
        self.detect_if_missing(paths, settings);
    }

    /// Loads models from now on with `server`, labelled `label` (a build, `custom`, or
    /// `detected`).
    pub fn set_server(&self, label: String, server: PathBuf) {
        set_server(&self.server, self.launcher.as_deref(), label, server);
    }

    /// When neither a custom `llama-server` nor an installed build is there, looks for a
    /// `llama-server` already on this machine in the background ([`resolve`]) and uses the best
    /// one found, unless the server was changed meanwhile.
    fn detect_if_missing(&self, paths: &Paths, settings: &Settings) {
        let current = self.server();
        if current.0 == SOURCE_CUSTOM
            || current.1.is_file()
            || tokio::runtime::Handle::try_current().is_err()
        {
            return;
        }
        let slot = Arc::clone(&self.server);
        let launcher = self.launcher.clone();
        let hardware = self.hardware.clone();
        let llamacpp = settings.runtime.llamacpp.clone();
        let data_dir = paths.data_dir().to_path_buf();
        tokio::spawn(async move {
            let resolved = resolve(&llamacpp, &data_dir, &hardware, home_dir().as_deref()).await;
            if resolved.source != SOURCE_DETECTED {
                tracing::info!(
                    looked_for = %current.1.display(),
                    "no llama.cpp runtime installed and no usable llama-server found"
                );
                return;
            }
            let unchanged = *slot.lock().unwrap_or_else(PoisonError::into_inner) == current;
            if unchanged {
                set_server(
                    &slot,
                    launcher.as_deref(),
                    resolved.label(),
                    resolved.server,
                );
            }
        });
    }

    /// Scans the folders again, such as after a download, so new models can be used. The caller
    /// applies the per-model settings afterwards, reading them under the lock that serialises
    /// saves (see `DaemonState::rescan_local_models`), so a save made meanwhile is not undone.
    pub fn rescan(&self, paths: &Paths, settings: &Settings) {
        let folders = scan_folders(paths, settings);
        let found = scan(&folders);
        tracing::info!(models = found.models.len(), "local models scanned again");
        self.supervisor.set_models(found.models);
        *self.scanned.lock().unwrap_or_else(PoisonError::into_inner) = (folders, found.skipped);
    }

    /// `gpu`, `partial`, `cpu`, or `none`: how a model with `placement` would load.
    pub fn placement_label(&self, placement: Option<&Placement>, cpu_lane: bool) -> &'static str {
        if cpu_lane && placement.is_some() {
            "cpu"
        } else {
            placement_name(placement, &self.hardware)
        }
    }

    /// Hands the supervisor the per-model settings and CPU-lane pins in `settings`; loaded models
    /// whose launch shape changed restart once nothing is using them.
    pub fn apply_model_settings(&self, settings: &Settings) {
        self.supervisor.set_cpu_pinned(cpu_pins(settings));
        self.supervisor
            .set_model_settings(settings.runtime.llamacpp.models.clone());
    }

    /// The memory local models may use.
    pub fn budget(&self) -> Budget {
        self.supervisor.budget()
    }

    /// What `models.local` reports, with the configured folders and default model from
    /// `settings`.
    pub fn listing_with(&self, settings: &Settings) -> LocalModelListResult {
        let mut listing = self.listing();
        listing.configured_folders = settings
            .models
            .folders
            .iter()
            .map(|folder| folder.display().to_string())
            .collect();
        listing.default_model.clone_from(&settings.models.default);
        listing.enabled = !listing.configured_folders.is_empty() || !listing.models.is_empty();
        listing
    }

    pub fn listing(&self) -> LocalModelListResult {
        let loaded = self.supervisor.loaded();
        let models = self
            .supervisor
            .models()
            .iter()
            .map(|model| {
                let footprint = self.supervisor.footprint(model);
                let launch = self.supervisor.launch(&model.id);
                let placement = self.supervisor.placement_when_idle(model);
                let status = loaded.iter().find(|l| l.model_id == model.id);
                let cpu_lane = self.supervisor.cpu_slots(&model.id).is_some();
                LocalModelRow {
                    id: model.id.clone(),
                    runtime: match model.format {
                        Format::Gguf => "llamacpp".into(),
                        Format::Safetensors => "vllm".into(),
                    },
                    path: model.path.display().to_string(),
                    repo: model.repo.clone(),
                    mmproj: model.mmproj.as_ref().map(|path| path.display().to_string()),
                    problem: (model.format == Format::Safetensors && !VLLM_AVAILABLE).then(|| {
                        "vLLM, which serves Hugging Face folders, runs on Linux or WSL2 only; \
                         use a GGUF build of this model here"
                            .to_string()
                    }),
                    architecture: model.info.architecture.clone(),
                    quantization: model.info.quantization().map(str::to_string),
                    parts: model.parts,
                    size_bytes: model.size_bytes,
                    context_length: model.info.context_length,
                    required_bytes: footprint.required_bytes(),
                    placement: self.placement_label(placement.as_ref(), cpu_lane).into(),
                    gpu_layers: placement
                        .map(|p| p.gpu_layers.min(footprint.layers.unwrap_or(999))),
                    loaded: status.is_some(),
                    slots: status.map_or(launch.slots, |s| s.slots),
                    slots_in_use: status.map_or(0, |s| s.slots_in_use),
                    queued: status.map_or(0, |s| s.queued),
                    cpu_lane,
                    tokens_per_s: self.supervisor.tokens_per_s(&model.id),
                    context: status.map_or(launch.context, |s| s.context),
                    pool_tokens: status.map_or(launch.pool_tokens, |s| s.pool_tokens),
                    pool_reserved_tokens: status.map_or(0, |s| s.pool_reserved_tokens),
                }
            })
            .collect();
        let budget = self.supervisor.budget();
        let scanned = self.scanned.lock().unwrap_or_else(PoisonError::into_inner);
        let (folders, skipped) = &*scanned;
        let (pack, server) = self.server();
        LocalModelListResult {
            enabled: true,
            folders: folders
                .iter()
                .map(|folder| folder.display().to_string())
                .collect(),
            configured_folders: Vec::new(),
            default_model: None,
            models,
            skipped: skipped
                .iter()
                .map(|skipped| SkippedModel {
                    path: skipped.path.display().to_string(),
                    reason: skipped.reason.clone(),
                })
                .collect(),
            hardware: Some(HardwareInfo {
                gpus: self
                    .hardware
                    .gpus
                    .iter()
                    .map(|gpu| GpuInfo {
                        name: gpu.name.clone(),
                        vendor: gpu.vendor.clone(),
                        vram_total_bytes: gpu.vram_total_bytes,
                        vram_free_bytes: gpu.vram_free_bytes,
                    })
                    .collect(),
                ram_total_bytes: self.hardware.ram_total_bytes,
                ram_available_bytes: self.hardware.ram_available_bytes,
                cpu_cores: u32::try_from(self.hardware.cpu_cores).unwrap_or(u32::MAX),
                unified_memory: self.hardware.unified_memory,
                overridden: self.hardware.overridden,
                budget_vram_bytes: budget.vram_bytes,
                budget_ram_bytes: budget.ram_bytes,
            }),
            runtime: Some(RuntimeInfo {
                pack,
                server_path: server.display().to_string(),
                installed: server.is_file(),
                llama_cpp_ref: llama_cpp_ref().into(),
            }),
        }
    }
}

fn placement_name(placement: Option<&Placement>, hardware: &Hardware) -> &'static str {
    match placement {
        None => "none",
        Some(p) if p.all_layers_on_gpu => "gpu",
        Some(p) if p.gpu_layers > 0 => "partial",
        Some(_) if hardware.unified_memory => "gpu",
        Some(_) => "cpu",
    }
}

/// How a model of `size_bytes` not on disk yet would load within `budget`, for download
/// choices: `gpu`, `partial`, `cpu`, or `none`. vLLM needs the GPU and Linux.
pub fn fit(size_bytes: u64, weights: Weights, budget: Budget) -> &'static str {
    imperium_local::suggest::fit(size_bytes, weights, budget).as_str()
}

/// The memory budget when no local runtime is running, from a fresh hardware probe.
pub fn probed_budget(settings: &Settings) -> Budget {
    Budget::of(
        &probe(&settings.hardware),
        settings.runtime.llamacpp.ram_headroom_mb * 1024 * 1024,
    )
}

/// The listing when no folders are configured.
pub fn disabled() -> LocalModelListResult {
    LocalModelListResult {
        enabled: false,
        folders: Vec::new(),
        configured_folders: Vec::new(),
        default_model: None,
        models: Vec::new(),
        skipped: Vec::new(),
        hardware: None,
        runtime: None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use imperium_config::{Config, Layer, Source};
    use imperium_engine::LocalModels;
    use imperium_local::gguf::GgufInfo;
    use imperium_local::hardware::Gpu;
    use imperium_local::scan::LocalModel;
    use imperium_local::supervisor::{LaunchFuture, LaunchSpec, Launcher, Server};

    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[derive(Debug)]
    struct FakeLauncher;

    struct FakeServer(String);

    impl Server for FakeServer {
        fn base_url(&self) -> &str {
            &self.0
        }
    }

    impl Launcher for FakeLauncher {
        fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
            Box::pin(async move {
                Ok(
                    Box::new(FakeServer(format!("http://fake/{}/v1", spec.model_id)))
                        as Box<dyn Server>,
                )
            })
        }
    }

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

    fn settings(table: &str) -> Settings {
        Config::from_layers([
            Layer::defaults(),
            Layer::new(Source::User("u".into()), table.parse().unwrap()),
        ])
        .settings()
        .unwrap()
    }

    #[test]
    fn cpu_lane_pins_come_from_the_model_table() {
        let settings = settings(
            r#"
            [[models.catalog]]
            id = "critic"
            spec = "llamacpp:qwen3-30b"
            location = "local"
            tier = "strong"
            placement = "cpu"
            parallel = 2
            context = 32768
            max_wait_s = 7200

            [[models.catalog]]
            id = "lane"
            spec = "llamacpp:consul"
            location = "local"
            tier = "strong"
            placement = "cpu"

            [[models.catalog]]
            id = "coder"
            spec = "llamacpp:coder"
            location = "local"
            tier = "standard"
            "#,
        );
        assert_eq!(
            cpu_pins(&settings),
            HashMap::from([
                (
                    "qwen3-30b".to_string(),
                    CpuLane {
                        slots: 2,
                        context: Some(32768),
                        max_wait_s: Some(7200),
                    }
                ),
                ("consul".to_string(), CpuLane::default()),
            ])
        );
    }

    #[tokio::test]
    async fn cpu_lane_listing_reports_the_lane_and_the_measured_speed() {
        let hardware = Hardware {
            gpus: vec![Gpu {
                name: "test".into(),
                vendor: "nvidia".into(),
                vram_total_bytes: 24 * GIB,
                vram_free_bytes: 24 * GIB,
                driver: None,
            }],
            ram_total_bytes: 64 * GIB,
            ram_available_bytes: 64 * GIB,
            cpu_cores: 16,
            unified_memory: false,
            overridden: true,
        };
        let settings = settings("");
        let supervisor = Supervisor::start(
            vec![model("coder", 6), model("critic", 18)],
            settings.runtime.llamacpp.clone(),
            Runtimes::default(),
            &hardware,
            Arc::new(FakeLauncher),
        );
        supervisor.set_cpu_pinned(HashMap::from([("critic".to_string(), CpuLane::default())]));
        let runtime = LocalRuntime {
            supervisor: Arc::clone(&supervisor),
            scanned: Mutex::new((Vec::new(), Vec::new())),
            hardware,
            server: Arc::new(Mutex::new(("cpu".into(), PathBuf::from("llama-server")))),
            launcher: None,
        };
        let row = |listing: &LocalModelListResult, id: &str| {
            listing
                .models
                .iter()
                .find(|row| row.id == id)
                .cloned()
                .unwrap()
        };

        let listing = runtime.listing();
        let (coder, critic) = (row(&listing, "coder"), row(&listing, "critic"));
        assert_eq!(coder.placement, "gpu");
        assert!(!coder.cpu_lane);
        assert_eq!(
            (critic.placement.as_str(), critic.gpu_layers),
            ("cpu", Some(0))
        );
        assert!(critic.cpu_lane);
        assert_eq!(critic.slots, 1, "a lane serves one call at a time");
        assert_eq!(
            coder.slots,
            settings.runtime.llamacpp.default_launch().slots,
            "a model without settings gets the default slots, not `parallel`"
        );
        assert_eq!(
            (coder.context, coder.pool_tokens, coder.pool_reserved_tokens),
            (
                settings.runtime.llamacpp.context,
                settings.runtime.llamacpp.default_launch().pool_tokens,
                0
            )
        );
        assert_eq!((coder.tokens_per_s, critic.tokens_per_s), (None, None));

        // A call that finished on the lane: 120 tokens generated over 10 s.
        let lease = supervisor.acquire("critic", "reviewer").await.unwrap();
        supervisor.record_throughput("critic", 120, Duration::from_secs(10));
        drop(lease);
        let listing = runtime.listing();
        let critic = row(&listing, "critic");
        assert!(critic.loaded);
        assert_eq!(critic.tokens_per_s, Some(12.0));
        assert_eq!(row(&listing, "coder").tokens_per_s, None);
    }
}
