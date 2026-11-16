//! Speculative decoding with a draft model: a small model proposes a few tokens at a time and
//! the main model checks them in one pass, which speeds up generation without changing what is
//! generated (llama.cpp `--spec-draft-model` with `--spec-type draft-simple`, b12180).
//!
//! A model's `draft_model` setting names the draft. Whether it can be used is decided every time
//! the model loads ([`resolve`]): the draft must be installed, a GGUF smaller than the model with
//! a compatible tokenizer, and fit in memory beside it. When it cannot be used, the model loads
//! without it and the reason is kept as a notice ([`Drafting::Skipped`]).

use imperium_config::{DraftLaunch, ModelLaunch};

use crate::gguf::GgufInfo;
use crate::planner::{DraftMemory, UNKNOWN_KV_FRACTION, cache_element_bytes};
use crate::scan::{Format, LocalModel};

/// Compute buffers and runtime state the draft context needs beyond its weights and KV cache.
pub const DRAFT_OVERHEAD_BYTES: u64 = 128 * 1024 * 1024;
/// Most entries the two vocabularies may differ by. llama.cpp checks the draft's vocabulary
/// against the model's the same way and refuses a draft beyond this; checking it here lets the
/// model load without the draft instead of failing to start.
pub const MAX_VOCAB_DIFFERENCE: u64 = 128;

/// A draft model that will be passed to the server.
#[derive(Debug, Clone)]
pub struct Draft {
    pub model: LocalModel,
    pub max: Option<u32>,
    pub min: Option<u32>,
    pub gpu_layers: Option<u32>,
}

impl Draft {
    /// Bytes of KV cache the draft holds per token of the pool (its context follows the
    /// model's, at llama.cpp's default `f16` cache); 0 when its header lacks the shape.
    pub fn kv_bytes_per_token(&self) -> u64 {
        self.model
            .info
            .kv_bytes_per_token(cache_element_bytes("f16"))
            .unwrap_or(0)
    }

    /// Memory the draft holds whatever the pool: weights, compute buffers, and a fifth of the
    /// weights as KV cache when its header lacks the shape.
    pub fn fixed_bytes(&self) -> u64 {
        let guessed_kv = if self.kv_bytes_per_token() == 0 {
            (self.model.size_bytes as f64 * UNKNOWN_KV_FRACTION) as u64
        } else {
            0
        };
        self.model.size_bytes + DRAFT_OVERHEAD_BYTES + guessed_kv
    }

    /// Memory the draft takes beside a model with a pool of `pool_tokens`, and the part of it in
    /// RAM: all of it when the model is on the CPU lane (`--device none` reaches the draft too)
    /// or `draft_gpu_layers` is 0; with a smaller `draft_gpu_layers` than the draft has layers,
    /// the share of its weights and KV cache for the layers left off the GPU (its buffers stay on
    /// the GPU); otherwise none. Unset `draft_gpu_layers` is llama.cpp's `auto`: every layer.
    pub fn memory(&self, pool_tokens: u32, main_on_cpu: bool) -> DraftMemory {
        let bytes = self.fixed_bytes() + self.kv_bytes_per_token() * u64::from(pool_tokens);
        let layered = bytes - DRAFT_OVERHEAD_BYTES;
        let ram_bytes = if main_on_cpu || self.gpu_layers == Some(0) {
            bytes
        } else {
            match (self.gpu_layers, self.model.info.block_count) {
                (Some(on_gpu), Some(layers)) if layers > 0 && u64::from(on_gpu) < layers => {
                    layered * (layers - u64::from(on_gpu)) / layers
                }
                _ => 0,
            }
        };
        DraftMemory { bytes, ram_bytes }
    }
}

/// What a model's draft setting comes to when it loads.
#[derive(Debug, Clone)]
pub enum Drafting {
    /// No draft is configured.
    Off,
    /// The draft is used.
    On(Box<Draft>),
    /// A draft is configured but cannot be used; the model loads without it. The text says why,
    /// for the notice.
    Skipped(String),
}

impl Drafting {
    /// The draft used, if any.
    pub fn draft(&self) -> Option<&Draft> {
        match self {
            Drafting::On(draft) => Some(draft.as_ref()),
            _ => None,
        }
    }

    /// The reason a configured draft is not used, if so.
    pub fn notice(&self) -> Option<&str> {
        match self {
            Drafting::Skipped(reason) => Some(reason),
            _ => None,
        }
    }
}

/// The text of the notice for a draft `model` that is not used, because `reason`.
pub fn skipped(draft_id: &str, reason: &str) -> String {
    format!("Draft model {draft_id} is not used: {reason}. The model runs without it.")
}

/// Whether `draft`'s tokenizer can draft for `main`: the same tokenizer family and vocabularies
/// that differ by at most [`MAX_VOCAB_DIFFERENCE`] entries. A header without the fields cannot
/// be compared and is not held against the draft.
pub fn compatible(main: &GgufInfo, draft: &GgufInfo) -> Result<(), String> {
    if let (Some(a), Some(b)) = (&main.tokenizer_model, &draft.tokenizer_model)
        && a != b
    {
        return Err(format!("its tokenizer ({b}) is not the model's ({a})"));
    }
    if let (Some(a), Some(b)) = (main.vocab_size, draft.vocab_size)
        && a.abs_diff(b) > MAX_VOCAB_DIFFERENCE
    {
        return Err(format!(
            "its vocabulary ({b} tokens) differs from the model's ({a} tokens) by more than \
             {MAX_VOCAB_DIFFERENCE}"
        ));
    }
    Ok(())
}

/// Decides what `launch.draft` comes to for `main`, among the `installed` models, before memory
/// is considered.
pub fn resolve(main: &LocalModel, launch: &ModelLaunch, installed: &[LocalModel]) -> Drafting {
    let Some(DraftLaunch {
        model: id,
        max,
        min,
        gpu_layers,
    }) = &launch.draft
    else {
        return Drafting::Off;
    };
    let skip = |reason: &str| Drafting::Skipped(skipped(id, reason));
    if main.format != Format::Gguf {
        return skip("only GGUF models served by llama.cpp can use a draft");
    }
    if *id == main.id {
        return skip("a model cannot draft for itself");
    }
    let Some(draft) = installed.iter().find(|m| m.id == *id) else {
        return skip("it is not an installed model (check models.folders)");
    };
    if draft.format != Format::Gguf {
        return skip("it is not a GGUF model");
    }
    if draft.size_bytes >= main.size_bytes {
        return skip("it is not smaller than the model, so it would not speed anything up");
    }
    if let Err(reason) = compatible(&main.info, &draft.info) {
        return skip(&reason);
    }
    Drafting::On(Box::new(Draft {
        model: draft.clone(),
        max: *max,
        min: *min,
        gpu_layers: *gpu_layers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn model(id: &str, gib: u64, tokenizer: &str, vocab: u64) -> LocalModel {
        LocalModel {
            id: id.into(),
            path: PathBuf::from(format!("{id}.gguf")),
            parts: 1,
            size_bytes: gib * GIB,
            info: GgufInfo {
                block_count: Some(28),
                embedding_length: Some(1024),
                head_count: Some(16),
                head_count_kv: Some(8),
                tokenizer_model: Some(tokenizer.into()),
                vocab_size: Some(vocab),
                ..GgufInfo::default()
            },
            mmproj: None,
            repo: None,
            format: Format::Gguf,
        }
    }

    fn launch(draft: Option<&str>) -> ModelLaunch {
        ModelLaunch {
            context: 4096,
            context_auto: false,
            pool_tokens: 16_384,
            pool_fixed: false,
            pool_max: 16_384,
            pool_clamped: false,
            slots: 2,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            flash_attn: "auto".into(),
            max_gpu_layers: None,
            max_wait_s: 600,
            max_output_tokens: 8192,
            extra_args: Vec::new(),
            draft: draft.map(|id| DraftLaunch {
                model: id.into(),
                max: Some(8),
                min: None,
                gpu_layers: None,
            }),
        }
    }

    #[test]
    fn a_compatible_smaller_draft_is_used_with_its_parameters() {
        let main = model("big", 8, "gpt2", 151_936);
        let installed = [main.clone(), model("small", 1, "gpt2", 151_936)];
        let drafting = resolve(&main, &launch(Some("small")), &installed);
        let draft = drafting.draft().expect("used");
        assert_eq!(
            (draft.model.id.as_str(), draft.max, draft.min),
            ("small", Some(8), None)
        );
        assert!(matches!(
            resolve(&main, &launch(None), &installed),
            Drafting::Off
        ));
    }

    #[test]
    fn a_missing_incompatible_or_unhelpful_draft_is_skipped_with_a_reason() {
        let main = model("big", 8, "gpt2", 151_936);
        let installed = [
            main.clone(),
            model("other-family", 1, "llama", 151_936),
            model("other-vocab", 1, "gpt2", 32_000),
            model("near-vocab", 1, "gpt2", 151_936 - MAX_VOCAB_DIFFERENCE),
            model("bigger", 9, "gpt2", 151_936),
        ];
        let reason = |id: &str| {
            resolve(&main, &launch(Some(id)), &installed)
                .notice()
                .map(str::to_string)
        };
        assert!(reason("absent").unwrap().contains("not an installed model"));
        assert!(reason("other-family").unwrap().contains("tokenizer"));
        assert!(reason("other-vocab").unwrap().contains("vocabulary"));
        assert!(reason("bigger").unwrap().contains("not smaller"));
        assert!(reason("big").unwrap().contains("itself"));
        // Every notice says the model runs without the draft.
        assert!(reason("absent").unwrap().ends_with("runs without it."));
        // A vocabulary within the allowed difference is fine.
        assert_eq!(reason("near-vocab"), None);
    }

    #[test]
    fn headers_without_tokenizer_fields_are_not_held_against_the_draft() {
        let mut main = model("big", 8, "gpt2", 100);
        let mut small = model("small", 1, "gpt2", 100);
        main.info.tokenizer_model = None;
        small.info.vocab_size = None;
        assert!(compatible(&main.info, &small.info).is_ok());
    }

    #[test]
    fn the_draft_memory_grows_with_the_pool() {
        let draft = Draft {
            model: model("small", 1, "gpt2", 1),
            max: None,
            min: None,
            gpu_layers: None,
        };
        // 28 layers x 8 KV heads x (64 + 64) x 2 bytes = 56 KiB a token.
        assert_eq!(draft.kv_bytes_per_token(), 28 * 8 * 128 * 2);
        let small = draft.memory(1_000, false);
        let large = draft.memory(2_000, false);
        assert_eq!(
            large.bytes - small.bytes,
            1_000 * draft.kv_bytes_per_token()
        );
        assert_eq!(
            small.bytes,
            GIB + DRAFT_OVERHEAD_BYTES + 1_000 * 28 * 8 * 128 * 2
        );
        assert_eq!(small.ram_bytes, 0, "unset layers: all on the GPU");
        let lane = draft.memory(1_000, true);
        assert_eq!(
            lane.ram_bytes, lane.bytes,
            "a CPU-lane model drafts on the CPU"
        );
        let pinned = Draft {
            gpu_layers: Some(0),
            ..draft.clone()
        };
        let memory = pinned.memory(1_000, false);
        assert_eq!(memory.ram_bytes, memory.bytes);
        // 7 of the draft's 28 layers on the GPU: three quarters of weights and KV in RAM.
        let partial = Draft {
            gpu_layers: Some(7),
            ..draft.clone()
        };
        let memory = partial.memory(1_000, false);
        assert_eq!(
            memory.ram_bytes,
            (memory.bytes - DRAFT_OVERHEAD_BYTES) * 21 / 28
        );
        let all = Draft {
            gpu_layers: Some(99),
            ..draft
        };
        assert_eq!(all.memory(1_000, false).ram_bytes, 0);
    }
}
