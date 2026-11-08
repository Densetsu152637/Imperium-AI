//! Inference runtimes the supervisor can start: what each serves, where its models fit, and how
//! its server is invoked.
//!
//! The supervisor holds a list of [`RuntimeBackend`]s and serves a model with the first one that
//! accepts the model's [`Format`] and is available here. [`builtin`] gives llama.cpp then vLLM; a
//! new runtime implements the trait and is passed to [`Supervisor::with_backends`].
//!
//! [`Supervisor::with_backends`]: crate::supervisor::Supervisor::with_backends

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use imperium_config::{LlamaCppSettings, VllmSettings};

use crate::planner::{Budget, Footprint, Placement, place, place_on_gpu};
use crate::scan::Format;
use crate::supervisor::{LaunchSpec, Runtimes};

/// The id of the llama.cpp runtime ([`LlamaCpp`]).
pub const LLAMA_CPP: &str = "llamacpp";
/// The id of the vLLM runtime ([`Vllm`]).
pub const VLLM: &str = "vllm";

/// A local inference runtime serving an OpenAI-compatible API.
pub trait RuntimeBackend: Send + Sync + fmt::Debug {
    /// Short, stable name; [`ProcessLauncher`](crate::process::ProcessLauncher) finds the
    /// runtime's executable by it.
    fn id(&self) -> &str;

    /// Whether this runtime serves models stored as `format`.
    fn accepts(&self, format: Format) -> bool;

    /// Whether this runtime can run on this machine.
    fn available(&self) -> bool;

    /// Where a model needing `footprint` goes within `budget`; `None` when it does not fit.
    fn place(&self, footprint: Footprint, budget: Budget) -> Option<Placement>;

    /// The share of GPU memory a server with `placement` may take
    /// ([`LaunchSpec::gpu_memory_utilization`]); runtimes that size their own memory ignore it.
    fn gpu_memory_utilization(&self, placement: &Placement, budget: Budget) -> f64;

    /// Whether requests can be pinned to a slot by index and the server keeps a host-memory
    /// prompt cache for slots another agent reused (llama.cpp). Then the supervisor hands each
    /// lease a slot index, sizes `LaunchSpec::cache_ram_mb`, and counts it in the RAM budget.
    fn slot_aware(&self) -> bool {
        false
    }

    /// Extra arguments from the settings, passed to every server of this runtime.
    fn extra_args(&self) -> Vec<String>;

    /// The server's arguments for `spec`, without the port.
    fn launch_args(&self, spec: &LaunchSpec) -> Vec<String>;

    /// Checks `program`, the configured executable, before it is started; the error explains
    /// how to fix it.
    fn check_program(&self, _program: &Path) -> Result<(), String> {
        Ok(())
    }
}

/// llama.cpp then vLLM, configured from the settings.
pub fn builtin(settings: &LlamaCppSettings, runtimes: Runtimes) -> Vec<Arc<dyn RuntimeBackend>> {
    vec![
        Arc::new(LlamaCpp {
            extra_args: settings.extra_args.clone(),
        }),
        Arc::new(Vllm {
            settings: runtimes.vllm,
            available: runtimes.vllm_available,
        }),
    ]
}

/// llama.cpp's `llama-server`, for GGUF files. It can split a model between GPU and RAM.
#[derive(Debug, Clone, Default)]
pub struct LlamaCpp {
    /// `runtime.llamacpp.extra_args`.
    pub extra_args: Vec<String>,
}

impl RuntimeBackend for LlamaCpp {
    fn id(&self) -> &str {
        LLAMA_CPP
    }

    fn accepts(&self, format: Format) -> bool {
        format == Format::Gguf
    }

    fn available(&self) -> bool {
        true
    }

    fn place(&self, footprint: Footprint, budget: Budget) -> Option<Placement> {
        place(footprint, budget)
    }

    /// Unused: llama.cpp is sized by its layers on the GPU.
    fn gpu_memory_utilization(&self, _placement: &Placement, _budget: Budget) -> f64 {
        0.0
    }

    fn slot_aware(&self) -> bool {
        true
    }

    fn extra_args(&self) -> Vec<String> {
        self.extra_args.clone()
    }

    fn launch_args(&self, spec: &LaunchSpec) -> Vec<String> {
        let mut args = vec![
            "--model".into(),
            spec.model_path.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--ctx-size".into(),
            spec.context.to_string(),
            "--parallel".into(),
            spec.slots.to_string(),
            "--n-gpu-layers".into(),
            // llama.cpp clamps a large count to the model's layers.
            spec.gpu_layers.min(999).to_string(),
            "--cache-type-k".into(),
            spec.cache_type.clone(),
            "--cache-type-v".into(),
            spec.cache_type.clone(),
            // MiB of host memory for the prompts of slots that were reused; 0 disables it.
            "--cache-ram".into(),
            spec.cache_ram_mb.to_string(),
            "--jinja".into(),
        ];
        if let Some(mmproj) = &spec.mmproj {
            args.extend(["--mmproj".into(), mmproj.display().to_string()]);
        }
        args.extend(spec.extra_args.iter().cloned());
        args
    }

    fn check_program(&self, program: &Path) -> Result<(), String> {
        if program.is_file() {
            return Ok(());
        }
        Err(format!(
            "llama-server not found at {}; run `imperium runtime install` or set \
             runtime.llamacpp.custom_path",
            program.display()
        ))
    }
}

/// vLLM's `vllm serve`, for Hugging Face folders. It needs the whole model on the GPU and runs
/// on Linux (including WSL2) only.
#[derive(Debug, Clone, Default)]
pub struct Vllm {
    pub settings: VllmSettings,
    /// Elsewhere Hugging Face models are listed but cannot be loaded.
    pub available: bool,
}

impl RuntimeBackend for Vllm {
    fn id(&self) -> &str {
        VLLM
    }

    fn accepts(&self, format: Format) -> bool {
        format == Format::Safetensors
    }

    fn available(&self) -> bool {
        self.available
    }

    fn place(&self, footprint: Footprint, budget: Budget) -> Option<Placement> {
        place_on_gpu(footprint, budget)
    }

    /// The model's share of free VRAM plus 5%, capped by `max_gpu_memory_utilization`.
    fn gpu_memory_utilization(&self, placement: &Placement, budget: Budget) -> f64 {
        let max = self.settings.max_gpu_memory_utilization;
        if budget.vram_bytes == 0 {
            max
        } else {
            (placement.vram_bytes as f64 / budget.vram_bytes as f64 + 0.05).min(max)
        }
    }

    fn extra_args(&self) -> Vec<String> {
        self.settings.extra_args.clone()
    }

    /// The model is served under its id.
    fn launch_args(&self, spec: &LaunchSpec) -> Vec<String> {
        let mut args = vec![
            "serve".into(),
            spec.model_path.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--served-model-name".into(),
            spec.model_id.clone(),
            "--max-model-len".into(),
            (spec.context / spec.slots.max(1)).to_string(),
            "--max-num-seqs".into(),
            spec.slots.to_string(),
            "--gpu-memory-utilization".into(),
            format!("{:.2}", spec.gpu_memory_utilization),
        ];
        args.extend(spec.extra_args.iter().cloned());
        args
    }
}
