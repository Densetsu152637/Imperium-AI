//! The daemon's local model runtime: scanned folders, the hardware probe, and the supervisor.

use std::path::PathBuf;
use std::sync::Arc;

use imperium_config::{Paths, Settings};
use imperium_local::hardware::{Hardware, probe};
use imperium_local::planner::Placement;
use imperium_local::process::ProcessLauncher;
use imperium_local::runtime::{llama_cpp_ref, server_path};
use imperium_local::scan::{Format, Skipped, scan};
use imperium_local::supervisor::{Runtimes, Supervisor};
use imperium_protocol::methods::{
    GpuInfo, HardwareInfo, LocalModelListResult, LocalModelRow, RuntimeInfo, SkippedModel,
};

#[derive(Debug)]
pub struct LocalRuntime {
    pub supervisor: Arc<Supervisor>,
    folders: Vec<PathBuf>,
    hardware: Hardware,
    skipped: Vec<Skipped>,
    pack: String,
    server: PathBuf,
}

impl LocalRuntime {
    /// Scans `models.folders`, probes the hardware, and starts the supervisor. `None` when no
    /// folders are configured. Must run inside the Tokio runtime.
    pub fn start(paths: &Paths, settings: &Settings) -> Option<Self> {
        let folders = settings.models.folders.clone();
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
        let launcher = Arc::new(ProcessLauncher::new(
            server.clone(),
            PathBuf::from(&vllm.command),
            paths.data_dir().join("logs"),
        ));
        let runtimes = Runtimes {
            vllm: vllm.clone(),
            vllm_available: cfg!(target_os = "linux"),
        };
        let supervisor = Supervisor::start(
            found.models,
            llamacpp.clone(),
            runtimes,
            &hardware,
            launcher,
        );
        Some(LocalRuntime {
            supervisor,
            folders,
            hardware,
            skipped: found.skipped,
            pack,
            server,
        })
    }

    pub fn listing(&self) -> LocalModelListResult {
        let loaded = self.supervisor.loaded();
        let models = self
            .supervisor
            .models()
            .iter()
            .map(|model| {
                let footprint = self.supervisor.footprint(model);
                let placement = self.supervisor.placement_when_idle(model);
                let status = loaded.iter().find(|l| l.model_id == model.id);
                LocalModelRow {
                    id: model.id.clone(),
                    runtime: match model.format {
                        Format::Gguf => "llamacpp".into(),
                        Format::Safetensors => "vllm".into(),
                    },
                    path: model.path.display().to_string(),
                    architecture: model.info.architecture.clone(),
                    quantization: model.info.quantization().map(str::to_string),
                    parts: model.parts,
                    size_bytes: model.size_bytes,
                    context_length: model.info.context_length,
                    required_bytes: footprint.required_bytes(),
                    placement: placement_name(placement.as_ref(), &self.hardware).into(),
                    gpu_layers: placement
                        .map(|p| p.gpu_layers.min(footprint.layers.unwrap_or(999))),
                    loaded: status.is_some(),
                    slots: status.map_or(self.supervisor.settings().parallel, |s| s.slots),
                    slots_in_use: status.map_or(0, |s| s.slots_in_use),
                }
            })
            .collect();
        let budget = self.supervisor.budget();
        LocalModelListResult {
            enabled: true,
            folders: self
                .folders
                .iter()
                .map(|folder| folder.display().to_string())
                .collect(),
            models,
            skipped: self
                .skipped
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
                pack: self.pack.clone(),
                server_path: self.server.display().to_string(),
                installed: self.server.is_file(),
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

/// The listing when no folders are configured.
pub fn disabled() -> LocalModelListResult {
    LocalModelListResult {
        enabled: false,
        folders: Vec::new(),
        models: Vec::new(),
        skipped: Vec::new(),
        hardware: None,
        runtime: None,
    }
}
