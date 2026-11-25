//! Model download suggestions: deciding when a session's best available model is poor for what
//! it needs, and which repository and quantisation from the curated list
//! (`[[models.suggestions]]`) to offer, sized to this machine. Both are pure functions over
//! what the caller gathered (local models, routing, the Hub's listings), so they are
//! deterministic and tested without hardware or a network.
//!
//! The rules are documented in docs/04-models-and-routing.md#model-download-suggestions.

use imperium_config::SuggestionCandidate;

use crate::hf::{RepoFile, Variant, Weights, projectors, variants};
use crate::planner::{Budget, Footprint, place, place_on_gpu};

/// What a session lacks, which decides the kind of model suggested. At most one suggestion is
/// made per need per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Need {
    /// A general local model: there is none, or routing falls back to a cloud model although
    /// local models are preferred.
    Local,
    /// Images: no local model has a vision projector.
    Vision,
    /// Reliable tool calling (asked for by the agent).
    Tools,
    /// Code (asked for by the agent).
    Code,
    /// More context than any local model reads.
    Context,
}

impl Need {
    pub fn as_str(self) -> &'static str {
        match self {
            Need::Local => "local",
            Need::Vision => "vision",
            Need::Tools => "tools",
            Need::Code => "code",
            Need::Context => "context",
        }
    }

    /// Reads a need as the `suggest_local_model` tool names it (`chat` is a general model).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "local" | "chat" | "general" => Some(Need::Local),
            "vision" | "image" | "images" => Some(Need::Vision),
            "tools" | "tool_use" | "tool-use" => Some(Need::Tools),
            "code" | "coding" => Some(Need::Code),
            "context" | "long_context" | "long-context" => Some(Need::Context),
            _ => None,
        }
    }

    /// The capability a candidate must list to meet the need.
    fn capability(self) -> &'static str {
        match self {
            Need::Local | Need::Context => "chat",
            Need::Vision => "vision",
            Need::Tools => "tools",
            Need::Code => "code",
        }
    }
}

/// What a local model on disk can do, as far as suggestions care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalCapability {
    /// Its trained context length, when the header gives one.
    pub context: Option<u64>,
    /// It has a multimodal projector beside it.
    pub vision: bool,
}

/// What the caller knows about a session at the moment a model is needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Signals<'a> {
    /// `routing.prefer = "local"`.
    pub prefer_local: bool,
    /// The session's own model runs on this machine.
    pub session_local: bool,
    /// Routing an ordinary task (default tier, no tags) picks a local model.
    pub routed_local: bool,
    /// Every local model found in the model folders.
    pub local: &'a [LocalCapability],
    /// The user's message, when a turn starts.
    pub text: Option<&'a str>,
    /// The prompt size of the model call just made, when known.
    pub prompt_tokens: Option<u64>,
}

/// A need and why it was detected, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub need: Need,
    pub reason: String,
    /// For [`Need::Context`]: the tokens the model must read.
    pub context_tokens: Option<u64>,
}

/// Decides whether a session needs a better local model, checking in this order:
///
/// 1. Nothing is suggested unless local models are wanted: `routing.prefer = "local"` or the
///    session's model is local.
/// 2. No local model at all: [`Need::Local`].
/// 3. The message names an image file (`.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`, `.bmp`) and
///    no local model has a projector: [`Need::Vision`].
/// 4. The prompt is larger than every local model's context length: [`Need::Context`].
/// 5. `routing.prefer = "local"` but routing an ordinary task picks a cloud model (no local row
///    in the model table can run): [`Need::Local`].
pub fn detect(signals: &Signals<'_>) -> Option<Detected> {
    if !(signals.prefer_local || signals.session_local) {
        return None;
    }
    let found = |need: Need, reason: String| {
        Some(Detected {
            need,
            reason,
            context_tokens: None,
        })
    };
    if signals.local.is_empty() {
        return found(
            Need::Local,
            "local models are preferred but none is installed".into(),
        );
    }
    if signals.text.is_some_and(mentions_image) && !signals.local.iter().any(|m| m.vision) {
        return found(
            Need::Vision,
            "the message refers to an image and no local model can read images".into(),
        );
    }
    if let Some(tokens) = signals.prompt_tokens {
        let largest = signals.local.iter().filter_map(|m| m.context).max();
        if largest.is_some_and(|largest| tokens > largest) {
            return Some(Detected {
                need: Need::Context,
                reason: format!(
                    "the conversation ({tokens} tokens) is longer than any local model reads ({} tokens)",
                    largest.unwrap_or_default()
                ),
                context_tokens: Some(tokens),
            });
        }
    }
    if signals.prefer_local && !signals.routed_local {
        return found(
            Need::Local,
            "local models are preferred but routing falls back to a cloud model".into(),
        );
    }
    None
}

const IMAGE_EXTENSIONS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"];

/// Whether `text` names an image file: a word ending in an image extension.
pub fn mentions_image(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')' | ','))
        .map(|word| word.trim_end_matches(['.', ':', ';', '!', '?']))
        .any(|word| {
            let lower = word.to_ascii_lowercase();
            IMAGE_EXTENSIONS
                .iter()
                .any(|ext| lower.len() > ext.len() && lower.ends_with(ext))
        })
}

/// How a model not on disk yet would load: on the GPU, split between GPU and RAM, on the CPU,
/// or not at all. Ordered best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fit {
    Gpu,
    Partial,
    Cpu,
    None,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Gpu => "gpu",
            Fit::Partial => "partial",
            Fit::Cpu => "cpu",
            Fit::None => "none",
        }
    }
}

/// How a model of `size_bytes` would load within `budget`, estimated from its size alone (the
/// KV cache as a fifth of the weights). vLLM needs the GPU and Linux.
pub fn fit(size_bytes: u64, weights: Weights, budget: Budget) -> Fit {
    let footprint = Footprint::estimate(size_bytes);
    let placement = match weights {
        Weights::Gguf => place(footprint, budget),
        Weights::Safetensors if cfg!(target_os = "linux") => place_on_gpu(footprint, budget),
        Weights::Safetensors => None,
    };
    match placement {
        None => Fit::None,
        // Unified memory (Apple Silicon) runs every layer on the GPU.
        Some(p) if p.all_layers_on_gpu => Fit::Gpu,
        Some(p) if p.gpu_layers > 0 => Fit::Partial,
        Some(_) => Fit::Cpu,
    }
}

/// Quantisations in the order they are preferred among choices that fit equally well: a good
/// balance of size and quality first. Others come after, smallest first.
const QUANT_PREFERENCE: &[&str] = &[
    "Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0", "Q4_K_S", "IQ4_XS", "Q4_0", "Q5_K_S", "Q5_0", "Q3_K_M",
    "IQ3_M", "Q3_K_L", "Q3_K_S", "IQ3_XXS", "Q2_K",
];

/// How a choice ranks in [`pick`]: fit, size class, list order, quantisation, size; lower wins.
type RankKey = (Fit, u8, usize, usize, u64);

fn size_rank(size: &str) -> u8 {
    match size {
        "large" => 0,
        "medium" => 1,
        _ => 2,
    }
}

/// The candidates that can meet `need`, in list order. For [`Need::Context`], only those
/// reading at least `context_tokens`.
pub fn candidates_for(
    list: &[SuggestionCandidate],
    need: Need,
    context_tokens: Option<u64>,
) -> Vec<&SuggestionCandidate> {
    list.iter()
        .filter(|c| c.capabilities.iter().any(|cap| cap == need.capability()))
        .filter(|c| context_tokens.is_none_or(|tokens| u64::from(c.context) >= tokens))
        .collect()
}

/// A repository and quantisation to offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub repo: String,
    pub quant: String,
    /// The local model id it will have once downloaded.
    pub model_id: String,
    /// The download's size, the projector included.
    pub size_bytes: u64,
    pub fit: Fit,
    pub mmproj: bool,
}

/// The projector `hf::choose` downloads with `mmproj`: the F16 one when there are several.
fn projector_size(files: &[RepoFile]) -> u64 {
    let found = projectors(files);
    found
        .iter()
        .find(|file| file.path.to_ascii_lowercase().contains("f16"))
        .or(found.first())
        .map_or(0, |file| file.size)
}

/// Chooses what to offer from `listed` (candidates with their Hub file listings): each
/// candidate's GGUF quantisations are sized against `budget`, and the best is taken by
/// fit (GPU, then partial offload, then CPU; never one that does not fit), then size class
/// (larger first), then the candidate's place in the list, then [`QUANT_PREFERENCE`], then
/// smaller size. A vision candidate's projector counts toward its size.
pub fn pick(listed: &[(&SuggestionCandidate, Vec<RepoFile>)], budget: Budget) -> Option<Pick> {
    let mut best: Option<(RankKey, Pick)> = None;
    for (order, (candidate, files)) in listed.iter().enumerate() {
        let extra = if candidate.mmproj {
            projector_size(files)
        } else {
            0
        };
        for variant in variants(files)
            .iter()
            .filter(|v| v.weights == Weights::Gguf)
        {
            let size = variant.size_bytes() + extra;
            let fit = fit(size, Weights::Gguf, budget);
            if fit == Fit::None {
                continue;
            }
            let key = (
                fit,
                size_rank(&candidate.size),
                order,
                quant_rank(variant),
                size,
            );
            if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
                best = Some((
                    key,
                    Pick {
                        repo: candidate.repo.clone(),
                        quant: variant.name.clone(),
                        model_id: variant.model_id(&candidate.repo),
                        size_bytes: size,
                        fit,
                        mmproj: candidate.mmproj,
                    },
                ));
            }
        }
    }
    best.map(|(_, pick)| pick)
}

fn quant_rank(variant: &Variant) -> usize {
    QUANT_PREFERENCE
        .iter()
        .position(|quant| quant.eq_ignore_ascii_case(&variant.name))
        .unwrap_or(QUANT_PREFERENCE.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn candidate(repo: &str, caps: &[&str], size: &str, context: u32) -> SuggestionCandidate {
        SuggestionCandidate {
            repo: repo.into(),
            capabilities: caps.iter().map(|c| c.to_string()).collect(),
            size: size.into(),
            context,
            mmproj: caps.contains(&"vision"),
        }
    }

    fn file(path: &str, size: u64) -> RepoFile {
        RepoFile {
            path: path.into(),
            size,
            sha256: None,
        }
    }

    /// A fake machine: `vram` GiB free on the GPU and `ram` GiB of RAM.
    fn machine(vram: u64, ram: u64) -> Budget {
        Budget {
            vram_bytes: vram * GIB,
            ram_bytes: ram * GIB,
            unified: false,
        }
    }

    fn local(context: u64, vision: bool) -> LocalCapability {
        LocalCapability {
            context: Some(context),
            vision,
        }
    }

    #[test]
    fn nothing_is_suggested_unless_local_models_are_wanted() {
        let signals = Signals {
            text: Some("look at shot.png"),
            ..Signals::default()
        };
        assert_eq!(detect(&signals), None);
    }

    #[test]
    fn needs_are_detected_in_order() {
        let none: [LocalCapability; 0] = [];
        let prefer = Signals {
            prefer_local: true,
            local: &none,
            ..Signals::default()
        };
        assert_eq!(detect(&prefer).unwrap().need, Need::Local);

        let text_only = [local(8192, false)];
        let image = Signals {
            session_local: true,
            routed_local: true,
            local: &text_only,
            text: Some("What is in (screens/Home.PNG)?"),
            ..Signals::default()
        };
        assert_eq!(detect(&image).unwrap().need, Need::Vision);
        let with_vision = [local(8192, true)];
        assert_eq!(
            detect(&Signals {
                local: &with_vision,
                ..image
            }),
            None
        );

        let long = Signals {
            session_local: true,
            routed_local: true,
            local: &text_only,
            prompt_tokens: Some(9000),
            ..Signals::default()
        };
        let detected = detect(&long).unwrap();
        assert_eq!(
            (detected.need, detected.context_tokens),
            (Need::Context, Some(9000))
        );
        assert_eq!(
            detect(&Signals {
                prompt_tokens: Some(8000),
                ..long
            }),
            None
        );

        let fallback = Signals {
            prefer_local: true,
            routed_local: false,
            local: &text_only,
            ..Signals::default()
        };
        let detected = detect(&fallback).unwrap();
        assert_eq!(detected.need, Need::Local);
        assert!(detected.reason.contains("cloud"));
        // A local session model without a local preference is not a fallback.
        assert_eq!(
            detect(&Signals {
                prefer_local: false,
                session_local: true,
                ..fallback
            }),
            None
        );
    }

    #[test]
    fn image_mentions_need_a_file_name() {
        assert!(mentions_image("see `a/b.jpeg`."));
        assert!(!mentions_image("convert to png please"));
        assert!(!mentions_image(".png"));
    }

    #[test]
    fn candidates_match_the_need_and_context() {
        let list = vec![
            candidate("a/chat", &["chat", "tools"], "small", 32_768),
            candidate("a/coder", &["code", "chat"], "medium", 262_144),
            candidate("a/eyes", &["vision", "chat"], "small", 131_072),
        ];
        let repos = |need, tokens| {
            candidates_for(&list, need, tokens)
                .iter()
                .map(|c| c.repo.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(repos(Need::Vision, None), ["a/eyes"]);
        assert_eq!(repos(Need::Tools, None), ["a/chat"]);
        assert_eq!(repos(Need::Context, Some(100_000)), ["a/coder", "a/eyes"]);
        assert_eq!(repos(Need::Local, None), ["a/chat", "a/coder", "a/eyes"]);
    }

    fn listing() -> Vec<RepoFile> {
        vec![
            file("m-Q2_K.gguf", 3 * GIB),
            file("m-Q4_K_M.gguf", 5 * GIB),
            file("m-Q8_0.gguf", 9 * GIB),
            file("m-BF16.gguf", 16 * GIB),
        ]
    }

    #[test]
    fn the_quantisation_that_fits_the_gpu_is_preferred() {
        let chat = candidate("a/m-GGUF", &["chat"], "small", 32_768);
        let listed = [(&chat, listing())];
        // 5 GiB weights + 1 GiB KV + 0.5 GiB overhead fit an 8 GiB GPU: Q4_K_M.
        let roomy = pick(&listed, machine(8, 32)).unwrap();
        assert_eq!((roomy.quant.as_str(), roomy.fit), ("Q4_K_M", Fit::Gpu));
        assert_eq!(roomy.model_id, "m-Q4_K_M");
        // Only Q2_K (4.1 GiB in all) fits a 5 GiB GPU entirely, so it beats a partial Q4_K_M.
        let small = pick(&listed, machine(5, 32)).unwrap();
        assert_eq!((small.quant.as_str(), small.fit), ("Q2_K", Fit::Gpu));
        // A tiny GPU: everything is partial, so the preferred quantisation wins.
        let tiny = pick(&listed, machine(1, 32)).unwrap();
        assert_eq!((tiny.quant.as_str(), tiny.fit), ("Q4_K_M", Fit::Partial));
        // No GPU: CPU only.
        let cpu = pick(&listed, machine(0, 32)).unwrap();
        assert_eq!((cpu.quant.as_str(), cpu.fit), ("Q4_K_M", Fit::Cpu));
        // Nothing fits at all.
        assert_eq!(pick(&listed, machine(0, 2)), None);
    }

    #[test]
    fn larger_candidates_win_when_both_fit_and_projectors_count() {
        let small = candidate("a/small-GGUF", &["vision"], "small", 8192);
        let medium = candidate("a/medium-GGUF", &["vision"], "medium", 8192);
        let with_projector = |weights: u64| {
            vec![
                file("m-Q4_K_M.gguf", weights * GIB),
                file("mmproj-F16.gguf", GIB),
                file("mmproj-F32.gguf", 2 * GIB),
            ]
        };
        let listed = [(&small, with_projector(2)), (&medium, with_projector(5))];
        let roomy = pick(&listed, machine(12, 32)).unwrap();
        assert_eq!(roomy.repo, "a/medium-GGUF");
        assert_eq!(roomy.size_bytes, 6 * GIB);
        assert!(roomy.mmproj);
        // The medium one (6 GiB with its projector) no longer fits the GPU; the small one does.
        let tight = pick(&listed, machine(5, 32)).unwrap();
        assert_eq!((tight.repo.as_str(), tight.fit), ("a/small-GGUF", Fit::Gpu));
    }

    #[test]
    fn needs_read_back_from_tool_arguments() {
        assert_eq!(Need::parse("Vision"), Some(Need::Vision));
        assert_eq!(Need::parse("chat"), Some(Need::Local));
        assert_eq!(Need::parse("long-context"), Some(Need::Context));
        assert_eq!(Need::parse("telepathy"), None);
    }
}
