//! Typed method definitions. Each method names its wire string and its params/result types.

use std::collections::BTreeMap;

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

/// `system.shutdown`: stops the daemon gracefully. It replies first, then stops serving; clients
/// see their connection close.
#[derive(Debug)]
pub struct Shutdown;

impl Method for Shutdown {
    const NAME: &'static str = "system.shutdown";
    type Params = ShutdownParams;
    type Result = ShutdownResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShutdownParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShutdownResult {
    /// The daemon's process id, so a caller can wait for it to exit.
    pub pid: u32,
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

/// `session.send`: sends a user message to the root agent or one of its running subagents.
/// While the agent is busy (a turn running, paused on a usage limit, or waiting for a
/// permission answer) the message is queued and runs as a new turn after the current one; with
/// `mode: "steer"` it joins the running turn instead. See `message_queued` and `steered`.
#[derive(Debug)]
pub struct SessionSend;

impl Method for SessionSend {
    const NAME: &'static str = "session.send";
    type Params = SessionSendParams;
    type Result = SessionSendResult;
}

/// `session.queue`: list, edit, or remove an agent's queued messages before they are
/// delivered.
#[derive(Debug)]
pub struct SessionQueue;

impl Method for SessionQueue {
    const NAME: &'static str = "session.queue";
    type Params = SessionQueueParams;
    type Result = SessionQueueResult;
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
    /// The session's working directory: its environment's first folder.
    pub project_dir: String,
    /// The environment the session works in, when it has one (sessions a schedule, a project
    /// agent, or auto-fix start work in a bare folder).
    #[serde(default)]
    pub environment_id: Option<String>,
    /// Every folder the session's tools may use, the working directory first.
    #[serde(default)]
    pub folders: Vec<String>,
    pub model: String,
    /// Where the session's commands run, such as `tier 0 · folder policy`.
    pub isolation: String,
    /// The session's first message, for lists.
    pub title: String,
    pub status: crate::session::SessionStatus,
    /// What it watches, when watching or merged.
    pub status_detail: Option<String>,
    pub resume_policy: crate::session::ResumePolicy,
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
    /// The environment the session works in: its folders are the sandbox, the first is the
    /// working directory. Give this or `project_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment_id: Option<String>,
    /// Absolute path of a folder to work in, for clients that predate environments. It maps to
    /// the environment whose first folder it is, created (named after the folder) if needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_dir: Option<String>,
    /// A model table id or `<endpoint>:<model>`; defaults to `models.default` from the
    /// configuration.
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
    /// The environment the session works in.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// The session's folders, the working directory first.
    #[serde(default)]
    pub folders: Vec<String>,
}

/// An environment: a named set of folders sessions work in. The folders are the sandbox the
/// agent's tools may use; the first is the working directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentInfo {
    pub id: String,
    pub name: String,
    /// A glyph name for the interface's icon, such as `code`; the interface picks one if unset.
    #[serde(default)]
    pub icon: Option<String>,
    /// The icon's tile colour, such as `#2f6fdb`.
    #[serde(default)]
    pub color: Option<String>,
    /// Absolute folder paths, the working directory first.
    pub folders: Vec<String>,
}

/// `environments.list`: every environment, in the order they were created.
#[derive(Debug)]
pub struct EnvironmentList;

impl Method for EnvironmentList {
    const NAME: &'static str = "environments.list";
    type Params = EnvironmentListParams;
    type Result = EnvironmentListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentListResult {
    pub environments: Vec<EnvironmentInfo>,
}

/// `environments.create`: a new environment, optionally with its first folders.
#[derive(Debug)]
pub struct EnvironmentCreate;

impl Method for EnvironmentCreate {
    const NAME: &'static str = "environments.create";
    type Params = EnvironmentCreateParams;
    type Result = EnvironmentInfo;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentCreateParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    /// Existing folders to start with (may be empty).
    #[serde(default)]
    pub folders: Vec<String>,
}

/// `environments.update`: rename an environment or change its icon. Fields left out keep
/// their value; an empty `icon` or `color` clears it.
#[derive(Debug)]
pub struct EnvironmentUpdate;

impl Method for EnvironmentUpdate {
    const NAME: &'static str = "environments.update";
    type Params = EnvironmentUpdateParams;
    type Result = EnvironmentInfo;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentUpdateParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
}

/// `environments.delete`: forget an environment. Its folders and running sessions are left
/// alone.
#[derive(Debug)]
pub struct EnvironmentDelete;

impl Method for EnvironmentDelete {
    const NAME: &'static str = "environments.delete";
    type Params = EnvironmentRef;
    type Result = Accepted;
}

/// Names one environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentRef {
    pub id: String,
}

/// `environments.add_folder`: add an existing folder to an environment. Sessions started from
/// then on may use it.
#[derive(Debug)]
pub struct EnvironmentAddFolder;

impl Method for EnvironmentAddFolder {
    const NAME: &'static str = "environments.add_folder";
    type Params = EnvironmentFolderParams;
    type Result = EnvironmentInfo;
}

/// `environments.remove_folder`: remove a folder from an environment.
#[derive(Debug)]
pub struct EnvironmentRemoveFolder;

impl Method for EnvironmentRemoveFolder {
    const NAME: &'static str = "environments.remove_folder";
    type Params = EnvironmentFolderParams;
    type Result = EnvironmentInfo;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentFolderParams {
    pub id: String,
    /// An absolute folder path.
    pub folder: String,
}

/// Notification sent to every client when environments change; the payload is an
/// [`EnvironmentListResult`].
pub const ENVIRONMENTS_CHANGED: &str = "environments.changed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSendParams {
    pub session_id: String,
    pub text: String,
    /// `queue` (the default) or `steer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<crate::session::SendMode>,
    /// A running subagent's task id (`t1`, ...); the root agent when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSendResult {
    /// The message's id; when it waits, `message_queued` carries the same id.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionQueueParams {
    pub session_id: String,
    /// `list`, `edit` (with `id` and `text`), or `remove` (with `id`). Editing or removing a
    /// message that was already delivered fails.
    #[ts(type = "\"list\" | \"edit\" | \"remove\"")]
    pub action: String,
    /// A running subagent's task id; the root agent when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}

/// The agent's queue after the action, in delivery order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionQueueResult {
    pub messages: Vec<crate::session::QueuedMessage>,
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
    /// `telemetry:read`, `sessions:read`, `pets:read`, `storage`; the desktop app grants only these.
    pub permissions: Vec<String>,
    pub panels: Vec<ModPanel>,
    /// Shipped with Imperium (opt-in in the desktop app) rather than installed by the user.
    pub bundled: bool,
    /// The entry ES module's source.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModPanel {
    pub id: String,
    pub title: String,
    /// Where the panel docks: `sidebar.left` or `sidebar.right` (the default).
    #[serde(default)]
    pub slot: Option<String>,
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

/// `pet.list`: installed pets, the selected one, and its current state. Installed pets are
/// listed even when `pets.enabled` is false; `selected` and `state` are then null.
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
    /// Animations the pet draws (a strip exists for each).
    pub drawn: Vec<String>,
    /// The package's `pet.json`, unparsed.
    pub pet_json: String,
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

/// `pet.strip`: a pet's strip image for an animation. Works without the pet engine, but then
/// `pet_id` is required (there is no selected pet).
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
    /// The pet to draw; the selected pet when absent.
    #[serde(default)]
    pub pet_id: Option<String>,
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

/// `session.suggestion_respond`: answers a `model_suggestion` event. `download` starts the
/// download (as `models.download` would) and returns its job; `dismiss` says not now (the
/// session does not suggest for that need again); `never` turns `models.suggest_downloads` off
/// in the user config file. Each is followed by a `model_suggestion_update` event.
#[derive(Debug)]
pub struct SessionSuggestionRespond;

impl Method for SessionSuggestionRespond {
    const NAME: &'static str = "session.suggestion_respond";
    type Params = SuggestionRespondParams;
    type Result = SuggestionRespondResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SuggestionRespondParams {
    pub session_id: String,
    pub suggestion_id: String,
    #[ts(type = "\"download\" | \"dismiss\" | \"never\"")]
    pub action: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SuggestionRespondResult {
    /// For `download`: the job, whose progress `models.download_progress` reports.
    #[serde(default)]
    pub job: Option<DownloadJob>,
}

/// Notification sent to every client when a session starts, whoever started it (a client, a
/// schedule, a project agent, or auto-fix). The payload is a [`SessionSummary`].
pub const SESSION_CREATED: &str = "session.created";

/// `session.control`: change how a session waits for usage resets, resume it now, set what
/// pull request it watches, switch its model, or compact its conversation.
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
    /// `watch` (with `repo` and `pull_request`), `unwatch`, `set_model` (with `model`, or none
    /// for `models.default`; refused while a turn runs), or `compact` (summarise older turns
    /// now; refused while a turn runs).
    #[ts(
        type = "\"set_resume_policy\" | \"resume_now\" | \"watch\" | \"unwatch\" | \"set_model\" | \"compact\""
    )]
    pub action: String,
    #[serde(default)]
    pub resume_policy: Option<crate::session::ResumePolicy>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub pull_request: Option<u64>,
    /// For `set_model`: a model table id or an `<endpoint>:<model>` spec.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionControlResult {
    /// For `set_model` and `compact`: the session's model afterwards.
    #[serde(default)]
    pub model: Option<String>,
    /// For `set_model` and `compact`: the conversation measured for that model's window.
    #[serde(default)]
    pub context: Option<ContextFit>,
}

/// A conversation measured against a model's context window, and any compaction it needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextFit {
    /// The conversation's size as the model will read it (after any compaction).
    #[ts(type = "number")]
    pub tokens: u64,
    /// Absent when the window is unknown; the conversation was then not checked.
    #[ts(type = "number | null")]
    pub context_window: Option<u64>,
    /// The tokens the conversation may use: the window less room for the reply.
    #[ts(type = "number | null")]
    pub budget: Option<u64>,
    pub method: crate::session::TokenCountMethod,
    /// Present when older turns were summarised to fit.
    pub compaction: Option<Compaction>,
}

/// What compacting a conversation did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Compaction {
    #[ts(type = "number")]
    pub before_tokens: u64,
    #[ts(type = "number")]
    pub after_tokens: u64,
    pub summarised_messages: u32,
    /// The model that wrote the summary.
    pub summary_model: String,
}

/// `config.describe`: the settings an app may show and change, with their effective values.
/// Structured lists and maps (endpoints, the model table, routing rules, MCP servers, projects,
/// schedules, hooks) are not included; they are edited in the config file.
#[derive(Debug)]
pub struct ConfigDescribe;

impl Method for ConfigDescribe {
    const NAME: &'static str = "config.describe";
    type Params = ConfigDescribeParams;
    type Result = ConfigDescribeResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigDescribeParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigDescribeResult {
    pub entries: Vec<ConfigEntry>,
    /// The user config file that `config.set` writes.
    pub user_file: String,
}

/// One setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigEntry {
    /// Dotted path, such as `runtime.llamacpp.context`.
    pub key: String,
    /// The top-level table, for grouping.
    pub section: String,
    pub label: String,
    pub description: String,
    #[ts(
        type = "\"bool\" | \"integer\" | \"number\" | \"string\" | \"path\" | \"path_list\" | \"string_list\" | \"enum\" | \"secret_ref\""
    )]
    pub kind: String,
    /// Allowed values of an `enum`; empty otherwise.
    pub options: Vec<String>,
    /// The built-in default; null when unset by default.
    #[ts(type = "unknown")]
    pub default: serde_json::Value,
    /// The effective value. A `secret_ref` shows the secret's name, never its value.
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
    /// The layer that set `value`.
    #[ts(type = "\"default\" | \"user\" | \"project\" | \"env\"")]
    pub source: String,
    /// Whether the daemon must restart before a change applies.
    pub restart: bool,
}

/// `config.set`: writes one setting to the user config file (never a project's), or removes it
/// with a null value so the default applies. The whole configuration is validated first; an
/// invalid change is not saved.
#[derive(Debug)]
pub struct ConfigSet;

impl Method for ConfigSet {
    const NAME: &'static str = "config.set";
    type Params = ConfigSetParams;
    type Result = ConfigSetResult;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigSetParams {
    pub key: String,
    /// The new value, matching the entry's kind; null removes the key from the user file.
    #[serde(default)]
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigSetResult {
    /// The setting after the change, as the daemon will load it at its next start.
    pub entry: ConfigEntry,
    /// Restart the daemon to apply the change.
    pub restart: bool,
    /// An environment variable (or project file) sets this key, so the saved value does not
    /// take effect while it does.
    pub overridden: bool,
}

// ---- Connectors: MCP servers, plugins, and skills ----

/// `mcp.list`: the MCP servers in the user config with their last known status, plus the
/// plugins and skills found in the user's folders.
#[derive(Debug)]
pub struct McpList;

impl Method for McpList {
    const NAME: &'static str = "mcp.list";
    type Params = McpListParams;
    type Result = McpListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpListResult {
    pub servers: Vec<McpServerInfo>,
    pub plugins: Vec<PluginInfo>,
    pub skills: Vec<SkillInfo>,
    /// The user config file servers are written to.
    pub user_file: String,
    /// Problems found while reading the config, plugins, and skills.
    pub problems: Vec<String>,
}

/// One `[mcp.servers.<name>]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServerInfo {
    pub name: String,
    pub config: McpServerConfig,
    pub status: McpStatus,
}

/// A server's settings as written in the config. Secrets appear only as secret names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServerConfig {
    /// A local command speaking MCP on stdio; exactly one of `command` and `url` is set.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Environment variables read from the secret store: variable name to secret name.
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    /// A streamable-HTTP endpoint.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The secret sent as `Authorization: Bearer` to an HTTP server.
    #[serde(default)]
    pub bearer_secret: Option<String>,
    /// Tools that run without asking, or `*` for all.
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub timeout_s: Option<u64>,
    /// The catalogue entry it was added from.
    #[serde(default)]
    pub catalog: Option<String>,
}

fn yes() -> bool {
    true
}

/// What the daemon last learned about a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpStatus {
    #[ts(type = "\"untested\" | \"connected\" | \"error\" | \"disabled\"")]
    pub state: String,
    /// The server's tools, from the last successful connection.
    pub tools: Vec<String>,
    pub error: Option<String>,
    /// When the status was learned, in Unix milliseconds.
    #[ts(type = "number | null")]
    pub checked_at: Option<u64>,
    /// Secrets the server refers to that are not in the secret store.
    pub missing_secrets: Vec<String>,
}

/// A plugin folder (Claude Code layout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInfo {
    pub name: String,
    pub root: String,
    /// False when listed in `extensions.disabled_plugins`.
    pub enabled: bool,
    pub skills: u32,
    pub commands: u32,
    pub hooks: u32,
    /// MCP servers the plugin declares.
    pub servers: Vec<String>,
}

/// A skill in the user's skills folder or a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// `user` or `plugin:<name>`.
    pub origin: String,
    pub dir: String,
}

/// `mcp.save`: adds or replaces `[mcp.servers.<name>]` in the user config. Secret values in
/// `secrets` are stored in the secret store first, under their names; they are never written
/// to the config or returned. New sessions use the change; running sessions keep their servers.
#[derive(Debug)]
pub struct McpSave;

impl Method for McpSave {
    const NAME: &'static str = "mcp.save";
    type Params = McpSaveParams;
    type Result = McpSaveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpSaveParams {
    pub name: String,
    pub config: McpServerConfig,
    /// Secret name to value.
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    /// Refuse to replace an existing server of the same name.
    #[serde(default)]
    pub create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpSaveResult {
    pub server: McpServerInfo,
}

/// `mcp.install`: adds a server from the connector catalogue, filling its settings from
/// `values` (field id to value). Secret fields go to the secret store as `mcp.<name>.<field>`.
#[derive(Debug)]
pub struct McpInstall;

impl Method for McpInstall {
    const NAME: &'static str = "mcp.install";
    type Params = McpInstallParams;
    type Result = McpSaveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpInstallParams {
    pub catalog_id: String,
    /// The server's name; the catalogue id when empty.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

/// `mcp.remove`: removes a server from the user config.
#[derive(Debug)]
pub struct McpRemove;

impl Method for McpRemove {
    const NAME: &'static str = "mcp.remove";
    type Params = McpRemoveParams;
    type Result = McpRemoveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpRemoveParams {
    pub name: String,
    /// Also delete the secrets the server referred to.
    #[serde(default)]
    pub forget_secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpRemoveResult {
    pub removed: bool,
}

/// `mcp.set_enabled`: turns a server on or off without removing it.
#[derive(Debug)]
pub struct McpSetEnabled;

impl Method for McpSetEnabled {
    const NAME: &'static str = "mcp.set_enabled";
    type Params = McpSetEnabledParams;
    type Result = McpSaveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpSetEnabledParams {
    pub name: String,
    pub enabled: bool,
}

/// `mcp.test`: starts a server, lists its tools, and stops it. The result is also what
/// `mcp.list` shows until the next test.
#[derive(Debug)]
pub struct McpTest;

impl Method for McpTest {
    const NAME: &'static str = "mcp.test";
    type Params = McpTestParams;
    type Result = McpStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpTestParams {
    pub name: String,
}

/// `mcp.catalog`: the connector catalogue, built in and extended or overridden by
/// `<config>/connectors.json`.
#[derive(Debug)]
pub struct McpCatalog;

impl Method for McpCatalog {
    const NAME: &'static str = "mcp.catalog";
    type Params = McpCatalogParams;
    type Result = McpCatalogResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpCatalogParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpCatalogResult {
    pub entries: Vec<CatalogServer>,
    /// The file that extends the built-in catalogue (it may not exist).
    pub override_file: String,
    /// Problems reading the override file.
    pub problems: Vec<String>,
}

/// A connector in the catalogue. `{{field}}` in `args`, `env` values, `url`, and `headers`
/// values is replaced by that field's value; an argument that is exactly `{{field}}` becomes
/// one argument per line of a `path_list` field, and is left out when the field is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CatalogServer {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    /// A Lucide icon name, such as `database`.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// A secret field sent as `Authorization: Bearer`, written `{{field}}`.
    #[serde(default)]
    pub bearer: Option<String>,
    #[serde(default)]
    pub fields: Vec<CatalogField>,
}

/// A setting the user fills in when adding a catalogue connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CatalogField {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// `secret` values go to the secret store and may only fill a whole `env` value or `bearer`.
    #[ts(type = "\"text\" | \"secret\" | \"path\" | \"path_list\" | \"url\"")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
}
