//! Decides whether a model fits in the memory that is free, and how to split it between GPU and
//! system memory:
//!
//! ```text
//! required = weights + kv_bytes_per_token × context × slots + runtime_overhead
//! ```

use crate::scan::LocalModel;

/// Compute buffers and runtime state llama.cpp needs beyond weights and KV cache.
pub const RUNTIME_OVERHEAD_BYTES: u64 = 512 * 1024 * 1024;
/// Assumed when a header lacks what the KV estimate needs: a fifth of the weights.
const UNKNOWN_KV_FRACTION: f64 = 0.2;

/// Memory available to new models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Budget {
    pub vram_bytes: u64,
    pub ram_bytes: u64,
    /// GPU and CPU share `ram_bytes`.
    pub unified: bool,
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
}

impl Footprint {
    pub fn of(model: &LocalModel, context: u32, slots: u32, cache_type: &str) -> Self {
        let tokens = u64::from(context) * u64::from(slots.max(1));
        let kv_bytes = model
            .info
            .kv_bytes_per_token(cache_element_bytes(cache_type))
            .map_or(
                (model.size_bytes as f64 * UNKNOWN_KV_FRACTION) as u64,
                |per_token| per_token * tokens,
            );
        Footprint {
            weights_bytes: model.size_bytes,
            kv_bytes,
            layers: model
                .info
                .block_count
                .and_then(|layers| u32::try_from(layers).ok()),
        }
    }

    pub fn required_bytes(&self) -> u64 {
        self.weights_bytes + self.kv_bytes + RUNTIME_OVERHEAD_BYTES
    }
}

/// Bytes per KV cache element for llama.cpp's `--cache-type-k/v` values.
pub fn cache_element_bytes(cache_type: &str) -> f64 {
    match cache_type {
        "f32" => 4.0,
        "q8_0" => 34.0 / 32.0,
        "q5_0" | "q5_1" => 22.0 / 32.0,
        "q4_0" | "q4_1" | "iq4_nl" => 18.0 / 32.0,
        _ => 2.0,
    }
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
    if required <= budget.vram_bytes {
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
        .min(layers - 1);
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

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A 32-layer model with 4 GiB of weights and 1 GiB of KV cache.
    fn footprint() -> Footprint {
        Footprint {
            weights_bytes: 4 * GIB,
            kv_bytes: GIB,
            layers: Some(32),
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
    fn cache_types_scale_the_kv_cache() {
        assert_eq!(cache_element_bytes("f16"), 2.0);
        assert!(cache_element_bytes("q8_0") < 1.1);
    }
}
