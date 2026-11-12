//! Per-model llama.cpp settings, the overrides the app may apply to model-table rows, and how a
//! local model's effective launch settings are resolved from them.

use serde::{Deserialize, Serialize};

use crate::settings::{
    CatalogEntry, LLAMACPP_ENDPOINT, LlamaCppSettings, ModelPlacement, Settings, Tier,
};

/// Slots of a model pinned to the CPU lane unless a setting says otherwise: the cores are slow
/// and shared, so calls take turns.
pub const CPU_LANE_SLOTS: u32 = 1;
/// Longest a call waits for a slot on a CPU-lane model unless a setting says otherwise (or the
/// global `max_wait_s` is longer): calls queue behind tasks that take minutes.
pub const CPU_LANE_MAX_WAIT_S: u64 = 3600;
/// Slots (calls decoded at once) a model gets unless a setting says otherwise, when `parallel` is
/// not larger. Slots reserve no KV cache of their own (they share one pool), so the ceiling is
/// high.
pub const DEFAULT_SLOTS: u32 = 16;
/// Most slots one server can have (`LLAMA_MAX_SEQ`).
pub const MAX_SLOTS: u32 = 256;
/// Tokens of KV cache a model is started with unless a setting fixes the pool: the pool grows
/// from here, on demand, up to the model's context ceiling.
pub const INITIAL_POOL_TOKENS: u32 = 16_384;
/// [`ModelLaunch::context`] when the context is `auto`: no limit of its own, so the ceiling is
/// what the model was trained for and what fits in memory.
pub const NO_CONTEXT_LIMIT: u32 = u32::MAX;

/// How much context one agent may use on a local model: `auto` (the most the model was trained
/// for that fits in memory) or a number of tokens. Written as `"auto"` or an integer; a string
/// holding an integer reads as that integer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ContextLimit {
    #[default]
    Auto,
    Tokens(u32),
}

impl ContextLimit {
    /// The tokens it fixes, or `None` for `auto`.
    pub fn tokens(self) -> Option<u32> {
        match self {
            ContextLimit::Auto => None,
            ContextLimit::Tokens(tokens) => Some(tokens),
        }
    }

    /// Reads `auto` or a whole number of tokens.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("auto") {
            return Ok(ContextLimit::Auto);
        }
        text.parse()
            .map(ContextLimit::Tokens)
            .map_err(|_| format!("context must be \"auto\" or a number of tokens, not {text:?}"))
    }
}

impl From<u32> for ContextLimit {
    fn from(tokens: u32) -> Self {
        ContextLimit::Tokens(tokens)
    }
}

impl std::fmt::Display for ContextLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextLimit::Auto => f.write_str("auto"),
            ContextLimit::Tokens(tokens) => write!(f, "{tokens}"),
        }
    }
}

impl Serialize for ContextLimit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ContextLimit::Auto => serializer.serialize_str("auto"),
            ContextLimit::Tokens(tokens) => serializer.serialize_u32(*tokens),
        }
    }
}

impl<'de> Deserialize<'de> for ContextLimit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = ContextLimit;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("\"auto\" or a number of tokens")
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<ContextLimit, E> {
                u32::try_from(value)
                    .map(ContextLimit::Tokens)
                    .map_err(|_| E::custom("context is too large"))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<ContextLimit, E> {
                u64::try_from(value)
                    .map_err(|_| E::custom("context cannot be negative"))
                    .and_then(|value| self.visit_u64(value))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<ContextLimit, E> {
                ContextLimit::parse(value).map_err(E::custom)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
/// KV cache types `llama-server` accepts for `--cache-type-k` and `--cache-type-v`.
pub const CACHE_TYPES: [&str; 9] = [
    "f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1",
];
/// Cache types that keep full-precision values, so they need no flash attention.
const FULL_PRECISION: [&str; 3] = ["f16", "bf16", "f32"];
/// Flags Imperium sets itself, which a model's `extra_args` may not repeat.
const OWNED_FLAGS: &[&str] = &[
    "-m",
    "--model",
    "--host",
    "--port",
    "-c",
    "--ctx-size",
    "-np",
    "--parallel",
    "-kvu",
    "--kv-unified",
    "-no-kvu",
    "--no-kv-unified",
    "--kv-unified-per-slot",
    "--cache-idle-slots",
    "--no-cache-idle-slots",
    "-ngl",
    "--gpu-layers",
    "--n-gpu-layers",
    "-ctk",
    "--cache-type-k",
    "-ctv",
    "--cache-type-v",
    "-fa",
    "--flash-attn",
    "--cache-ram",
    "-cram",
    "--slot-save-path",
    "--device",
];
/// Whether `flag` belongs to llama-server's draft model family (b12180), which `draft_model`,
/// `draft_max`, `draft_min` and `draft_gpu_layers` replace: anything that names, loads, places,
/// shapes or tunes a draft would bypass the memory plan. By pattern, so flags a later release
/// adds to the family are covered: `--spec-draft-*`, `--draft*`, the long forms ending in
/// `-draft` (`--model-draft`, `--gpu-layers-draft`, `--cache-type-k-draft`, ...), and the short
/// ones (`-md`, `-ngld`, `-hfd`, `-hfrd`, `-ctkd`, `-ctvd`, `-devd`, `-otd`, `-cmoed`,
/// `-ncmoed`, `-td`, `-tbd`, `-Cd`, `-Crd`, `-Cbd`).
pub fn is_draft_flag(flag: &str) -> bool {
    const SHORT: [&str; 15] = [
        "-md", "-ngld", "-hfd", "-hfrd", "-ctkd", "-ctvd", "-devd", "-otd", "-cmoed", "-ncmoed",
        "-td", "-tbd", "-Cd", "-Crd", "-Cbd",
    ];
    flag.starts_with("--spec-draft-")
        || flag == "--draft"
        || flag.starts_with("--draft-")
        || (flag.starts_with("--") && flag.ends_with("-draft"))
        || SHORT.contains(&flag)
}
/// llama-server's own default for `--spec-draft-n-max` (b12180), what `draft_max` replaces.
pub const DEFAULT_DRAFT_MAX: u32 = 3;

/// `[runtime.llamacpp.models."<local model id>"]`: settings of one local model. Unset keys take
/// the defaults under `[runtime.llamacpp]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct LocalModelSettings {
    /// A display name clients show instead of the id, such as `Qwen 3 (4B)`. The id stays
    /// what config, routing and `--model` use.
    pub alias: Option<String>,
    /// The model card: what the model is good at and notes, for agents choosing a model (see
    /// [`check_card`]). Added to what the model's own metadata says.
    pub card: Option<String>,
    /// Most tokens one agent may use: `auto` or a number.
    pub context: Option<ContextLimit>,
    /// Fixes the tokens of KV cache all slots share (at least `context`); unset, the pool
    /// starts small and grows on demand.
    pub pool_tokens: Option<u32>,
    /// Most calls the server runs at once (`--parallel`); also written `max_slots`. Slots share
    /// one KV pool and reserve none of their own.
    #[serde(alias = "max_slots")]
    pub slots: Option<u32>,
    pub cache_type_k: Option<String>,
    pub cache_type_v: Option<String>,
    /// `auto`, `on`, or `off`.
    pub flash_attn: Option<String>,
    /// Cap on the GPU layers the planner places.
    pub max_gpu_layers: Option<u32>,
    pub max_wait_s: Option<u64>,
    /// Most tokens one reply may generate before it is retried without a cap; 0 = no cap.
    pub max_output_tokens: Option<u32>,
    /// Arguments added after `runtime.llamacpp.extra_args`.
    pub extra_args: Option<Vec<String>>,
    /// Whether the model can reason, over what its chat template says (a thinking switch,
    /// think tags or `reasoning_effort` mark one that can).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// The id of an installed local model that drafts tokens for this one (speculative
    /// decoding, llama.cpp `--spec-draft-model`): a smaller model with the same tokenizer
    /// family. Without it, no draft is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_model: Option<String>,
    /// Most tokens the draft proposes per step (`--spec-draft-n-max`); unset keeps llama.cpp's
    /// own default. Needs `draft_model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_max: Option<u32>,
    /// Fewest tokens the draft proposes per step (`--spec-draft-n-min`); unset keeps llama.cpp's
    /// own default. Needs `draft_model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_min: Option<u32>,
    /// Layers of the draft model placed on the GPU (`--spec-draft-ngl`); 0 keeps it on the CPU.
    /// Unset lets llama.cpp place them all. Needs `draft_model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_gpu_layers: Option<u32>,
}

/// `[models.overrides."<catalog id>"]`: what the app changes on a model-table row.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct TableOverride {
    /// A display name for the row, over the row's own `alias`.
    pub alias: Option<String>,
    /// A model card for the row, over the row's own `card`.
    pub card: Option<String>,
    pub enabled: Option<bool>,
    pub tier: Option<Tier>,
    pub tags: Option<Vec<String>>,
    pub context_window: Option<u32>,
    pub input_usd_per_mtok: Option<f64>,
    pub output_usd_per_mtok: Option<f64>,
}

/// What a CPU-lane row of the model table says about the model it pins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaneRow {
    pub slots: Option<u32>,
    pub context: Option<u32>,
    pub max_wait_s: Option<u64>,
}

/// The draft model a local model is configured to use for speculative decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftLaunch {
    /// The id of the installed local model that drafts.
    pub model: String,
    pub max: Option<u32>,
    pub min: Option<u32>,
    pub gpu_layers: Option<u32>,
}

/// The settings a local model is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLaunch {
    /// Most tokens one agent may use. From the settings alone it is the configured limit, or
    /// [`NO_CONTEXT_LIMIT`] for `auto`; the supervisor caps it to what the model was trained for
    /// and what fits in memory.
    pub context: u32,
    /// The context is `auto`.
    pub context_auto: bool,
    /// Tokens of KV cache shared by every slot (`--ctx-size`): the fixed pool, or the pool the
    /// model starts with and grows from.
    pub pool_tokens: u32,
    /// A setting fixes `pool_tokens`, so the pool never grows.
    pub pool_fixed: bool,
    /// Most tokens the pool may grow to when several agents fill it: what fits in memory beside
    /// the weights, at most every slot at the full context. From the settings alone it is
    /// `pool_tokens`; the supervisor sizes it.
    pub pool_max: u32,
    /// The fixed pool did not fit beside the weights on the GPU, so the supervisor cut
    /// `pool_tokens` to what fits rather than moving layers to the CPU. Always false from the
    /// settings alone.
    pub pool_clamped: bool,
    pub slots: u32,
    pub cache_type_k: String,
    pub cache_type_v: String,
    pub flash_attn: String,
    pub max_gpu_layers: Option<u32>,
    pub max_wait_s: u64,
    pub max_output_tokens: u32,
    /// The global arguments, then the model's.
    pub extra_args: Vec<String>,
    /// The draft model configured for speculative decoding. The supervisor decides at launch
    /// whether it can be used.
    pub draft: Option<DraftLaunch>,
}

impl ModelLaunch {
    /// Whether a server started with `self` must be restarted to match `other`: any setting
    /// that reaches the launch arguments, the memory plan, or the token pool differs. The wait
    /// and the output cap (which only sizes reservations) apply without a restart.
    pub fn restarts_for(&self, other: &ModelLaunch) -> bool {
        let same_live = ModelLaunch {
            max_wait_s: self.max_wait_s,
            max_output_tokens: self.max_output_tokens,
            ..other.clone()
        };
        *self != same_live
    }
}

impl LlamaCppSettings {
    /// A local model's effective settings: its own entry, then (for the CPU lane) its row's
    /// legacy keys and the lane defaults, then the globals.
    pub fn launch(&self, id: &str, lane: Option<LaneRow>) -> ModelLaunch {
        self.launch_with(self.models.get(id), lane)
    }

    /// Like [`LlamaCppSettings::launch`], with the model's own settings given.
    pub fn launch_with(
        &self,
        own: Option<&LocalModelSettings>,
        lane: Option<LaneRow>,
    ) -> ModelLaunch {
        let own = own.cloned().unwrap_or_default();
        let row = lane.unwrap_or_default();
        let on_lane = lane.is_some();
        let limit = own
            .context
            .or(row.context.map(ContextLimit::Tokens))
            .unwrap_or(self.context);
        let slots = if on_lane {
            own.slots.or(row.slots).unwrap_or(CPU_LANE_SLOTS)
        } else {
            own.slots
                .or(self.slots)
                .unwrap_or_else(|| DEFAULT_SLOTS.max(self.parallel))
        };
        // The pool starts small and grows on demand; a small fixed context times the calls
        // that may share it is smaller still.
        let (context, derived) = match limit {
            ContextLimit::Auto => (NO_CONTEXT_LIMIT, INITIAL_POOL_TOKENS),
            ContextLimit::Tokens(context) => (
                context,
                context
                    .saturating_mul(if on_lane { slots } else { self.parallel })
                    .clamp(1, INITIAL_POOL_TOKENS),
            ),
        };
        let fixed_pool = own.pool_tokens.or(self.pool_tokens);
        let default_wait = if on_lane {
            CPU_LANE_MAX_WAIT_S.max(self.max_wait_s)
        } else {
            self.max_wait_s
        };
        let mut extra_args = self.extra_args.clone();
        extra_args.extend(own.extra_args.unwrap_or_default());
        let draft = own.draft_model.map(|model| DraftLaunch {
            model,
            max: own.draft_max,
            min: own.draft_min,
            gpu_layers: own.draft_gpu_layers,
        });
        ModelLaunch {
            context,
            context_auto: limit == ContextLimit::Auto,
            pool_tokens: fixed_pool.unwrap_or(derived),
            pool_fixed: fixed_pool.is_some(),
            pool_clamped: false,
            pool_max: fixed_pool.unwrap_or(derived),
            slots,
            cache_type_k: own.cache_type_k.unwrap_or_else(|| self.cache_type.clone()),
            cache_type_v: own.cache_type_v.unwrap_or_else(|| self.cache_type.clone()),
            flash_attn: own.flash_attn.unwrap_or_else(|| self.flash_attn.clone()),
            max_gpu_layers: own.max_gpu_layers,
            max_wait_s: own.max_wait_s.or(row.max_wait_s).unwrap_or(default_wait),
            max_output_tokens: own.max_output_tokens.unwrap_or(self.max_output_tokens),
            extra_args,
            draft,
        }
    }

    /// The settings a model without any of its own gets.
    pub fn default_launch(&self) -> ModelLaunch {
        self.launch_with(None, None)
    }
}

/// Checks one local model's settings on their own; the message names the problem, not the model.
/// Longest display alias, in characters.
pub const MAX_ALIAS_CHARS: usize = 80;

/// Checks a model's display alias: when set, non-blank, without control characters or
/// surrounding spaces, and at most [`MAX_ALIAS_CHARS`] characters.
pub fn check_alias(alias: Option<&str>) -> Result<(), String> {
    let Some(alias) = alias else {
        return Ok(());
    };
    if alias.trim().is_empty() {
        return Err("alias is blank; leave it out to show the id".into());
    }
    if alias.trim() != alias {
        return Err(format!("alias {alias:?} starts or ends with a space"));
    }
    if alias.chars().any(char::is_control) {
        return Err(format!("alias {alias:?} has a control character"));
    }
    if alias.chars().count() > MAX_ALIAS_CHARS {
        return Err(format!("alias is longer than {MAX_ALIAS_CHARS} characters"));
    }
    Ok(())
}

/// Longest model card, in characters.
pub const MAX_CARD_CHARS: usize = 4000;

/// Checks a model card: when set, not blank, at most [`MAX_CARD_CHARS`] characters, and without
/// control characters other than line breaks and tabs.
pub fn check_card(card: Option<&str>) -> Result<(), String> {
    let Some(card) = card else {
        return Ok(());
    };
    if card.trim().is_empty() {
        return Err("card is blank; leave it out instead".into());
    }
    if card
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("card has a control character".into());
    }
    if card.chars().count() > MAX_CARD_CHARS {
        return Err(format!("card is longer than {MAX_CARD_CHARS} characters"));
    }
    Ok(())
}

pub fn check_local_model(settings: &LocalModelSettings) -> Result<(), String> {
    check_alias(settings.alias.as_deref())?;
    check_card(settings.card.as_deref())?;
    for (key, value) in [
        (
            "context",
            settings
                .context
                .and_then(ContextLimit::tokens)
                .map(u64::from),
        ),
        ("pool_tokens", settings.pool_tokens.map(u64::from)),
        ("slots", settings.slots.map(u64::from)),
        ("max_wait_s", settings.max_wait_s),
    ] {
        if value == Some(0) {
            return Err(format!("{key} needs a value of at least 1"));
        }
    }
    if settings.slots.is_some_and(|slots| slots > MAX_SLOTS) {
        return Err(format!("slots must be at most {MAX_SLOTS}"));
    }
    check_cache_type("cache_type_k", settings.cache_type_k.as_deref())?;
    check_cache_type("cache_type_v", settings.cache_type_v.as_deref())?;
    check_flash_attn(settings.flash_attn.as_deref())?;
    check_draft(settings)?;
    check_extra_args(
        settings.extra_args.iter().flatten(),
        settings.draft_model.is_some(),
        true,
    )
}

/// Checks extra arguments for the draft model flags, always, and for `--spec-type` while a draft
/// model is set (Imperium passes `--spec-type draft-simple` then). With `all_owned`, also for
/// every other flag Imperium sets itself: a model's own arguments. The global arguments are
/// checked for the draft flags only, so a global `--flash-attn on` or `--ctx-size` written
/// before these flags were owned keeps loading.
fn check_extra_args<'a>(
    args: impl Iterator<Item = &'a String>,
    drafting: bool,
    all_owned: bool,
) -> Result<(), String> {
    for arg in args {
        let flag = arg.split('=').next().unwrap_or(arg);
        if is_draft_flag(flag) || (all_owned && OWNED_FLAGS.contains(&flag)) {
            return Err(format!(
                "extra_args may not contain {flag}, which Imperium sets itself"
            ));
        }
        if flag == "--spec-type" && drafting {
            return Err(
                "extra_args may not contain --spec-type while draft_model is set, \
                 which has Imperium pass --spec-type draft-simple"
                    .into(),
            );
        }
    }
    Ok(())
}

/// Checks the speculative-decoding keys of one model: the draft's id is not blank, the draft
/// parameters come with a draft, and its token counts make sense.
fn check_draft(settings: &LocalModelSettings) -> Result<(), String> {
    match settings.draft_model.as_deref() {
        Some(id) if id.trim().is_empty() => {
            return Err("draft_model is blank; leave it out to use no draft".into());
        }
        Some(_) => {}
        None => {
            for (key, set) in [
                ("draft_max", settings.draft_max.is_some()),
                ("draft_min", settings.draft_min.is_some()),
                ("draft_gpu_layers", settings.draft_gpu_layers.is_some()),
            ] {
                if set {
                    return Err(format!("{key} needs draft_model"));
                }
            }
        }
    }
    if settings.draft_max == Some(0) {
        return Err("draft_max needs a value of at least 1".into());
    }
    // An unset `draft_max` is llama.cpp's default, which `draft_min` may not exceed either.
    if let Some(min) = settings.draft_min {
        let max = settings.draft_max.unwrap_or(DEFAULT_DRAFT_MAX);
        if min > max {
            return Err(format!(
                "draft_min ({min}) may not exceed draft_max ({max}{})",
                if settings.draft_max.is_none() {
                    ", llama.cpp's default"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(())
}

fn check_cache_type(key: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        Some(value) if !CACHE_TYPES.contains(&value) => Err(format!(
            "{key} must be one of {}, not {value:?}",
            CACHE_TYPES.join(", ")
        )),
        _ => Ok(()),
    }
}

fn check_flash_attn(value: Option<&str>) -> Result<(), String> {
    match value {
        Some(value) if !matches!(value, "auto" | "on" | "off") => Err(format!(
            "flash_attn must be auto, on, or off, not {value:?}"
        )),
        _ => Ok(()),
    }
}

/// Checks a launch resolved from the settings.
pub fn check_launch(launch: &ModelLaunch) -> Result<(), String> {
    // Only a fixed pool must hold a fixed context; a pool that grows does so up to the context.
    if launch.pool_fixed && !launch.context_auto && launch.pool_tokens < launch.context {
        return Err(format!(
            "pool_tokens ({}) must be at least context ({})",
            launch.pool_tokens, launch.context
        ));
    }
    if !FULL_PRECISION.contains(&launch.cache_type_v.as_str()) && launch.flash_attn == "off" {
        return Err(format!(
            "cache_type_v {:?} is quantised, which needs flash_attn on or auto",
            launch.cache_type_v
        ));
    }
    Ok(())
}

impl Settings {
    /// Checks the global and per-model llama.cpp settings and the app's row overrides.
    pub(crate) fn validate_models(&self) -> Result<(), String> {
        let llama = &self.runtime.llamacpp;
        check_cache_type("runtime.llamacpp.cache_type", Some(&llama.cache_type))?;
        check_flash_attn(Some(&llama.flash_attn)).map_err(|e| format!("runtime.llamacpp: {e}"))?;
        if !["off", "ask", "auto"].contains(&llama.update_external.as_str()) {
            return Err(format!(
                "runtime.llamacpp.update_external must be off, ask, or auto, not {:?}",
                llama.update_external
            ));
        }
        check_local_model(&LocalModelSettings {
            context: Some(llama.context),
            pool_tokens: llama.pool_tokens,
            slots: llama.slots,
            ..LocalModelSettings::default()
        })
        .map_err(|e| format!("runtime.llamacpp: {e}"))?;
        check_launch(&llama.default_launch()).map_err(|e| format!("runtime.llamacpp: {e}"))?;
        check_extra_args(llama.extra_args.iter(), false, false)
            .map_err(|e| format!("runtime.llamacpp: {e}"))?;
        for (id, own) in &llama.models {
            check_local_model(own).map_err(|e| format!("runtime.llamacpp.models.{id:?}: {e}"))?;
            if own.draft_model.is_some() {
                check_extra_args(llama.extra_args.iter(), true, false)
                    .map_err(|e| format!("runtime.llamacpp (with the draft of {id:?}): {e}"))?;
            }
            if own.draft_model.as_deref() == Some(id.as_str()) {
                return Err(format!(
                    "runtime.llamacpp.models.{id:?}: draft_model may not be the model itself"
                ));
            }
            let row = self.cpu_row(id);
            if let (Some(row), Some(entry)) = (row, self.cpu_catalog_entry(id)) {
                let numbers = [
                    (
                        "parallel",
                        "slots",
                        row.slots.map(u64::from),
                        own.slots.map(u64::from),
                    ),
                    (
                        "context",
                        "context",
                        row.context.map(u64::from),
                        own.context.and_then(ContextLimit::tokens).map(u64::from),
                    ),
                    ("max_wait_s", "max_wait_s", row.max_wait_s, own.max_wait_s),
                ];
                for (row_key, key, in_row, in_table) in numbers {
                    if let (Some(a), Some(b)) = (in_row, in_table)
                        && a != b
                    {
                        return Err(format!(
                            "models.catalog row {:?} sets {row_key} = {a}, but runtime.llamacpp.models.{id:?} sets {key} = {b}; set one of them",
                            entry.id
                        ));
                    }
                }
            }
            check_launch(&llama.launch(id, row))
                .map_err(|e| format!("runtime.llamacpp.models.{id:?}: {e}"))?;
        }
        for entry in &self.models.catalog {
            check_alias(entry.alias.as_deref())
                .and_then(|()| check_card(entry.card.as_deref()))
                .map_err(|e| format!("models.catalog row {:?}: {e}", entry.id))?;
        }
        for (id, over) in &self.models.overrides {
            check_alias(over.alias.as_deref())
                .and_then(|()| check_card(over.card.as_deref()))
                .map_err(|e| format!("models.overrides.{id:?}: {e}"))?;
            if over.input_usd_per_mtok.is_some() != over.output_usd_per_mtok.is_some() {
                return Err(format!(
                    "models.overrides.{id:?}: set both input_usd_per_mtok and output_usd_per_mtok, or neither"
                ));
            }
            if over.context_window == Some(0) {
                return Err(format!(
                    "models.overrides.{id:?}: context_window needs a value of at least 1"
                ));
            }
        }
        Ok(())
    }

    fn cpu_catalog_entry(&self, id: &str) -> Option<&CatalogEntry> {
        self.models.catalog.iter().find(|entry| {
            entry.placement == ModelPlacement::Cpu && entry.llamacpp_model() == Some(id)
        })
    }

    /// What a CPU-lane row says about local model `id`, if one pins it.
    pub fn cpu_row(&self, id: &str) -> Option<LaneRow> {
        self.cpu_catalog_entry(id).map(|entry| LaneRow {
            slots: entry.parallel,
            context: entry.context,
            max_wait_s: entry.max_wait_s,
        })
    }

    /// The effective launch settings of local model `id`.
    pub fn launch_of(&self, id: &str) -> ModelLaunch {
        self.runtime.llamacpp.launch(id, self.cpu_row(id))
    }

    /// Applies `[models.overrides]` to the model table and the endpoint prices. Overrides naming
    /// no row are ignored.
    pub(crate) fn apply_overrides(&mut self) {
        let overrides = self.models.overrides.clone();
        for entry in &mut self.models.catalog {
            let Some(over) = overrides.get(&entry.id) else {
                continue;
            };
            if over.alias.is_some() {
                entry.alias.clone_from(&over.alias);
            }
            if over.card.is_some() {
                entry.card.clone_from(&over.card);
            }
            if let Some(enabled) = over.enabled {
                entry.enabled = enabled;
            }
            if let Some(tier) = over.tier {
                entry.tier = tier;
            }
            if let Some(tags) = &over.tags {
                entry.tags.clone_from(tags);
            }
            if over.context_window.is_some() {
                entry.context_window = over.context_window;
            }
            if let (Some(input), Some(output)) = (over.input_usd_per_mtok, over.output_usd_per_mtok)
                && let Some((endpoint, model)) = entry.spec.split_once(':')
                && endpoint != LLAMACPP_ENDPOINT
                && let Some(endpoint) = self.endpoints.get_mut(endpoint)
            {
                let cached = endpoint
                    .pricing
                    .get(model)
                    .and_then(|pricing| pricing.cached_input_usd_per_mtok);
                endpoint.pricing.insert(
                    model.to_string(),
                    imperium_api::Pricing {
                        input_usd_per_mtok: input,
                        output_usd_per_mtok: output,
                        cached_input_usd_per_mtok: cached,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{INITIAL_POOL_TOKENS, NO_CONTEXT_LIMIT, check_launch};
    use crate::{Config, Layer, Paths, Source, Tier, load};
    use std::path::PathBuf;

    fn config(layers: &[(Source, &str)]) -> Config {
        let mut all = vec![Layer::defaults()];
        all.extend(
            layers
                .iter()
                .map(|(source, text)| Layer::new(source.clone(), text.parse().unwrap())),
        );
        Config::from_layers(all)
    }

    fn user() -> Source {
        Source::User(PathBuf::from("config.toml"))
    }

    fn app() -> Source {
        Source::App(PathBuf::from("models.toml"))
    }

    #[test]
    fn per_model_settings_resolve_over_the_defaults() {
        // 1. Nothing set: the globals, with the default slots the larger of 8 and `parallel`;
        // the context is `auto` (no limit of its own) and the pool starts small and grows.
        let settings = config(&[]).settings().unwrap();
        let launch = settings.launch_of("any");
        assert_eq!(
            (launch.context, launch.pool_tokens, launch.slots),
            (NO_CONTEXT_LIMIT, INITIAL_POOL_TOKENS, 16)
        );
        assert!(launch.context_auto && !launch.pool_fixed);
        assert_eq!(check_launch(&launch), Ok(()));
        assert_eq!(
            (launch.cache_type_k.as_str(), launch.flash_attn.as_str()),
            ("f16", "auto")
        );
        assert_eq!(
            (launch.max_output_tokens, launch.max_gpu_layers),
            (8192, None)
        );
        let wide = config(&[(user(), "[runtime.llamacpp]\nparallel = 20\n")])
            .settings()
            .unwrap();
        assert_eq!(wide.launch_of("any").slots, 20);
        assert_eq!(wide.launch_of("any").pool_tokens, INITIAL_POOL_TOKENS);
        // A fixed context starts the pool at context x parallel, at most the initial pool, and
        // the pool may start below the context because it grows.
        let fixed = |text: &str| {
            config(&[(user(), &format!("[runtime.llamacpp]\n{text}\n"))])
                .settings()
                .unwrap()
                .launch_of("any")
        };
        let small = fixed("context = 4096");
        assert_eq!((small.context, small.pool_tokens), (4096, 8192));
        assert!(!small.context_auto);
        let large = fixed("context = 131072");
        assert_eq!(
            (large.context, large.pool_tokens),
            (131072, INITIAL_POOL_TOKENS)
        );
        assert_eq!(check_launch(&large), Ok(()));
        assert_eq!(fixed("context = \"65536\"").context, 65536);
        assert!(fixed("context = \"auto\"").context_auto);
        // A fixed pool must still hold a fixed context.
        let short = config(&[(
            user(),
            "[runtime.llamacpp]\ncontext = 32768\npool_tokens = 16384\n",
        )]);
        assert!(short.settings().is_err());
        let slots = config(&[(
            user(),
            "[runtime.llamacpp]\nslots = 3\npool_tokens = 9000\n",
        )])
        .settings()
        .unwrap();
        let launch = slots.launch_of("any");
        assert_eq!((launch.slots, launch.pool_tokens), (3, 9000));

        // 2. A model's own entry wins field by field, and its extra arguments follow the
        // global ones.
        let own = config(&[(
            user(),
            r#"
[runtime.llamacpp]
extra_args = ["--global"]
[runtime.llamacpp.models."Qwen3-8B-Q4_K_M"]
context = 32768
pool_tokens = 65536
slots = 4
cache_type_k = "q8_0"
flash_attn = "on"
max_gpu_layers = 20
max_output_tokens = 0
extra_args = ["--n-cpu-moe", "8"]
"#,
        )])
        .settings()
        .unwrap();
        let launch = own.launch_of("Qwen3-8B-Q4_K_M");
        assert_eq!(
            (launch.context, launch.pool_tokens, launch.slots),
            (32768, 65536, 4)
        );
        assert_eq!(
            (launch.cache_type_k.as_str(), launch.cache_type_v.as_str()),
            ("q8_0", "f16")
        );
        assert_eq!(
            (
                launch.flash_attn.as_str(),
                launch.max_gpu_layers,
                launch.max_output_tokens
            ),
            ("on", Some(20), 0)
        );
        assert_eq!(launch.extra_args, ["--global", "--n-cpu-moe", "8"]);
        assert_eq!(
            own.launch_of("other").slots,
            16,
            "other models keep the defaults"
        );

        // 3. A CPU-lane row's legacy keys and the lane defaults: one slot, an hour's wait.
        let row = |extra: &str| {
            format!(
                "[[models.catalog]]\nid = \"critic\"\nspec = \"llamacpp:critic\"\nlocation = \"local\"\ntier = \"standard\"\nplacement = \"cpu\"\n{extra}"
            )
        };
        let lane = config(&[(user(), &row(""))]).settings().unwrap();
        let launch = lane.launch_of("critic");
        assert_eq!(
            (launch.slots, launch.pool_tokens, launch.max_wait_s),
            (1, INITIAL_POOL_TOKENS, 3600)
        );
        let lane = config(&[(user(), &row("parallel = 2\ncontext = 1024\n"))])
            .settings()
            .unwrap();
        let launch = lane.launch_of("critic");
        assert_eq!(
            (launch.slots, launch.context, launch.pool_tokens),
            (2, 1024, 2048)
        );
        // The per-model table wins over a row that leaves the key out, and a row and a table
        // that disagree are refused, naming both keys.
        let both = config(&[
            (user(), &row("parallel = 2\n")),
            (app(), "[runtime.llamacpp.models.critic]\nslots = 3\n"),
        ])
        .settings()
        .unwrap_err()
        .to_string();
        assert!(
            both.contains("parallel") && both.contains("runtime.llamacpp.models"),
            "{both}"
        );
        let agree = config(&[
            (user(), &row("parallel = 3\n")),
            (app(), "[runtime.llamacpp.models.critic]\nslots = 3\n"),
        ])
        .settings()
        .unwrap();
        assert_eq!(agree.launch_of("critic").slots, 3);

        // 4. The app layer wins over config.toml, key by key.
        let layered = config(&[
            (
                user(),
                "[runtime.llamacpp.models.x]\ncontext = 1000\nslots = 3\n",
            ),
            (app(), "[runtime.llamacpp.models.x]\nslots = 5\n"),
        ])
        .settings()
        .unwrap();
        let launch = layered.launch_of("x");
        assert_eq!((launch.context, launch.slots), (1000, 5));

        // 5. Overrides change a model-table row and its endpoint's prices; an id that names
        // no row is ignored.
        let table = r#"
[[models.catalog]]
id = "coder"
spec = "cloud:coder-1"
location = "cloud"
tier = "basic"
tags = ["code"]
[endpoints.cloud]
base_url = "https://example.invalid/v1"
"#;
        let overridden = config(&[
            (user(), table),
            (
                app(),
                r#"
[models.overrides.coder]
enabled = false
tier = "strong"
tags = ["review"]
context_window = 131072
input_usd_per_mtok = 0.3
output_usd_per_mtok = 1.2
[models.overrides.ghost]
enabled = false
"#,
            ),
        ])
        .settings()
        .unwrap();
        let entry = &overridden.models.catalog[0];
        assert!(!entry.enabled);
        assert_eq!(entry.tier, Tier::Strong);
        assert_eq!(entry.tags, ["review"]);
        assert_eq!(entry.context_window, Some(131072));
        assert_eq!(
            overridden.endpoints["cloud"].pricing["coder-1"].input_usd_per_mtok,
            0.3
        );
        let untouched = config(&[(user(), table)]).settings().unwrap();
        assert!(untouched.models.catalog[0].enabled);
        // Prices go in pairs.
        assert!(
            config(&[
                (user(), table),
                (
                    app(),
                    "[models.overrides.coder]\ninput_usd_per_mtok = 1.0\n"
                ),
            ])
            .settings()
            .is_err()
        );
    }

    #[test]
    fn aliases_are_kept_per_model_and_checked() {
        let table = r#"
[[models.catalog]]
id = "coder"
spec = "cloud:coder-1"
location = "cloud"
tier = "basic"
alias = "Coder One"
[[models.catalog]]
id = "plain"
spec = "cloud:plain-1"
location = "cloud"
tier = "basic"
[endpoints.cloud]
base_url = "https://example.invalid/v1"
"#;
        // A row's own alias, then the app's override over it; the id is unchanged.
        let own = config(&[(user(), table)]).settings().unwrap();
        assert_eq!(own.models.catalog[0].alias.as_deref(), Some("Coder One"));
        assert_eq!(own.models.catalog[1].alias, None);
        let overridden = config(&[
            (user(), table),
            (
                app(),
                r#"
[models.overrides.coder]
alias = "Coder"
[models.overrides.plain]
alias = "Plain"
"#,
            ),
        ])
        .settings()
        .unwrap();
        assert_eq!(overridden.models.catalog[0].id, "coder");
        assert_eq!(overridden.models.catalog[0].alias.as_deref(), Some("Coder"));
        assert_eq!(overridden.models.catalog[1].alias.as_deref(), Some("Plain"));
        // A local model's alias sits with its other settings and leaves its launch alone.
        let id = "Qwen3-4B-Instruct-2507-Q4_K_M";
        let text = format!("[runtime.llamacpp.models.{id:?}]\nalias = \"Qwen 3 (4B)\"\n");
        let local = config(&[(app(), text.as_str())]).settings().unwrap();
        assert_eq!(
            local.runtime.llamacpp.models[id].alias.as_deref(),
            Some("Qwen 3 (4B)")
        );
        assert_eq!(
            local.launch_of(id),
            config(&[]).settings().unwrap().launch_of(id)
        );
        // Blank, padded, multi-line and overlong aliases are refused.
        let bad = |text: &str| config(&[(app(), text)]).settings().unwrap_err().to_string();
        for alias in ["", " ", " x", "a\nb"] {
            let text = format!("[runtime.llamacpp.models.x]\nalias = {alias:?}\n");
            assert!(bad(&text).contains("alias"), "{alias:?}");
        }
        let long = "x".repeat(super::MAX_ALIAS_CHARS + 1);
        assert!(bad(&format!("[models.overrides.coder]\nalias = {long:?}\n")).contains("alias"));
        let long_row = table.replace("\"Coder One\"", &format!("{long:?}"));
        let refused = config(&[(user(), long_row.as_str())])
            .settings()
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("alias"),
            "a row's own alias is checked too"
        );
    }

    #[test]
    fn cards_are_kept_per_model_and_checked() {
        let table = r#"
[[models.catalog]]
id = "coder"
spec = "cloud:coder-1"
location = "cloud"
tier = "basic"
card = "Good at: coding."
[endpoints.cloud]
base_url = "https://example.invalid/v1"
[models]
hidden = ["cloud:coder-1"]
list_models = "all"
"#;
        let own = config(&[(user(), table)]).settings().unwrap();
        assert_eq!(
            own.models.catalog[0].card.as_deref(),
            Some("Good at: coding.")
        );
        assert_eq!(own.models.hidden, ["cloud:coder-1"]);
        assert_eq!(own.models.list_models, crate::ListModelsScope::All);
        assert_eq!(
            config(&[]).settings().unwrap().models.list_models,
            crate::ListModelsScope::Root
        );
        // The app's card replaces the row's; a local model's card sits with its settings.
        let overridden = config(&[
            (user(), table),
            (
                app(),
                "[models.overrides.coder]\ncard = \"Good at: refactoring.\\nFast.\"\n[runtime.llamacpp.models.q]\ncard = \"Writing.\"\n",
            ),
        ])
        .settings()
        .unwrap();
        assert_eq!(
            overridden.models.catalog[0].card.as_deref(),
            Some("Good at: refactoring.\nFast.")
        );
        assert_eq!(
            overridden.runtime.llamacpp.models["q"].card.as_deref(),
            Some("Writing.")
        );
        let bad = |text: &str| config(&[(app(), text)]).settings().unwrap_err().to_string();
        for card in ["\" \"", "\"a\\u0007b\""] {
            let text = format!("[runtime.llamacpp.models.x]\ncard = {card}\n");
            assert!(bad(&text).contains("card"), "{card:?}");
        }
        let long = "x".repeat(super::MAX_CARD_CHARS + 1);
        assert!(bad(&format!("[models.overrides.coder]\ncard = {long:?}\n")).contains("card"));
    }

    #[test]
    fn invalid_values_are_refused_naming_the_key() {
        let bad = |text: &str| config(&[(app(), text)]).settings().unwrap_err().to_string();
        assert!(bad("[runtime.llamacpp.models.x]\nslots = 0\n").contains("slots"));
        assert!(bad("[runtime.llamacpp.models.x]\nslots = 257\n").contains("slots"));
        assert!(
            bad("[runtime.llamacpp.models.x]\ncache_type_v = \"nope\"\n").contains("cache_type_v")
        );
        assert!(
            bad("[runtime.llamacpp.models.x]\ncontext = 4096\npool_tokens = 1024\n")
                .contains("pool_tokens")
        );
        assert!(
            bad("[runtime.llamacpp.models.x]\ncache_type_v = \"q8_0\"\nflash_attn = \"off\"\n")
                .contains("flash_attn")
        );
        // The flags Imperium sets are refused whatever their spelling.
        for flag in [
            "--ctx-size",
            "--ctx-size=4096",
            "-c=4096",
            "--kv-unified",
            "-ngl",
        ] {
            let text = format!("[runtime.llamacpp.models.x]\nextra_args = [\"{flag}\"]\n");
            assert!(bad(&text).contains("extra_args"), "{flag}");
        }
        // A value alone is not a flag.
        assert!(
            config(&[(
                app(),
                "[runtime.llamacpp.models.x]\nextra_args = [\"--n-cpu-moe\", \"4096\"]\n"
            )])
            .settings()
            .is_ok()
        );
    }

    #[test]
    fn invalid_app_entries_are_dropped_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(
            paths.app_models_file(),
            r#"
[runtime.llamacpp.models.good]
slots = 4
[runtime.llamacpp.models.zero]
slots = 0
[runtime.llamacpp.models.typo]
cache_type_k = "nope"
[models.overrides.lonely]
input_usd_per_mtok = 1.0
[models.overrides.fine]
enabled = false
"#,
        )
        .unwrap();
        let config = load(&paths, None, []).unwrap();
        let settings = config.settings().unwrap();
        assert_eq!(settings.launch_of("good").slots, 4);
        assert_eq!(
            settings.launch_of("zero").slots,
            16,
            "dropped, so the default"
        );
        assert_eq!(settings.runtime.llamacpp.models.len(), 1);
        assert!(settings.models.overrides.contains_key("fine"));
        assert!(!settings.models.overrides.contains_key("lonely"));
        let warnings = config.warnings().join("\n");
        assert_eq!(config.warnings().len(), 3, "{warnings}");
        for id in ["zero", "typo", "lonely"] {
            assert!(warnings.contains(id), "{warnings}");
        }

        // A file that is not valid TOML is ignored with a warning, and never stops the load.
        std::fs::write(paths.app_models_file(), "this is = [not toml").unwrap();
        let config = load(&paths, None, []).unwrap();
        assert!(
            config
                .settings()
                .unwrap()
                .runtime
                .llamacpp
                .models
                .is_empty()
        );
        assert_eq!(config.warnings().len(), 1);
        assert!(
            config.warnings()[0].contains("app settings"),
            "{:?}",
            config.warnings()
        );

        // Without the file there is nothing to report.
        std::fs::remove_file(paths.app_models_file()).unwrap();
        assert!(load(&paths, None, []).unwrap().warnings().is_empty());
    }

    #[test]
    fn a_models_draft_settings_resolve_and_are_checked() {
        let settings = config(&[(
            user(),
            r#"
[runtime.llamacpp.models."Big"]
draft_model = "Small"
draft_max = 12
draft_min = 2
draft_gpu_layers = 0
"#,
        )])
        .settings()
        .unwrap();
        let draft = settings.launch_of("Big").draft.unwrap();
        assert_eq!(
            (draft.model.as_str(), draft.max, draft.min, draft.gpu_layers),
            ("Small", Some(12), Some(2), Some(0))
        );
        assert_eq!(
            settings.launch_of("other").draft,
            None,
            "off unless configured"
        );
        // A change of draft restarts a loaded model.
        let with = settings.launch_of("Big");
        let mut without = with.clone();
        without.draft = None;
        assert!(with.restarts_for(&without));

        let refused = |body: &str| {
            config(&[(
                user(),
                &format!("[runtime.llamacpp.models.\"Big\"]\n{body}\n"),
            )])
            .settings()
            .unwrap_err()
            .to_string()
        };
        assert!(refused("draft_max = 4").contains("draft_max needs draft_model"));
        assert!(refused("draft_model = \"Small\"\ndraft_max = 0").contains("at least 1"));
        assert!(
            refused("draft_model = \"Small\"\ndraft_max = 2\ndraft_min = 3")
                .contains("may not exceed")
        );
        assert!(refused("draft_model = \"Small\"\ndraft_min = 4").contains("llama.cpp's default"));
        assert!(refused("draft_model = \"Big\"").contains("the model itself"));
        // Every way to name or shape a draft is Imperium's, with or without `=value`.
        for flag in [
            "--spec-draft-p-min",
            "--spec-draft-future-flag",
            "--draft-p-split",
            "--device-draft",
            "--threads-batch-draft",
            "--override-tensor-draft",
            "-devd",
            "-otd",
            "-cmoed",
            "-ncmoed",
            "-td",
            "-tbd",
            "-md",
            "--model-draft",
            "--spec-draft-model",
            "--draft",
            "--draft-max",
            "--draft-n",
            "--spec-draft-n-max",
            "--draft-min",
            "--draft-n-min",
            "--spec-draft-n-min",
            "-ngld",
            "--gpu-layers-draft",
            "--n-gpu-layers-draft",
            "--spec-draft-ngl",
            "-hfd",
            "-hfrd",
            "--hf-repo-draft",
            "--spec-draft-hf",
            "-ctkd",
            "-ctvd",
            "--spec-draft-type-k",
            "--spec-draft-type-v",
        ] {
            let args = format!("extra_args = [\"{flag}=x\"]");
            assert!(refused(&args).contains("Imperium sets itself"), "{flag}");
        }
        // Flags Imperium has set since before the draft flags stay fine in the global
        // arguments, where they were never checked.
        let older = config(&[(
            user(),
            "[runtime.llamacpp]\nextra_args = [\"--flash-attn\", \"on\", \"--ctx-size=4096\", \"-ngl\", \"99\"]\n",
        )])
        .settings();
        assert!(older.is_ok(), "{older:?}");
        // ... but the draft flags are refused in the global arguments, which reach every model.
        let global = config(&[(
            user(),
            "[runtime.llamacpp]\nextra_args = [\"-md\", \"x.gguf\"]\n",
        )])
        .settings()
        .unwrap_err()
        .to_string();
        assert!(
            global.contains("-md") && global.contains("runtime.llamacpp"),
            "{global}"
        );
        // Without a draft, `--spec-type` is the user's (n-gram speculation, say); with one, in
        // the global arguments too, it would override the draft's.
        let ngram = "[runtime.llamacpp]\nextra_args = [\"--spec-type\", \"ngram-mod\"]\n";
        assert!(config(&[(user(), ngram)]).settings().is_ok());
        let both = config(&[(
            user(),
            &format!("{ngram}[runtime.llamacpp.models.\"Big\"]\ndraft_model = \"Small\"\n"),
        )])
        .settings()
        .unwrap_err()
        .to_string();
        assert!(both.contains("--spec-type"), "{both}");
        assert!(refused("draft_model = \" \"").contains("blank"));
        assert!(
            refused("draft_model = \"Small\"\nextra_args = [\"-md=x.gguf\"]")
                .contains("Imperium sets itself")
        );
        assert!(
            refused("draft_model = \"Small\"\nextra_args = [\"--spec-type\", \"ngram-mod\"]")
                .contains("--spec-type")
        );
    }
}
