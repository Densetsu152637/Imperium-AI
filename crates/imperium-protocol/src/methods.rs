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
    /// Where the session's commands run, such as `tier 0 · folder policy`.
    pub isolation: String,
    /// The session's first message, for lists.
    pub title: String,
    pub status: crate::session::SessionStatus,
    /// What it watches, when watching or merged.
    pub status_detail: Option<String>,
    pub resume_policy: crate::session::ResumePolicy,
    /// Started by a schedule or a project agent: it gives way to the user's own sessions.
    pub background: bool,
    /// Why background work is waiting (deferred before it started, or paused at a step
    /// boundary), until the machine is quiet or the user runs it now.
    pub hold: Option<SessionHold>,
}

/// A background session that is waiting for a quiet machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionHold {
    pub cause: crate::session::PauseCause,
    pub reason: String,
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
    pub isolation: String,
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
    /// The row's `context_window`, in tokens, when configured.
    #[serde(default)]
    pub context_window: Option<u32>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelRow {
    /// Use as `<runtime>:<id>` (`llamacpp:` or `vllm:`) in `[[models.catalog]]`.
    pub id: String,
    /// `llamacpp` for GGUF files, `vllm` for Hugging Face folders.
    #[ts(type = "\"llamacpp\" | \"vllm\"")]
    pub runtime: String,
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
    /// Calls waiting for a slot on this model.
    pub queued: u32,
    /// Pinned to the CPU lane by `placement = "cpu"` in the model table: served with no GPU
    /// layers, in system RAM only.
    #[serde(default)]
    pub cpu_lane: bool,
    /// How fast the model has generated in its recent calls, in tokens per second; absent
    /// before a call finished since the daemon started.
    #[serde(default)]
    pub tokens_per_s: Option<f64>,
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

/// `models.hf_files`: what a Hugging Face repository offers to download (GGUF quantisations and
/// a safetensors folder), with sizes and whether each would fit on this machine.
#[derive(Debug)]
pub struct HfFiles;

impl Method for HfFiles {
    const NAME: &'static str = "models.hf_files";
    type Params = HfFilesParams;
    type Result = HfFilesResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HfFilesParams {
    /// `owner/name`, such as `Qwen/Qwen3-8B-GGUF`.
    pub repo: String,
    /// A branch, tag, or commit; `main` when absent.
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HfFilesResult {
    pub repo: String,
    pub revision: String,
    /// GGUF quantisations in name order, then the safetensors folder when there is one.
    pub variants: Vec<HfVariant>,
    /// The variant `models.download` picks when given neither `quant` nor `file`.
    pub default: Option<String>,
    /// Multimodal projector files, downloaded with `mmproj: true`.
    pub projectors: Vec<HfFile>,
    /// Every file in the repository, for `models.download` with `file`.
    pub files: Vec<HfFile>,
    /// Where `models.download` would put the files (`<download_dir>/<owner>/<repo>`).
    pub download_dir: String,
}

/// One downloadable model in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HfVariant {
    /// The quantisation, such as `Q4_K_M`, or `safetensors`.
    pub name: String,
    #[ts(type = "\"llamacpp\" | \"vllm\"")]
    pub runtime: String,
    /// The id the model will have in `models.local`.
    pub model_id: String,
    /// Every file, all parts of a split set.
    pub files: Vec<String>,
    #[ts(type = "number")]
    pub size_bytes: u64,
    /// How it would load on this machine when nothing else is loaded, estimated from its size.
    #[ts(type = "\"gpu\" | \"partial\" | \"cpu\" | \"none\"")]
    pub fit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HfFile {
    pub path: String,
    #[ts(type = "number")]
    pub size_bytes: u64,
    /// SHA256 of large (LFS) files, checked after download.
    pub sha256: Option<String>,
}

/// `models.download`: starts downloading a model from Hugging Face into `models.download_dir`.
/// The download runs in the background, reported by `models.download_progress`; once done the
/// model is in `models.local`.
#[derive(Debug)]
pub struct ModelDownload;

impl Method for ModelDownload {
    const NAME: &'static str = "models.download";
    type Params = ModelDownloadParams;
    type Result = DownloadJob;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDownloadParams {
    /// `owner/name`.
    pub repo: String,
    /// A GGUF quantisation, matched without regard to case; `Q4_K_M` by default (or the only
    /// variant, or the safetensors folder).
    #[serde(default)]
    pub quant: Option<String>,
    /// A file by path or name instead of a quantisation; every part when it is split.
    #[serde(default)]
    pub file: Option<String>,
    /// A branch, tag, or commit; `main` when absent.
    #[serde(default)]
    pub revision: Option<String>,
    /// Also download the multimodal projector of a GGUF repository.
    #[serde(default)]
    pub mmproj: bool,
}

/// `models.downloads`: downloads started since the daemon started, oldest first.
#[derive(Debug)]
pub struct ModelDownloads;

impl Method for ModelDownloads {
    const NAME: &'static str = "models.downloads";
    type Params = ModelDownloadsParams;
    type Result = ModelDownloadsResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDownloadsParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDownloadsResult {
    pub jobs: Vec<DownloadJob>,
}

/// `models.download_cancel`: stops a running download. Its `.part` files stay, so downloading
/// the same model again resumes.
#[derive(Debug)]
pub struct ModelDownloadCancel;

impl Method for ModelDownloadCancel {
    const NAME: &'static str = "models.download_cancel";
    type Params = ModelDownloadCancelParams;
    type Result = ModelDownloadCancelResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDownloadCancelParams {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelDownloadCancelResult {
    /// False when no running download has that id.
    pub found: bool,
}

/// Notification sent to every client as a download progresses and when it ends. The payload is
/// a [`DownloadJob`].
pub const MODEL_DOWNLOAD_PROGRESS: &str = "models.download_progress";

/// Notification sent to every client when a local model is unloaded to make room for another
/// (or after `idle_unload_s`), with its slots saved to disk. The payload is a [`ModelSwap`].
pub const MODEL_SWAPPED_OUT: &str = "model.swapped_out";
/// Notification sent to every client when a local model is loaded in place of one swapped out,
/// or comes back with saved slots. The payload is a [`ModelSwap`].
pub const MODEL_SWAPPED_IN: &str = "model.swapped_in";

/// A local model swapped out or in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelSwap {
    pub model_id: String,
    /// Out: saving the slots and stopping the server. In: starting the server.
    pub seconds: f64,
    /// Out: bytes of slot state written to disk. In: memory the model took.
    #[ts(type = "number")]
    pub bytes: u64,
    /// Out: slots saved. In: saved slots waiting to be restored on their agents' next calls.
    pub slots: u32,
    /// Out: `room` (another model needed the memory) or `idle`. In: `swap` (it replaced a
    /// model) or `restore` (it came back with saved slots).
    pub reason: String,
}

/// A download and how far it has got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DownloadJob {
    pub job_id: String,
    pub repo: String,
    pub revision: String,
    /// The quantisation, `safetensors`, or the file asked for.
    pub variant: String,
    /// What to use in `[[models.catalog]]` or `--model` once it is done, such as
    /// `llamacpp:Qwen3-8B-Q4_K_M`.
    pub model: String,
    pub files: Vec<String>,
    /// The folder the files go in.
    pub dir: String,
    /// The file being downloaded.
    pub file: Option<String>,
    #[ts(type = "number")]
    pub file_done_bytes: u64,
    #[ts(type = "number")]
    pub file_total_bytes: u64,
    /// Across every file.
    #[ts(type = "number")]
    pub done_bytes: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "\"running\" | \"done\" | \"failed\" | \"cancelled\"")]
    pub state: String,
    /// Why it failed.
    pub message: Option<String>,
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
    /// Set while a run waits for a quiet machine (`prefer_quiet`, `when_quiet`): why. Run it
    /// now with `schedules.run`.
    pub deferred: Option<String>,
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
    #[ts(type = "\"deferred\" | \"started\" | \"completed\" | \"failed\"")]
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

/// `pet.list`: installed pets, the selected one, and its current state.
#[derive(Debug)]
pub struct PetList;

impl Method for PetList {
    const NAME: &'static str = "pet.list";
    type Params = PetListParams;
    type Result = PetListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetListResult {
    pub pets: Vec<PetInfo>,
    /// None when pets are off or none is installed.
    pub selected: Option<String>,
    pub state: Option<PetStateInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub cell_width: u32,
    pub cell_height: u32,
}

/// Notification sent to every client when the pet's animation changes.
pub const PET_STATE: &str = "pet.state";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetStateInfo {
    pub pet_id: String,
    pub animation: String,
    pub frames: u32,
    /// Per-frame durations in milliseconds.
    pub durations: Vec<u32>,
    pub looping: bool,
    /// When the animation started, so every client draws it in phase.
    #[ts(type = "number")]
    pub since_unix_ms: u64,
    pub hidden: bool,
}

/// `pet.strip`: the selected pet's strip image for an animation.
#[derive(Debug)]
pub struct PetStrip;

impl Method for PetStrip {
    const NAME: &'static str = "pet.strip";
    type Params = PetStripParams;
    type Result = PetStripResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetStripParams {
    pub animation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetStripResult {
    /// The strip PNG, base64-encoded.
    pub png_base64: String,
}

/// `pet.interact`: the user patted or clicked the pet.
#[derive(Debug)]
pub struct PetInteract;

impl Method for PetInteract {
    const NAME: &'static str = "pet.interact";
    type Params = PetInteractParams;
    type Result = PetInteractResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetInteractParams {
    /// `pat`, `hide`, or `show`.
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetInteractResult {}

/// `phone.pair`: a one-time code a phone enters to pair with this computer.
#[derive(Debug)]
pub struct PhonePair;

impl Method for PhonePair {
    const NAME: &'static str = "phone.pair";
    type Params = PhonePairParams;
    type Result = PhonePairResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhonePairParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhonePairResult {
    pub pairing: PairingCode,
    /// Where the phone app is served, when the phone listener is on.
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PairingCode {
    pub code: String,
    #[ts(type = "number")]
    pub expires_in_s: u64,
}

/// `phone.devices`: paired phones.
#[derive(Debug)]
pub struct PhoneDevices;

impl Method for PhoneDevices {
    const NAME: &'static str = "phone.devices";
    type Params = PhoneDevicesParams;
    type Result = PhoneDevicesResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneDevicesParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneDevicesResult {
    pub devices: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    /// `read`, `approve`.
    pub scopes: Vec<String>,
    pub paired_at: String,
}

/// `phone.revoke`: unpair a phone.
#[derive(Debug)]
pub struct PhoneRevoke;

impl Method for PhoneRevoke {
    const NAME: &'static str = "phone.revoke";
    type Params = PhoneRevokeParams;
    type Result = PhoneRevokeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneRevokeParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneRevokeResult {
    pub found: bool,
}

/// Notification sent to every client when a session starts, whoever started it (a client, a
/// schedule, a project agent, or auto-fix). The payload is a [`SessionSummary`].
pub const SESSION_CREATED: &str = "session.created";

/// `session.control`: change how a session waits for usage resets, resume it now, or set what
/// pull request it watches.
#[derive(Debug)]
pub struct SessionControl;

impl Method for SessionControl {
    const NAME: &'static str = "session.control";
    type Params = SessionControlParams;
    type Result = SessionControlResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionControlParams {
    pub session_id: String,
    /// `set_resume_policy` (with `resume_policy`, or none to use the default), `resume_now`,
    /// `watch` (with `repo` and `pull_request`), or `unwatch`.
    #[ts(type = "\"set_resume_policy\" | \"resume_now\" | \"watch\" | \"unwatch\"")]
    pub action: String,
    #[serde(default)]
    pub resume_policy: Option<crate::session::ResumePolicy>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub pull_request: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionControlResult {}
