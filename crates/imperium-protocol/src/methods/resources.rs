use super::*;

/// `system.resources`: what the machine and the sessions are using right now.
#[derive(Debug)]
pub struct SystemResources;

impl Method for SystemResources {
    const NAME: &'static str = "system.resources";
    type Params = SystemResourcesParams;
    type Result = ResourceSnapshot;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemResourcesParams {}

/// `system.resources_subscribe`: start (`on: true`) or stop `system.resources_changed`
/// notifications on this connection. The daemon samples only while a connection is subscribed;
/// closing the connection unsubscribes it.
#[derive(Debug)]
pub struct SystemResourcesSubscribe;

impl Method for SystemResourcesSubscribe {
    const NAME: &'static str = "system.resources_subscribe";
    type Params = SystemResourcesSubscribeParams;
    type Result = SystemResourcesSubscribeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemResourcesSubscribeParams {
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemResourcesSubscribeResult {
    pub on: bool,
    /// How often snapshots arrive while subscribed.
    pub interval_ms: u32,
}

/// `events.subscribe`: choose which notification topics this connection receives. A new
/// connection receives every notification it may; the first subscription narrows that to the
/// topics named, and later ones add to them. A topic is a notification method such as
/// `session.event` or `model.residency`, or a prefix ending in `.*` such as `session.*`; `*`
/// alone means everything again. A subscription only narrows: it never delivers anything the
/// connection could not already receive, so naming a topic a paired phone may not see changes
/// nothing. An empty list changes nothing and returns the current choice.
#[derive(Debug)]
pub struct EventsSubscribe;

impl Method for EventsSubscribe {
    const NAME: &'static str = "events.subscribe";
    type Params = EventsSubscribeParams;
    type Result = EventsSelection;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventsSubscribeParams {
    #[serde(default)]
    pub topics: Vec<String>,
}

/// `events.unsubscribe`: stop receiving notification topics. Topics that were subscribed are
/// removed; on a connection still receiving everything they are muted. Unsubscribing every
/// topic leaves a connection with only replies.
#[derive(Debug)]
pub struct EventsUnsubscribe;

impl Method for EventsUnsubscribe {
    const NAME: &'static str = "events.unsubscribe";
    type Params = EventsUnsubscribeParams;
    type Result = EventsSelection;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventsUnsubscribeParams {
    #[serde(default)]
    pub topics: Vec<String>,
}

/// Which notification topics a connection receives now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventsSelection {
    /// The subscribed topics, or `None` while the connection still receives everything.
    pub topics: Option<Vec<String>>,
    /// Topics withheld from what `topics` (or everything) would deliver.
    pub muted: Vec<String>,
}

/// Whether `topic` is a well-formed subscription: a dotted lowercase method name, a prefix such
/// as `session.*`, or `*`.
pub fn is_topic(topic: &str) -> bool {
    if topic == "*" {
        return true;
    }
    let name = topic.strip_suffix(".*").unwrap_or(topic);
    !name.is_empty()
        && name.len() <= 64
        && name.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

/// Whether a subscription `pattern` covers the notification `method`.
pub fn topic_matches(pattern: &str, method: &str) -> bool {
    match pattern.strip_suffix(".*") {
        Some(prefix) => method
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.')),
        None => pattern == "*" || pattern == method,
    }
}

/// Notification carrying a [`ResourceSnapshot`], sent about every two seconds to connections
/// subscribed with `system.resources_subscribe`.
pub const RESOURCES_CHANGED: &str = "system.resources_changed";

/// What the machine and the sessions are using.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResourceSnapshot {
    /// When it was taken, in milliseconds since the Unix epoch.
    #[ts(type = "number")]
    pub taken_at_ms: u64,
    pub cpu: CpuUsage,
    pub memory: MemoryUsage,
    /// Empty when no supported GPU (NVIDIA, through its driver) is present.
    pub gpus: Vec<GpuUsage>,
    /// The daemon itself, then each model server it runs.
    pub processes: Vec<ProcessUsage>,
    /// Local models loaded now, in use or idle (cached until they unload).
    pub local_models: Vec<ActiveLocalModel>,
    /// Hosted endpoints that live sessions use, with their last 15 minutes of calls.
    pub endpoints: Vec<ActiveEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CpuUsage {
    /// Busy share of every core together, 0 to 100.
    pub total_percent: f32,
    /// Busy share of each logical core, 0 to 100.
    pub per_core: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MemoryUsage {
    #[ts(type = "number")]
    pub ram_used_bytes: u64,
    #[ts(type = "number")]
    pub ram_total_bytes: u64,
    #[ts(type = "number")]
    pub swap_used_bytes: u64,
    #[ts(type = "number")]
    pub swap_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GpuUsage {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    /// Busy share, 0 to 100; absent when the driver does not report it.
    pub utilization_percent: Option<f32>,
    #[ts(type = "number")]
    pub vram_used_bytes: u64,
    #[ts(type = "number")]
    pub vram_total_bytes: u64,
    pub temperature_c: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProcessUsage {
    pub pid: u32,
    pub name: String,
    /// `daemon` for imperium-core, otherwise the runtime serving `model` (`llamacpp`, `vllm`).
    pub role: String,
    pub model: Option<String>,
    /// Share of the whole machine (every core), 0 to 100.
    pub cpu_percent: f32,
    #[ts(type = "number")]
    pub ram_bytes: u64,
    /// Absent when the driver does not report GPU memory per process.
    #[ts(type = "number | null")]
    pub vram_bytes: Option<u64>,
}

/// A local model loaded by the supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActiveLocalModel {
    pub model: String,
    /// The runtime serving it: `llamacpp` or `vllm`.
    pub runtime: String,
    /// `gpu`, `partial` (layers split between GPU and RAM), or `cpu`.
    #[ts(type = "\"gpu\" | \"partial\" | \"cpu\"")]
    pub placement: String,
    pub gpu_layers: u32,
    /// Tokens one session may use.
    pub context: u32,
    /// Tokens of KV cache its slots share (the KV pool).
    pub pool_tokens: u32,
    /// Of those, tokens reserved by calls running now.
    pub pool_reserved_tokens: u32,
    /// Memory planned for it.
    #[ts(type = "number")]
    pub vram_bytes: u64,
    #[ts(type = "number")]
    pub ram_bytes: u64,
    pub slots: u32,
    pub slots_in_use: u32,
    /// Calls waiting for a slot or for room in the pool.
    pub queued: u32,
    /// Pinned to the CPU lane.
    pub cpu_pinned: bool,
    /// The draft model it runs with for speculative decoding; its memory is in `vram_bytes` and
    /// `ram_bytes`.
    #[serde(default)]
    pub draft_model: Option<String>,
    /// Why a configured draft model is not in use: the model runs without it.
    #[serde(default)]
    pub draft_notice: Option<String>,
    /// An agent is mid-turn on it (between its calls), so it stays loaded.
    pub claimed: bool,
    /// Live sessions on this model: as their model, or with calls on it in the last 15 minutes.
    pub sessions: Vec<String>,
    pub pid: Option<u32>,
    /// How long it has been idle; absent while it is in use (a slot in use, a call queued, or
    /// an agent mid-turn on it).
    #[ts(type = "number | null")]
    pub idle_ms: Option<u64>,
    /// When it unloads if it stays idle; absent while it is in use.
    #[ts(type = "number | null")]
    pub unload_in_ms: Option<u64>,
    /// `models.unload` can release it now (nothing is using it).
    pub can_unload: bool,
}

/// A hosted model live sessions use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActiveEndpoint {
    /// The `[endpoints.<id>]` it is served from.
    pub endpoint: String,
    /// The endpoint's host, from its base URL.
    pub host: Option<String>,
    pub model: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    pub sessions: Vec<String>,
    /// Model calls in the last 15 minutes.
    pub requests: u32,
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    /// How long the latest call took.
    #[ts(type = "number | null")]
    pub last_latency_ms: Option<u64>,
    /// Failed turns on this model in the last 15 minutes, and the latest message.
    pub errors: u32,
    pub last_error: Option<String>,
}

/// `models.unload`: unload an idle local model now, freeing its memory.
#[derive(Debug)]
pub struct ModelUnload;

impl Method for ModelUnload {
    const NAME: &'static str = "models.unload";
    type Params = ModelUnloadParams;
    type Result = ModelUnloadResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUnloadParams {
    /// The local model id, as `local_models[].model` names it.
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUnloadResult {
    pub unloaded: bool,
}
