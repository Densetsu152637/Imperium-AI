//! Decides whether a model fits in the memory that is free, and how to split it between GPU and
//! system memory:
//!
//! ```text
//! required = weights + kv_bytes_per_token × pool_tokens + runtime_overhead
//! ```

use imperium_config::ModelLaunch;

use crate::hardware::Hardware;
use crate::scan::LocalModel;

/// Compute buffers and runtime state llama.cpp needs beyond weights and KV cache.
pub const RUNTIME_OVERHEAD_BYTES: u64 = 512 * 1024 * 1024;
/// Assumed when a header lacks what the KV estimate needs: a fifth of the weights.
const UNKNOWN_KV_FRACTION: f64 = 0.2;
/// Layers assumed for a model whose header has not been read (one not downloaded yet), so a
/// GPU and RAM split can still be estimated.
const ASSUMED_LAYERS: u32 = 32;

/// Memory available to new models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Budget {
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    /// GPU and CPU share `ram_bytes`.
    pub unified: bool,
}

impl Budget {
    /// Memory free on `hardware`, keeping `ram_headroom_bytes` of RAM for everything else.
    pub fn of(hardware: &Hardware, ram_headroom_bytes: u64) -> Self {
        Budget {
            vram_bytes: hardware.vram_free_bytes(),
            ram_bytes: hardware
                .ram_available_bytes
                .saturating_sub(ram_headroom_bytes),
            unified: hardware.unified_memory,
        }
    }
}

/// How a model would be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Layers offloaded to the GPU (`-ngl`); every layer when it fits entirely.
    pub gpu_layers: u32,
    pub all_layers_on_gpu: bool,
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    pub required_bytes: u64,
}

/// What loading a model needs at a given context and slot count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footprint {
    pub weights_bytes: u64,
    pub kv_bytes: u64,
    pub layers: Option<u32>,
    /// Most layers the settings let the GPU take.
    pub max_gpu_layers: Option<u32>,
}

impl Footprint {
    /// What `model` needs started with `launch`: the KV cache is one pool of `pool_tokens`,
    /// whatever the slots.
    pub fn of(model: &LocalModel, launch: &ModelLaunch) -> Self {
        let kv_bytes = model
            .info
            .kv_bytes_per_token_kv(
                cache_element_bytes(&launch.cache_type_k),
                cache_element_bytes(&launch.cache_type_v),
            )
            .map_or(
                (model.size_bytes as f64 * UNKNOWN_KV_FRACTION) as u64,
                |per_token| per_token * u64::from(launch.pool_tokens),
            );
        Footprint {
            weights_bytes: model.size_bytes,
            kv_bytes,
            layers: model
                .info
                .block_count
                .and_then(|layers| u32::try_from(layers).ok()),
            max_gpu_layers: launch.max_gpu_layers,
        }
    }

    /// An estimate from the weights' size alone, for a model not on disk yet: the KV cache as
    /// for a header without the model's shape, and an assumed layer count.
    pub fn estimate(weights_bytes: u64) -> Self {
        Footprint {
            weights_bytes,
            kv_bytes: (weights_bytes as f64 * UNKNOWN_KV_FRACTION) as u64,
            layers: Some(ASSUMED_LAYERS),
            max_gpu_layers: None,
        }
    }

    pub fn required_bytes(&self) -> u64 {
        self.weights_bytes + self.kv_bytes + RUNTIME_OVERHEAD_BYTES
    }
}

/// MiB of host memory for llama.cpp's prompt cache (`--cache-ram`): what the setting asks for,
/// capped to the RAM left in `budget` after the model at `placement`.
pub fn prompt_cache_mb(requested_mb: u64, placement: &Placement, budget: Budget) -> u64 {
    let free_mb = budget.ram_bytes.saturating_sub(placement.ram_bytes) / (1024 * 1024);
    requested_mb.min(free_mb)
}

/// Bytes per KV cache element for llama.cpp's `--cache-type-k/v` values.
pub fn cache_element_bytes(cache_type: &str) -> f64 {
    match cache_type {
        "f32" => 4.0,
        "bf16" => 2.0,
        "q8_0" => 34.0 / 32.0,
        "q5_0" | "q5_1" => 22.0 / 32.0,
        "q4_0" | "q4_1" | "iq4_nl" => 18.0 / 32.0,
        _ => 2.0,
    }
}

/// Places a model on the GPU only (vLLM cannot offload to system memory).
pub fn place_on_gpu(footprint: Footprint, budget: Budget) -> Option<Placement> {
    let required = footprint.required_bytes();
    let room = if budget.unified {
        budget.ram_bytes
    } else {
        budget.vram_bytes
    };
    (required <= room).then_some(Placement {
        gpu_layers: u32::MAX,
        all_layers_on_gpu: true,
        vram_bytes: if budget.unified { 0 } else { required },
        ram_bytes: if budget.unified { required } else { 0 },
        required_bytes: required,
    })
}

/// Places a model on the CPU lane (`placement = "cpu"`): no GPU layers, and its weights, KV
/// cache and runtime state all in system RAM, even when VRAM is free. `None` when the RAM left
/// in `budget` cannot hold it.
pub fn place_on_cpu(footprint: Footprint, budget: Budget) -> Option<Placement> {
    let required = footprint.required_bytes();
    (required <= budget.ram_bytes).then_some(Placement {
        gpu_layers: 0,
        all_layers_on_gpu: false,
        vram_bytes: 0,
        ram_bytes: required,
        required_bytes: required,
    })
}

/// Places a model: entirely on the GPU if it fits, otherwise as many layers as fit with the rest
/// in system memory, otherwise `None`.
pub fn place(footprint: Footprint, budget: Budget) -> Option<Placement> {
    let required = footprint.required_bytes();
    if budget.unified || budget.vram_bytes == 0 {
        return (required <= budget.ram_bytes).then_some(Placement {
            gpu_layers: if budget.unified { u32::MAX } else { 0 },
            all_layers_on_gpu: budget.unified,
            vram_bytes: 0,
            ram_bytes: required,
            required_bytes: required,
        });
    }
    let capped = match (footprint.max_gpu_layers, footprint.layers) {
        (Some(cap), Some(layers)) => cap < layers,
        _ => false,
    };
    if required <= budget.vram_bytes && !capped {
        return Some(Placement {
            gpu_layers: u32::MAX,
            all_layers_on_gpu: true,
            vram_bytes: required,
            ram_bytes: 0,
            required_bytes: required,
        });
    }
    // Partial offload needs a layer count to split by.
    let layers = footprint.layers?.max(1);
    let per_layer = (footprint.weights_bytes + footprint.kv_bytes).div_ceil(u64::from(layers));
    let gpu_room = budget.vram_bytes.saturating_sub(RUNTIME_OVERHEAD_BYTES);
    let gpu_layers = u32::try_from(gpu_room / per_layer.max(1))
        .unwrap_or(u32::MAX)
        .min(layers - 1)
        .min(footprint.max_gpu_layers.unwrap_or(u32::MAX));
    let vram = if gpu_layers == 0 {
        0
    } else {
        u64::from(gpu_layers) * per_layer + RUNTIME_OVERHEAD_BYTES
    };
    let ram = u64::from(layers - gpu_layers) * per_layer
        + if gpu_layers == 0 {
            RUNTIME_OVERHEAD_BYTES
        } else {
            0
        };
    (ram <= budget.ram_bytes).then_some(Placement {
        gpu_layers,
        all_layers_on_gpu: false,
        vram_bytes: vram,
        ram_bytes: ram,
        required_bytes: required,
    })
}

/// Whether a model fits in memory now, for agents choosing a model (`list_models`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Loaded already.
    Loaded,
    /// Fits beside the loaded models.
    Fits,
    /// Fits once loaded models nobody is using are unloaded (a swap).
    AfterUnloadingIdle,
    /// Does not fit while the models in use stay loaded, or not at all on this machine.
    DoesNotFit,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Loaded => "loaded",
            Fit::Fits => "fits_now",
            Fit::AfterUnloadingIdle => "fits_after_unloading_idle",
            Fit::DoesNotFit => "does_not_fit",
        }
    }
}

/// Memory a loaded model holds, and whether it is idle: no call on it or waiting for it, and
/// no agent mid-turn on it, so it may be unloaded for another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    pub idle: bool,
}

/// Whether a model that is not loaded fits beside the loaded models in `held` (the other
/// models), or once the idle ones are unloaded, with `place` the model's placer. Returns the
/// placement it would get, when it fits. Nothing is loaded or unloaded.
pub fn fit_beside(
    held: &[Held],
    budget: Budget,
    place: impl Fn(Budget) -> Option<Placement>,
) -> (Fit, Option<Placement>) {
    let left = |models: &mut dyn Iterator<Item = &Held>| {
        let (vram, ram) = models.fold((0, 0), |(vram, ram), held| {
            (vram + held.vram_bytes, ram + held.ram_bytes)
        });
        Budget {
            vram_bytes: budget.vram_bytes.saturating_sub(vram),
            ram_bytes: budget.ram_bytes.saturating_sub(ram),
            unified: budget.unified,
        }
    };
    if let Some(placement) = place(left(&mut held.iter())) {
        return (Fit::Fits, Some(placement));
    }
    if held.iter().any(|held| held.idle)
        && let Some(placement) = place(left(&mut held.iter().filter(|held| !held.idle)))
    {
        return (Fit::AfterUnloadingIdle, Some(placement));
    }
    (Fit::DoesNotFit, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn fit_beside_counts_what_is_loaded_and_what_could_be_unloaded() {
        let budget = Budget {
            vram_bytes: 8 * GIB,
            ram_bytes: 16 * GIB,
            unified: false,
        };
        // Needs 5 GiB of VRAM and nothing else.
        let place = |left: Budget| {
            (left.vram_bytes >= 5 * GIB).then_some(Placement {
                gpu_layers: u32::MAX,
                all_layers_on_gpu: true,
                vram_bytes: 5 * GIB,
                ram_bytes: 0,
                required_bytes: 5 * GIB,
            })
        };
        let held = |gib: u64, idle: bool| Held {
            vram_bytes: gib * GIB,
            ram_bytes: 0,
            idle,
        };
        assert_eq!(fit_beside(&[], budget, place).0, Fit::Fits);
        assert_eq!(fit_beside(&[held(2, false)], budget, place).0, Fit::Fits);
        // 4 GiB held: fits only once the idle model goes.
        let (fit, placement) = fit_beside(&[held(4, true)], budget, place);
        assert_eq!(fit, Fit::AfterUnloadingIdle);
        assert!(placement.is_some_and(|p| p.all_layers_on_gpu));
        assert_eq!(
            fit_beside(&[held(2, true), held(2, false)], budget, place).0,
            Fit::AfterUnloadingIdle
        );
        // A model in use is never counted as free.
        assert_eq!(
            fit_beside(&[held(4, false)], budget, place),
            (Fit::DoesNotFit, None)
        );
        assert_eq!(
            fit_beside(&[held(2, true), held(4, false)], budget, place).0,
            Fit::DoesNotFit
        );
        // Too large for the machine at all.
        let never = |_: Budget| None;
        assert_eq!(fit_beside(&[], budget, never).0, Fit::DoesNotFit);
        assert_eq!(
            Fit::AfterUnloadingIdle.as_str(),
            "fits_after_unloading_idle"
        );
    }

    /// A 32-layer model with 4 GiB of weights and 1 GiB of KV cache.
    fn footprint() -> Footprint {
        Footprint {
            weights_bytes: 4 * GIB,
            kv_bytes: GIB,
            layers: Some(32),
            max_gpu_layers: None,
        }
    }

    #[test]
    fn whole_models_go_to_the_gpu_when_they_fit() {
        let budget = Budget {
            vram_bytes: 8 * GIB,
            ram_bytes: 16 * GIB,
            unified: false,
        };
        let placement = place(footprint(), budget).unwrap();
        assert!(placement.all_layers_on_gpu);
        assert_eq!(placement.vram_bytes, 5 * GIB + RUNTIME_OVERHEAD_BYTES);
        assert_eq!(placement.ram_bytes, 0);
    }

    #[test]
    fn models_split_across_gpu_and_ram_when_vram_is_short() {
        let budget = Budget {
            vram_bytes: 3 * GIB,
            ram_bytes: 16 * GIB,
            unified: false,
        };
        let placement = place(footprint(), budget).unwrap();
        // 5 GiB over 32 layers = 160 MiB a layer; 2.5 GiB of room holds 16 layers.
        assert_eq!(placement.gpu_layers, 16);
        assert!(placement.vram_bytes <= 3 * GIB);
        assert_eq!(placement.ram_bytes, 16 * 160 * 1024 * 1024);
    }

    #[test]
    fn nothing_is_placed_without_room() {
        let tight = Budget {
            vram_bytes: 2 * GIB,
            ram_bytes: GIB,
            unified: false,
        };
        assert_eq!(place(footprint(), tight), None);
        let cpu_only = Budget {
            vram_bytes: 0,
            ram_bytes: 6 * GIB,
            unified: false,
        };
        assert_eq!(place(footprint(), cpu_only).unwrap().gpu_layers, 0);
        let unified = Budget {
            unified: true,
            ..cpu_only
        };
        assert!(place(footprint(), unified).unwrap().all_layers_on_gpu);
    }

    #[test]
    fn cpu_lane_placement_takes_only_ram_even_when_vram_is_free() {
        let budget = Budget {
            vram_bytes: 24 * GIB,
            ram_bytes: 16 * GIB,
            unified: false,
        };
        let placement = place_on_cpu(footprint(), budget).unwrap();
        assert_eq!(placement.gpu_layers, 0);
        assert!(!placement.all_layers_on_gpu);
        assert_eq!(placement.vram_bytes, 0);
        assert_eq!(placement.ram_bytes, 5 * GIB + RUNTIME_OVERHEAD_BYTES);
        // The ordinary planner would have put the same model on the GPU.
        assert!(place(footprint(), budget).unwrap().all_layers_on_gpu);
        // It does not spill to VRAM: with the RAM short it is not placed at all.
        let short = Budget {
            ram_bytes: 5 * GIB,
            ..budget
        };
        assert_eq!(place_on_cpu(footprint(), short), None);
        // Unified memory is one pool of RAM, and the model is still served on the CPU.
        let unified = Budget {
            unified: true,
            ..budget
        };
        assert_eq!(place_on_cpu(footprint(), unified).unwrap().gpu_layers, 0);
    }

    #[test]
    fn gpu_only_placement_never_spills_to_ram() {
        let short = Budget {
            vram_bytes: 3 * GIB,
            ram_bytes: 64 * GIB,
            unified: false,
        };
        assert_eq!(place_on_gpu(footprint(), short), None);
        let roomy = Budget {
            vram_bytes: 8 * GIB,
            ..short
        };
        assert!(place_on_gpu(footprint(), roomy).unwrap().all_layers_on_gpu);
    }

    #[test]
    fn estimates_from_file_size_can_still_split() {
        let estimate = Footprint::estimate(10 * GIB);
        assert_eq!(estimate.kv_bytes, 2 * GIB);
        let budget = Budget {
            vram_bytes: 6 * GIB,
            ram_bytes: 32 * GIB,
            unified: false,
        };
        let placement = place(estimate, budget).unwrap();
        assert!(placement.gpu_layers > 0 && !placement.all_layers_on_gpu);
    }

    #[test]
    fn the_prompt_cache_takes_what_the_setting_asks_within_the_ram_left() {
        let budget = Budget {
            vram_bytes: 8 * GIB,
            ram_bytes: 10 * GIB,
            unified: false,
        };
        let on_gpu = place(footprint(), budget).unwrap();
        assert_eq!(prompt_cache_mb(8192, &on_gpu, budget), 8192);
        assert_eq!(prompt_cache_mb(0, &on_gpu, budget), 0);
        // A model that spills into RAM leaves less: 10 GiB less 4 GiB of the model.
        let split = Placement {
            ram_bytes: 4 * GIB,
            ..on_gpu
        };
        assert_eq!(prompt_cache_mb(8192, &split, budget), 6144);
        let full = Placement {
            ram_bytes: 12 * GIB,
            ..on_gpu
        };
        assert_eq!(prompt_cache_mb(8192, &full, budget), 0);
    }

    fn kv_model() -> LocalModel {
        LocalModel {
            id: "m".into(),
            path: std::path::PathBuf::from("m.gguf"),
            parts: 1,
            size_bytes: 4 * GIB,
            info: crate::gguf::GgufInfo {
                block_count: Some(32),
                embedding_length: Some(4096),
                head_count: Some(32),
                head_count_kv: Some(8),
                ..crate::gguf::GgufInfo::default()
            },
            mmproj: None,
            repo: None,
            format: crate::scan::Format::Gguf,
        }
    }

    fn launch(slots: u32, pool_tokens: u32, k: &str, v: &str) -> ModelLaunch {
        ModelLaunch {
            context: 4096,
            context_auto: false,
            pool_tokens,
            pool_fixed: true,
            pool_clamped: false,
            pool_max: pool_tokens,
            slots,
            cache_type_k: k.into(),
            cache_type_v: v.into(),
            flash_attn: "auto".into(),
            max_gpu_layers: None,
            max_wait_s: 600,
            max_output_tokens: 8192,
            extra_args: Vec::new(),
        }
    }

    #[test]
    fn the_kv_cache_is_one_pool_whatever_the_slots() {
        let model = kv_model();
        let kv = |slots, pool, k, v| Footprint::of(&model, &launch(slots, pool, k, v)).kv_bytes;
        // 32 layers x 8 KV heads x (128 + 128) x 2 bytes = 128 KiB a token.
        assert_eq!(kv(1, 16384, "f16", "f16"), 16384 * 131_072);
        assert_eq!(kv(1, 16384, "f16", "f16"), kv(16, 16384, "f16", "f16"));
        assert_eq!(kv(4, 32768, "f16", "f16"), 2 * kv(4, 16384, "f16", "f16"));
        // K at q8_0 with V at f16 lies between the two uniform types.
        let mixed = kv(4, 16384, "q8_0", "f16");
        assert!(kv(4, 16384, "q8_0", "q8_0") < mixed && mixed < kv(4, 16384, "f16", "f16"));
    }

    #[test]
    fn a_gpu_layer_cap_leaves_the_rest_in_ram() {
        let budget = Budget {
            vram_bytes: 24 * GIB,
            ram_bytes: 16 * GIB,
            unified: false,
        };
        let capped = Footprint {
            max_gpu_layers: Some(8),
            ..footprint()
        };
        let placement = place(capped, budget).unwrap();
        assert_eq!(placement.gpu_layers, 8);
        assert!(!placement.all_layers_on_gpu);
        // 5 GiB over 32 layers = 160 MiB a layer; the 24 layers left stay in RAM.
        assert_eq!(placement.ram_bytes, 24 * 160 * 1024 * 1024);
    }

    #[test]
    fn cache_types_scale_the_kv_cache() {
        assert_eq!(cache_element_bytes("f16"), 2.0);
        assert!(cache_element_bytes("q8_0") < 1.1);
    }
}
