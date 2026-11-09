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

use crate::planner::{Budget, Footprint, Placement, place, place_on_cpu, place_on_gpu};
use crate::scan::Format;
use crate::supervisor::{LaunchSpec, Runtimes};

/// The id of the llama.cpp runtime ([`LlamaCpp`]).
pub const LLAMA_CPP: &str = "llamacpp";
/// The id of the vLLM runtime ([`Vllm`]).
pub const VLLM: &str = "vllm";

/// Why a llama.cpp model cannot load when `program` (the `llama-server` looked for) is missing,
/// with what to do in the app and on the command line.
pub fn missing_runtime(program: &Path) -> String {
    format!(
        "No llama.cpp runtime yet — open Local models → Runtime to install one or choose an \
         existing llama-server (or run `imperium runtime install`). Looked for {}.",
        program.display()
    )
}

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

    /// Where a model pinned to the CPU lane goes within `budget`: no GPU layers, everything in
    /// RAM. `None` when it does not fit, and for runtimes that cannot serve on the CPU.
    fn place_on_cpu(&self, _footprint: Footprint, _budget: Budget) -> Option<Placement> {
        None
    }

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

    fn place_on_cpu(&self, footprint: Footprint, budget: Budget) -> Option<Placement> {
        place_on_cpu(footprint, budget)
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
            // One KV cell pool shared by every slot, budgeted in tokens.
            "--ctx-size".into(),
            spec.pool_tokens.to_string(),
            "--kv-unified".into(),
        ];
        if spec.context < spec.pool_tokens {
            // Caps what one slot may use; with the whole pool as the context it could not bind.
            args.extend(["--kv-unified-per-slot".into(), spec.context.to_string()]);
        }
        args.extend([
            "--parallel".into(),
            spec.slots.to_string(),
            // An idle slot keeps its cells for the swap-out save; llama-server still purges
            // idle slots when the pool runs short.
            "--no-cache-idle-slots".into(),
            "--n-gpu-layers".into(),
            // llama.cpp clamps a large count to the model's layers.
            spec.gpu_layers.min(999).to_string(),
            "--cache-type-k".into(),
            spec.cache_type_k.clone(),
            "--cache-type-v".into(),
            spec.cache_type_v.clone(),
            "--flash-attn".into(),
            spec.flash_attn.clone(),
            // MiB of host memory for the prompts of slots that were reused; 0 disables it.
            // Never add `--slot-prompt-similarity 0`: requests are pinned to a slot with
            // `id_slot`, and it is the server's default similarity threshold that saves a pinned
            // slot's old prompt to this cache and restores the agent's own (llama.cpp
            // b12180, `get_available_slot`); 0 would skip both and re-read every prompt.
            "--cache-ram".into(),
            spec.cache_ram_mb.to_string(),
            "--jinja".into(),
        ]);
        if spec.cpu_only {
            // `--n-gpu-layers 0` alone still initialises the GPU backend (compute buffers in
            // VRAM) and runs large batched operations there; `none` uses no GPU device at all
            // (llama.cpp b12180, `--device`).
            args.extend(["--device".into(), "none".into()]);
        }
        if let Some(dir) = &spec.slot_save_path {
            // Where slots are saved before the model is swapped out, and restored from.
            args.extend(["--slot-save-path".into(), dir.display().to_string()]);
        }
        if let Some(mmproj) = &spec.mmproj {
            args.extend(["--mmproj".into(), mmproj.display().to_string()]);
            if spec.cpu_only {
                // In b12180 the projector already follows `--device none` off the GPU
                // (`common_params_parse_ex` defaults its device to the first of `--device`);
                // the flag says so explicitly, so the lane never depends on that default.
                args.push("--no-mmproj-offload".into());
            }
        }
        args.extend(spec.extra_args.iter().cloned());
        args
    }

    fn check_program(&self, program: &Path) -> Result<(), String> {
        if program.is_file() {
            return Ok(());
        }
        Err(missing_runtime(program))
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
            spec.context.to_string(),
            "--max-num-seqs".into(),
            spec.slots.to_string(),
            "--gpu-memory-utilization".into(),
            format!("{:.2}", spec.gpu_memory_utilization),
        ];
        args.extend(spec.extra_args.iter().cloned());
        args
    }
}
