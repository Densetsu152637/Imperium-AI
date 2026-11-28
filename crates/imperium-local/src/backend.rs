//! Inference runtimes the supervisor can start: what each serves, where its models fit, and how
//! its server is invoked.
//!
//! The supervisor holds a list of [`RuntimeBackend`]s and serves a model with the first one that
//! accepts the model's [`Format`] and is available here. [`builtin`] gives llama.cpp, vLLM, then
//! `ncm-server`; a new runtime implements the trait and is passed to
//! [`Supervisor::with_backends`].
//!
//! [`Supervisor::with_backends`]: crate::supervisor::Supervisor::with_backends

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use imperium_config::{LlamaCppSettings, ModelLaunch, NcmSettings, VllmSettings};

use crate::planner::{Budget, Footprint, Placement, place, place_on_cpu, place_on_gpu};
use crate::scan::Format;
use crate::supervisor::{LaunchSpec, Runtimes};

/// The id of the llama.cpp runtime ([`LlamaCpp`]).
pub const LLAMA_CPP: &str = "llamacpp";
/// The id of the vLLM runtime ([`Vllm`]).
pub const VLLM: &str = "vllm";
/// The id of the NCM runtime ([`Ncm`]), `ncm-server`.
pub const NCM: &str = "ncm";

/// Why a llama.cpp model cannot load when `program` (the `llama-server` looked for) is missing,
/// with what to do in the app and on the command line.
pub fn missing_runtime(program: &Path) -> String {
    format!(
        "No llama.cpp runtime yet — open Local models → Runtime to install one or choose an \
         existing llama-server (or run `imperium runtime install`). Looked for {}.",
        program.display()
    )
}

/// Environment variables llama-server reads for its draft model (b12180 README), removed from
/// the environment of every llama.cpp server: the draft is Imperium's to choose and count.
const DRAFT_ENV: [&str; 12] = [
    "LLAMA_ARG_SPEC_DRAFT_MODEL",
    "LLAMA_ARG_SPEC_DRAFT_HF_REPO",
    "LLAMA_ARG_N_GPU_LAYERS_DRAFT",
    "LLAMA_ARG_SPEC_DRAFT_N_MAX",
    "LLAMA_ARG_SPEC_DRAFT_N_MIN",
    "LLAMA_ARG_DRAFT_MAX",
    "LLAMA_ARG_DRAFT_MIN",
    "LLAMA_ARG_SPEC_DRAFT_CACHE_TYPE_K",
    "LLAMA_ARG_SPEC_DRAFT_CACHE_TYPE_V",
    "LLAMA_ARG_SPEC_DRAFT_P_MIN",
    "LLAMA_ARG_SPEC_DRAFT_P_SPLIT",
    "LLAMA_ARG_SPEC_DRAFT_N_UBATCH",
];

/// The prefix of llama.cpp's draft environment variables (`LLAMA_ARG_SPEC_DRAFT_*`), all of
/// which are removed, those a later release adds too.
const DRAFT_ENV_PREFIX: &str = "LLAMA_ARG_SPEC_DRAFT_";

/// The environment variables to remove from the server started for `spec`: llama.cpp's draft
/// settings, and `LLAMA_ARG_SPEC_TYPE` while Imperium passes `--spec-type draft-simple` itself
/// (without a draft it is left to the user, such as for n-gram speculation).
pub fn scrubbed_env(spec: &LaunchSpec) -> Vec<String> {
    if spec.backend.id() != LLAMA_CPP {
        return Vec::new();
    }
    let present = std::env::vars_os().filter_map(|(name, _)| name.into_string().ok());
    scrub_names(present, spec.draft.is_some())
}

/// [`scrubbed_env`] for the variables named in `present`: the known draft ones, and any
/// `LLAMA_ARG_SPEC_DRAFT_*` among `present`.
pub fn scrub_names(present: impl IntoIterator<Item = String>, drafting: bool) -> Vec<String> {
    let mut names: Vec<String> = DRAFT_ENV.iter().map(ToString::to_string).collect();
    names.extend(
        present.into_iter().filter(|name| {
            name.starts_with(DRAFT_ENV_PREFIX) && !DRAFT_ENV.contains(&name.as_str())
        }),
    );
    if drafting {
        names.push("LLAMA_ARG_SPEC_TYPE".into());
    }
    names
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

    /// The launch settings for this runtime's models, from the ones the settings give
    /// (`runtime.llamacpp.*` and the model's own): unchanged for runtimes sized like llama.cpp.
    fn adjust_launch(&self, launch: ModelLaunch) -> ModelLaunch {
        launch
    }

    /// Whether every request must carry `max_tokens`, because the server would otherwise
    /// generate to the end of its context (`ncm-server`). Leases then always carry one.
    fn requires_max_tokens(&self) -> bool {
        false
    }

    /// The only environment variables the server is started with, when the runtime takes an
    /// allowlist (every other one, secrets included, is dropped); `None` keeps the daemon's
    /// environment less [`scrubbed_env`].
    fn env_allowlist(&self) -> Option<&'static [&'static str]> {
        None
    }
}

/// llama.cpp, vLLM, then `ncm-server`, configured from the settings. Each takes its own
/// format, so the order decides nothing between them.
pub fn builtin(settings: &LlamaCppSettings, runtimes: Runtimes) -> Vec<Arc<dyn RuntimeBackend>> {
    vec![
        Arc::new(LlamaCpp {
            extra_args: settings.extra_args.clone(),
        }),
        Arc::new(Vllm {
            settings: runtimes.vllm,
            available: runtimes.vllm_available,
        }),
        Arc::new(Ncm {
            settings: runtimes.ncm,
            cuda: runtimes.ncm_cuda,
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
        if let Some(draft) = &spec.draft {
            // Speculative decoding: the draft proposes tokens, the model checks them
            // (llama.cpp b12180, tools/server README: `--spec-draft-model`, `--spec-type`,
            // `--spec-draft-n-max`, `--spec-draft-n-min`, `--spec-draft-ngl`). A draft model
            // does nothing without `--spec-type draft-simple`, whose default is `none`.
            args.extend([
                "--spec-draft-model".into(),
                draft.path.display().to_string(),
                "--spec-type".into(),
                "draft-simple".into(),
            ]);
            for (flag, value) in [
                ("--spec-draft-n-max", draft.max),
                ("--spec-draft-n-min", draft.min),
                ("--spec-draft-ngl", draft.gpu_layers),
            ] {
                if let Some(value) = value {
                    args.extend([flag.into(), value.to_string()]);
                }
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

/// The environment an `ncm-server` starts with ([`Ncm::env_allowlist`]): what a process needs to
/// run and find temporary and home folders, and `CUDA_PATH`, where CubeCL finds NVRTC's headers
/// (and NVRTC itself) for a CUDA build. `CUDA_VISIBLE_DEVICES` is kept so a GPU the user hid stays
/// hidden. On Unix `LD_LIBRARY_PATH`, `CUDA_HOME` and `CUDA_ROOT` are kept too, where a CUDA
/// toolkit outside the system's library path is found. Every other variable, `CUBECL_*` and
/// secrets included, is dropped.
pub const NCM_ENV: &[&str] = ncm_env(cfg!(unix));

/// [`NCM_ENV`] for a Unix (`unix`) or a Windows machine.
pub const fn ncm_env(unix: bool) -> &'static [&'static str] {
    if unix {
        &[
            "PATH",
            "TEMP",
            "TMP",
            "TMPDIR",
            "HOME",
            "LANG",
            "CUDA_PATH",
            "CUDA_VISIBLE_DEVICES",
            "LD_LIBRARY_PATH",
            "CUDA_HOME",
            "CUDA_ROOT",
        ]
    } else {
        &[
            "PATH",
            "SYSTEMROOT",
            "WINDIR",
            "TEMP",
            "TMP",
            "HOME",
            "USERPROFILE",
            "LANG",
            "CUDA_PATH",
            "CUDA_VISIBLE_DEVICES",
        ]
    }
}

/// The variables of `present` an allowlisted server keeps, compared without regard to case on
/// Windows (where `Path` is `PATH`).
pub fn allowed_env(
    allowlist: &[&str],
    present: impl IntoIterator<Item = (String, std::ffi::OsString)>,
    windows: bool,
) -> Vec<(String, std::ffi::OsString)> {
    present
        .into_iter()
        .filter(|(name, _)| {
            allowlist.iter().any(|allowed| {
                if windows {
                    allowed.eq_ignore_ascii_case(name)
                } else {
                    allowed == name
                }
            })
        })
        .collect()
}

/// `args` without anything that moves where a server listens: `--host`/`--port` (and their
/// abbreviations, [`imperium_config::moves_listener`]) with their value, and their `=` forms.
pub fn without_listener_args(args: &[String]) -> Vec<String> {
    let mut kept = Vec::with_capacity(args.len());
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if imperium_config::moves_listener(arg) {
            if !arg.contains('=') {
                skip_value = true;
            }
            tracing::warn!(
                arg,
                "dropped from the ncm-server arguments: it always listens on 127.0.0.1"
            );
            continue;
        }
        kept.push(arg.clone());
    }
    kept
}

/// `ncm-server` (Imperium-Models `models/ncm/runtime`), for NCM weight files (Princeps). It takes
/// llama-server's launch line, answers `/health` with 503 while loading and 200 when ready, and
/// serves one request at a time (`--parallel` only divides the context), so it runs with one
/// slot and calls queue in Imperium. A model goes wholly on the GPU or wholly on the CPU.
#[derive(Debug, Clone, Default)]
pub struct Ncm {
    /// `runtime.ncm.*`.
    pub settings: NcmSettings,
    /// Whether the approved `ncm-server` has CUDA ([`crate::ncm_runtime::Approval::cuda`]).
    /// Without it `device = "auto"` places every model on the CPU.
    pub cuda: bool,
}

impl RuntimeBackend for Ncm {
    fn id(&self) -> &str {
        NCM
    }

    fn accepts(&self, format: Format) -> bool {
        format == Format::NcmSafetensors
    }

    /// Always: without an approved `ncm-server` the load fails with what to do
    /// ([`crate::ncm_runtime::HOW_TO_INSTALL`]) rather than the model having no runtime.
    fn available(&self) -> bool {
        true
    }

    /// With `device = "auto"`: on the GPU when the approved build has CUDA and the model fits
    /// there, else on the CPU. `cuda` and `cpu` force one. Never split.
    fn place(&self, footprint: Footprint, budget: Budget) -> Option<Placement> {
        let gpu = budget.vram_bytes > 0 && !budget.unified;
        match self.settings.device.as_str() {
            "cpu" => place_on_cpu(footprint, budget),
            "cuda" => gpu.then(|| place_on_gpu(footprint, budget)).flatten(),
            _ => (gpu && self.cuda)
                .then(|| place_on_gpu(footprint, budget))
                .flatten()
                .or_else(|| place_on_cpu(footprint, budget)),
        }
    }

    fn place_on_cpu(&self, footprint: Footprint, budget: Budget) -> Option<Placement> {
        place_on_cpu(footprint, budget)
    }

    /// Unused: ncm-server allocates what its context needs at launch.
    fn gpu_memory_utilization(&self, _placement: &Placement, _budget: Budget) -> f64 {
        0.0
    }

    fn extra_args(&self) -> Vec<String> {
        without_listener_args(&self.settings.extra_args)
    }

    /// One slot with the whole context, `runtime.ncm.context` unless the model's own settings
    /// fix one, and nothing of llama.cpp's draft.
    fn adjust_launch(&self, mut launch: ModelLaunch) -> ModelLaunch {
        launch.slots = 1;
        launch.draft = None;
        if launch.context_auto
            && let Some(tokens) = self.settings.context.tokens()
        {
            launch.context = tokens.max(1);
            launch.context_auto = false;
        }
        if !launch.pool_fixed {
            launch.pool_tokens = launch.context;
            launch.pool_max = launch.context;
        }
        launch
    }

    fn requires_max_tokens(&self) -> bool {
        true
    }

    fn env_allowlist(&self) -> Option<&'static [&'static str]> {
        Some(NCM_ENV)
    }

    /// The model is served under its id, on `127.0.0.1` whatever the arguments say.
    fn launch_args(&self, spec: &LaunchSpec) -> Vec<String> {
        let tokenizer = spec.model_path.with_file_name("tokenizer.json");
        let on_cpu = spec.cpu_only || spec.gpu_layers == 0;
        let mut args = vec![
            "--model".into(),
            spec.model_path.display().to_string(),
            "--tokenizer".into(),
            tokenizer.display().to_string(),
            "--host".into(),
            "127.0.0.1".into(),
            "--ctx-size".into(),
            spec.pool_tokens.max(1).to_string(),
            "--parallel".into(),
            "1".into(),
            "--n-gpu-layers".into(),
            if on_cpu { "0" } else { "999" }.into(),
            "--device".into(),
            if on_cpu { "cpu" } else { "cuda" }.into(),
            "--alias".into(),
            spec.model_id.clone(),
        ];
        if self.settings.quant != "none" {
            args.extend(["--quant".into(), self.settings.quant.clone()]);
        }
        if self.settings.speculate && !on_cpu {
            args.push("--speculate".into());
        }
        args.extend(without_listener_args(&spec.extra_args));
        args
    }

    fn check_program(&self, program: &Path) -> Result<(), String> {
        if program.is_file() {
            return Ok(());
        }
        Err(crate::ncm_runtime::HOW_TO_INSTALL.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec(ncm: &Ncm, gpu_layers: u32, cpu_only: bool) -> LaunchSpec {
        LaunchSpec {
            backend: Arc::new(ncm.clone()),
            format: Format::NcmSafetensors,
            model_id: "princeps-ncm-250m".into(),
            model_path: PathBuf::from("/models/princeps/princeps-ncm-250m.safetensors"),
            mmproj: None,
            context: 32768,
            pool_tokens: 32768,
            slots: 1,
            gpu_layers,
            cpu_only,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            flash_attn: "auto".into(),
            cache_ram_mb: 0,
            slot_save_path: None,
            gpu_memory_utilization: 0.0,
            draft: None,
            extra_args: ncm.extra_args(),
        }
    }

    fn defaults() -> LlamaCppSettings {
        imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
            .settings()
            .unwrap()
            .runtime
            .llamacpp
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn ncm_is_built_in_and_serves_only_ncm_files() {
        let backends = builtin(&defaults(), Runtimes::default());
        let ids: Vec<_> = backends.iter().map(|b| b.id().to_string()).collect();
        assert_eq!(ids, [LLAMA_CPP, VLLM, NCM]);
        let serving = |format| {
            backends
                .iter()
                .filter(|b| b.accepts(format))
                .map(|b| b.id().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(serving(Format::Gguf), [LLAMA_CPP]);
        assert_eq!(serving(Format::Safetensors), [VLLM]);
        assert_eq!(serving(Format::NcmSafetensors), [NCM]);
    }

    #[test]
    fn the_launch_line_matches_the_ncm_server_contract() {
        let ncm = Ncm::default();
        // Built the way the backend builds it, so the separator matches on Windows too.
        let tokenizer = PathBuf::from("/models/princeps/princeps-ncm-250m.safetensors")
            .with_file_name("tokenizer.json")
            .display()
            .to_string();
        let model = PathBuf::from("/models/princeps/princeps-ncm-250m.safetensors")
            .display()
            .to_string();
        assert_eq!(
            ncm.launch_args(&spec(&ncm, u32::MAX, false)),
            strings(&[
                "--model",
                &model,
                "--tokenizer",
                &tokenizer,
                "--host",
                "127.0.0.1",
                "--ctx-size",
                "32768",
                "--parallel",
                "1",
                "--n-gpu-layers",
                "999",
                "--device",
                "cuda",
                "--alias",
                "princeps-ncm-250m",
            ])
        );
        let cpu = ncm.launch_args(&spec(&ncm, 0, true));
        assert!(cpu.windows(2).any(|w| w == ["--device", "cpu"]));
        assert!(cpu.windows(2).any(|w| w == ["--n-gpu-layers", "0"]));
        // Nothing of llama.cpp's own: cache types, flash attention, drafts, slots.
        for flag in [
            "--cache-type-k",
            "--flash-attn",
            "--cache-ram",
            "--slot-save-path",
            "--spec-draft-model",
            "--kv-unified",
        ] {
            assert!(!cpu.iter().any(|arg| arg == flag), "{flag}");
        }

        let tuned = Ncm {
            settings: NcmSettings {
                quant: "q4".into(),
                speculate: true,
                extra_args: strings(&["--prefill-chunk", "512"]),
                ..NcmSettings::default()
            },
            cuda: true,
        };
        let args = tuned.launch_args(&spec(&tuned, u32::MAX, false));
        assert!(args.windows(2).any(|w| w == ["--quant", "q4"]));
        assert!(args.iter().any(|arg| arg == "--speculate"));
        assert!(args.ends_with(&strings(&["--prefill-chunk", "512"])));
        // Speculation needs CUDA.
        let on_cpu = tuned.launch_args(&spec(&tuned, 0, true));
        assert!(!on_cpu.iter().any(|arg| arg == "--speculate"));
    }

    #[test]
    fn host_and_port_in_extra_args_are_dropped_at_launch() {
        let sneaky = strings(&[
            "--host",
            "0.0.0.0",
            "--port=80",
            "--ho",
            "::",
            "--PORT",
            "81",
            "--prefill-chunk",
            "512",
            "--host=0.0.0.0",
        ]);
        assert_eq!(
            without_listener_args(&sneaky),
            strings(&["--prefill-chunk", "512"])
        );
        let ncm = Ncm {
            settings: NcmSettings {
                extra_args: sneaky.clone(),
                ..NcmSettings::default()
            },
            cuda: true,
        };
        let mut launch = spec(&ncm, u32::MAX, false);
        launch.extra_args.extend(sneaky);
        let args = ncm.launch_args(&launch);
        let hosts: Vec<_> = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| arg.starts_with("--host") || arg.starts_with("--ho"))
            .map(|(at, _)| args[at + 1].as_str())
            .collect();
        assert_eq!(hosts, ["127.0.0.1"]);
        assert!(
            !args
                .iter()
                .any(|arg| arg.to_ascii_lowercase().starts_with("--po"))
        );
    }

    #[test]
    fn the_environment_is_an_allowlist_that_keeps_cuda_path() {
        let present = [
            ("Path", "C:\\Windows"),
            ("CUDA_PATH", "C:\\CUDA\\v13.0"),
            ("CUBECL_DEBUG_LOG", "1"),
            ("CUBECL_CACHE_DIR", "/tmp/x"),
            ("OPENAI_API_KEY", "sk-secret"),
            ("GITHUB_TOKEN", "ghp_secret"),
            ("TEMP", "C:\\Temp"),
            ("LLAMA_ARG_SPEC_DRAFT_MODEL", "x"),
        ]
        .map(|(name, value)| (name.to_string(), std::ffi::OsString::from(value)));
        let names = |windows| {
            allowed_env(NCM_ENV, present.clone(), windows)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(true), ["Path", "CUDA_PATH", "TEMP"]);
        assert_eq!(names(false), ["CUDA_PATH", "TEMP"]);
        assert_eq!(Ncm::default().env_allowlist(), Some(NCM_ENV));
        assert!(!NCM_ENV.iter().any(|name| name.starts_with("CUBECL")));
    }

    #[test]
    fn on_unix_the_cuda_toolkit_variables_are_kept_too() {
        let present = [
            ("LD_LIBRARY_PATH", "/usr/local/cuda/lib64"),
            ("CUDA_HOME", "/usr/local/cuda"),
            ("CUDA_ROOT", "/usr/local/cuda"),
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("CUBECL_DEBUG_LOG", "1"),
        ]
        .map(|(name, value)| (name.to_string(), std::ffi::OsString::from(value)));
        let kept: Vec<_> = allowed_env(ncm_env(true), present.clone(), false)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(kept, ["LD_LIBRARY_PATH", "CUDA_HOME", "CUDA_ROOT"]);
        assert!(allowed_env(ncm_env(false), present, true).is_empty());
    }

    #[test]
    fn a_model_is_placed_whole_on_the_gpu_or_the_cpu_never_split() {
        let footprint = Footprint {
            weights_bytes: 1 << 30,
            kv_bytes: 64 << 20,
            layers: None,
            max_gpu_layers: None,
            draft: crate::planner::DraftMemory::default(),
            overhead_bytes: 512 << 20,
        };
        let roomy = Budget {
            vram_bytes: 8 << 30,
            ram_bytes: 16 << 30,
            unified: false,
        };
        let tight = Budget {
            vram_bytes: 1 << 30,
            ..roomy
        };
        let auto = Ncm {
            cuda: true,
            ..Ncm::default()
        };
        let on_gpu = auto.place(footprint, roomy).unwrap();
        assert!(on_gpu.all_layers_on_gpu && on_gpu.ram_bytes == 0);
        // Too little VRAM: all of it on the CPU, not some layers of each.
        let on_cpu = auto.place(footprint, tight).unwrap();
        assert_eq!((on_cpu.gpu_layers, on_cpu.vram_bytes), (0, 0));
        let cuda_only = Ncm {
            settings: NcmSettings {
                device: "cuda".into(),
                ..NcmSettings::default()
            },
            cuda: true,
        };
        assert!(cuda_only.place(footprint, tight).is_none());
        let cpu_only = Ncm {
            settings: NcmSettings {
                device: "cpu".into(),
                ..NcmSettings::default()
            },
            cuda: true,
        };
        assert_eq!(cpu_only.place(footprint, roomy).unwrap().gpu_layers, 0);
        assert!(auto.place_on_cpu(footprint, roomy).is_some());
        // A build without CUDA runs on the CPU under `auto`, whatever the GPU has free.
        let cpu_build = Ncm::default();
        assert_eq!(cpu_build.place(footprint, roomy).unwrap().gpu_layers, 0);
    }

    #[test]
    fn launches_get_one_slot_and_the_ncm_context() {
        let ncm = Ncm::default();
        let launch = defaults().default_launch();
        let adjusted = ncm.adjust_launch(launch.clone());
        assert_eq!(adjusted.slots, 1);
        assert_eq!(adjusted.context, imperium_config::NCM_DEFAULT_CONTEXT);
        assert_eq!(adjusted.pool_tokens, imperium_config::NCM_DEFAULT_CONTEXT);
        assert!(adjusted.draft.is_none());
        // A context the model's own settings fix is kept.
        let own = ModelLaunch {
            context: 8192,
            context_auto: false,
            ..launch
        };
        assert_eq!(ncm.adjust_launch(own).context, 8192);
        assert!(ncm.requires_max_tokens());
        assert!(!LlamaCpp::default().requires_max_tokens());
    }

    #[test]
    fn a_missing_server_says_how_to_build_and_approve_it() {
        let error = Ncm::default()
            .check_program(Path::new("/nowhere/ncm-server"))
            .unwrap_err();
        assert!(
            error.contains("cargo build --release -p ncm-server"),
            "{error}"
        );
        assert!(error.contains("imperium runtime ncm use"), "{error}");
    }
}
