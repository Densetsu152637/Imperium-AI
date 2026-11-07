//! Typed method definitions. Each method names its wire string and its params/result types.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ProtocolVersion;
use crate::session::PermissionDecision;

/// A daemon method: its wire name and the shapes of its params and result.
pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;
}

/// `system.handshake`: must be the first call on every connection.
#[derive(Debug)]
pub struct Handshake;

impl Method for Handshake {
    const NAME: &'static str = "system.handshake";
    type Params = HandshakeParams;
    type Result = HandshakeResult;
}

/// `system.ping`: a liveness check.
#[derive(Debug)]
pub struct Ping;

impl Method for Ping {
    const NAME: &'static str = "system.ping";
    type Params = PingParams;
    type Result = PingResult;
}

/// Names and versions a program, such as the CLI or the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Peer {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HandshakeParams {
    pub client: Peer,
    pub protocol: ProtocolVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HandshakeResult {
    pub daemon: Peer,
    pub protocol: ProtocolVersion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingResult {
    /// Milliseconds since the daemon started.
    #[ts(type = "number")]
    pub uptime_ms: u64,
}

/// `session.create`: starts a session in a project folder. The calling connection receives the
/// session's `session.event` notifications.
#[derive(Debug)]
pub struct SessionCreate;

impl Method for SessionCreate {
    const NAME: &'static str = "session.create";
    type Params = SessionCreateParams;
    type Result = SessionCreateResult;
}

/// `session.send`: sends a user message. Messages sent during a turn run after it finishes.
#[derive(Debug)]
pub struct SessionSend;

impl Method for SessionSend {
    const NAME: &'static str = "session.send";
    type Params = SessionSendParams;
    type Result = Accepted;
}

/// `session.respond`: answers a `permission_requested` event.
#[derive(Debug)]
pub struct SessionRespond;

impl Method for SessionRespond {
    const NAME: &'static str = "session.respond";
    type Params = SessionRespondParams;
    type Result = Accepted;
}

/// `session.list`: the sessions running in this daemon.
#[derive(Debug)]
pub struct SessionList;

impl Method for SessionList {
    const NAME: &'static str = "session.list";
    type Params = SessionListParams;
    type Result = SessionListResult;
}

/// `session.subscribe`: receive a session's `session.event` notifications on this connection.
#[derive(Debug)]
pub struct SessionSubscribe;

impl Method for SessionSubscribe {
    const NAME: &'static str = "session.subscribe";
    type Params = SessionSubscribeParams;
    type Result = SessionSubscribeResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSummary {
    pub session_id: String,
    pub project_dir: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSubscribeParams {
    pub session_id: String,
}

/// Every event the session has emitted so far; later ones arrive as notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSubscribeResult {
    pub history: Vec<crate::session::AgentEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateParams {
    /// Absolute path of the project folder the session works in.
    pub project_dir: String,
    /// `<endpoint>:<model>`; defaults to `models.default` from the configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateResult {
    pub session_id: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSendParams {
    pub session_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionRespondParams {
    pub session_id: String,
    #[ts(type = "number")]
    pub request_id: u64,
    pub decision: PermissionDecision,
}

/// An empty acknowledgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Accepted {}

/// `models.list`: the model table that delegation rules and `spawn_agent` refer to.
#[derive(Debug)]
pub struct ModelList;

impl Method for ModelList {
    const NAME: &'static str = "models.list";
    type Params = ModelListParams;
    type Result = ModelListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelListParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelListResult {
    pub models: Vec<ModelRow>,
    /// `routing.prefer`: how ties between suitable models are broken.
    #[ts(type = "\"cheapest\" | \"local\" | \"cloud\"")]
    pub prefer: String,
    /// Number of `[[routing.rules]]` entries.
    pub rules: u32,
}

/// One row of the model table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelRow {
    pub id: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    #[ts(type = "\"local\" | \"cloud\"")]
    pub location: String,
    #[ts(type = "\"basic\" | \"standard\" | \"strong\" | \"frontier\"")]
    pub tier: String,
    pub tags: Vec<String>,
    pub input_usd_per_mtok: Option<f64>,
    pub output_usd_per_mtok: Option<f64>,
    /// False when the row names an endpoint that is not configured.
    pub endpoint_configured: bool,
}

/// `models.local`: models found in `models.folders`, the hardware they are planned against, and
/// which are loaded.
#[derive(Debug)]
pub struct LocalModelList;

impl Method for LocalModelList {
    const NAME: &'static str = "models.local";
    type Params = LocalModelListParams;
    type Result = LocalModelListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelListResult {
    /// False when no `models.folders` are configured.
    pub enabled: bool,
    pub folders: Vec<String>,
    pub models: Vec<LocalModelRow>,
    /// Files that looked like models but could not be used, with the reason.
    pub skipped: Vec<SkippedModel>,
    pub hardware: Option<HardwareInfo>,
    pub runtime: Option<RuntimeInfo>,
}

/// A GGUF model found in a model folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelRow {
    /// Use as `llamacpp:<id>` in `[[models.catalog]]`.
    pub id: String,
    pub path: String,
    pub architecture: Option<String>,
    pub quantization: Option<String>,
    pub parts: u32,
    #[ts(type = "number")]
    pub size_bytes: u64,
    #[ts(type = "number | null")]
    pub context_length: Option<u64>,
    /// Memory needed at the configured context and slots.
    #[ts(type = "number")]
    pub required_bytes: u64,
    /// How the model would load on this machine when nothing else is loaded.
    #[ts(type = "\"gpu\" | \"partial\" | \"cpu\" | \"none\"")]
    pub placement: String,
    pub gpu_layers: Option<u32>,
    pub loaded: bool,
    pub slots: u32,
    pub slots_in_use: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SkippedModel {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HardwareInfo {
    pub gpus: Vec<GpuInfo>,
    #[ts(type = "number")]
    pub ram_total_bytes: u64,
    #[ts(type = "number")]
    pub ram_available_bytes: u64,
    pub cpu_cores: u32,
    pub unified_memory: bool,
    /// Some values come from `[hardware]` settings rather than the probe.
    pub overridden: bool,
    /// Memory local models may use: free VRAM, and free RAM less the headroom.
    #[ts(type = "number")]
    pub budget_vram_bytes: u64,
    #[ts(type = "number")]
    pub budget_ram_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: String,
    #[ts(type = "number")]
    pub vram_total_bytes: u64,
    #[ts(type = "number")]
    pub vram_free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeInfo {
    /// The runtime pack in use, such as `windows-x64-cuda13`, or `custom`.
    pub pack: String,
    pub server_path: String,
    pub installed: bool,
    pub llama_cpp_ref: String,
}

/// `mods.list`: UI mods installed in `<config>/mods`, with their entry modules.
#[derive(Debug)]
pub struct ModList;

impl Method for ModList {
    const NAME: &'static str = "mods.list";
    type Params = ModListParams;
    type Result = ModListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModListResult {
    pub mods: Vec<ModInfo>,
    /// Mods that could not be loaded, with the reason.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `telemetry:read`, `sessions:read`; the desktop app forwards only what is granted.
    pub permissions: Vec<String>,
    pub panels: Vec<ModPanel>,
    /// The entry ES module's source.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModPanel {
    pub id: String,
    pub title: String,
}

/// `schedules.list`: configured schedules with their next and last runs.
#[derive(Debug)]
pub struct ScheduleList;

impl Method for ScheduleList {
    const NAME: &'static str = "schedules.list";
    type Params = ScheduleListParams;
    type Result = ScheduleListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleListResult {
    pub schedules: Vec<ScheduleRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRow {
    pub id: String,
    pub enabled: bool,
    /// The trigger as written, such as `cron 0 8 * * MON-FRI`.
    pub trigger: String,
    /// `prompt` or `command`.
    pub action: String,
    pub project: String,
    /// RFC 3339 local times.
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub last_ok: Option<bool>,
    pub running: bool,
    /// Why the schedule cannot run, if it cannot.
    pub problem: Option<String>,
}

/// `schedules.run`: start a schedule now.
#[derive(Debug)]
pub struct ScheduleRun;

impl Method for ScheduleRun {
    const NAME: &'static str = "schedules.run";
    type Params = ScheduleRunParams;
    type Result = ScheduleRunResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRunParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleRunResult {
    /// False when the schedule was already running and does not allow overlap.
    pub started: bool,
}

/// Notification sent to every client when a scheduled run starts or ends.
pub const SCHEDULE_EVENT: &str = "schedule.event";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleEventParams {
    pub schedule_id: String,
    #[ts(type = "\"started\" | \"completed\" | \"failed\"")]
    pub status: String,
    /// The session a prompt run created.
    pub session_id: Option<String>,
    pub summary: String,
}

/// Notification sent to every client when unattended work needs a yes or no.
pub const APPROVAL_REQUESTED: &str = "approval.requested";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalInfo {
    #[ts(type = "number")]
    pub id: u64,
    /// What is being approved, such as `push`.
    pub kind: String,
    pub summary: String,
    pub requested_at: String,
}

/// `approvals.list`: pending approval requests.
#[derive(Debug)]
pub struct ApprovalList;

impl Method for ApprovalList {
    const NAME: &'static str = "approvals.list";
    type Params = ApprovalListParams;
    type Result = ApprovalListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalListResult {
    pub approvals: Vec<ApprovalInfo>,
}

/// `approvals.respond`: approve or decline a pending request.
#[derive(Debug)]
pub struct ApprovalRespond;

impl Method for ApprovalRespond {
    const NAME: &'static str = "approvals.respond";
    type Params = ApprovalRespondParams;
    type Result = ApprovalRespondResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalRespondParams {
    #[ts(type = "number")]
    pub id: u64,
    pub approve: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalRespondResult {
    /// False when the request was already answered or never existed.
    pub found: bool,
}

/// Notification sent to every client as auto-fix works on a pull request.
pub const AUTOFIX_EVENT: &str = "autofix.event";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AutofixEventParams {
    pub repo: String,
    #[ts(type = "number")]
    pub pull_request: u64,
    /// `started`, `awaiting_approval`, `pushed`, `declined`, `nothing`, `failed`, or `stopped`.
    pub status: String,
    pub session_id: Option<String>,
    pub summary: String,
}

/// `projects.list`: projects with a running project agent.
#[derive(Debug)]
pub struct ProjectList;

impl Method for ProjectList {
    const NAME: &'static str = "projects.list";
    type Params = ProjectListParams;
    type Result = ProjectListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectListResult {
    pub agents: Vec<ProjectAgentRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectAgentRow {
    pub project: String,
    /// The agent's own session, which can be opened like any other.
    pub session_id: String,
}

/// `projects.message`: wake a project agent with a message.
#[derive(Debug)]
pub struct ProjectMessage;

impl Method for ProjectMessage {
    const NAME: &'static str = "projects.message";
    type Params = ProjectMessageParams;
    type Result = ProjectMessageResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectMessageParams {
    pub project: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectMessageResult {
    /// False when the project has no running agent.
    pub delivered: bool,
}
