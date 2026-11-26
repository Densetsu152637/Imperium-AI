use super::*;

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
    /// The display name set for the row (its `alias`, or the app's override), shown instead of
    /// the id; the id is what rules, agents and `--model` use.
    #[serde(default)]
    pub alias: Option<String>,
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
    /// False when the row is switched off and left out of routing.
    pub enabled: bool,
    /// Whether the model can reason, so sessions on it send their effort: the row's
    /// `reasoning` setting or tags, a local model's chat template, or a known model name.
    #[serde(default)]
    pub reasoning: bool,
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
    /// Every folder scanned: `models.folders` and the download folder.
    pub folders: Vec<String>,
    /// `models.folders` alone, the folders `models.remove_folder` can remove.
    #[serde(default)]
    pub configured_folders: Vec<String>,
    /// `models.default`: the model new sessions use, when set.
    #[serde(default)]
    pub default_model: Option<String>,
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
    /// The display name set for the model (`runtime.llamacpp.models."<id>".alias`), shown
    /// instead of the id.
    #[serde(default)]
    pub alias: Option<String>,
    /// Whether the model can reason: its `reasoning` setting, else its chat template (a
    /// thinking switch, think tags, or `reasoning_effort`), else its name.
    #[serde(default)]
    pub reasoning: bool,
    /// `llamacpp` for GGUF files, `vllm` for Hugging Face folders, `ncm` for NCM safetensors.
    #[ts(type = "\"llamacpp\" | \"vllm\" | \"ncm\"")]
    pub runtime: String,
    pub path: String,
    /// The repository it came from, such as `unsloth/Qwen3-8B-GGUF`, from the Hugging Face
    /// cache or the `<publisher>/<repo>` folders above the file.
    #[serde(default)]
    pub repo: Option<String>,
    /// The multimodal projector paired with it, which lets it read images.
    #[serde(default)]
    pub mmproj: Option<String>,
    /// Why it cannot be used on this machine, when it cannot (such as vLLM models off Linux).
    #[serde(default)]
    pub problem: Option<String>,
    pub architecture: Option<String>,
    pub quantization: Option<String>,
    pub parts: u32,
    #[ts(type = "number")]
    pub size_bytes: u64,
    #[ts(type = "number | null")]
    pub context_length: Option<u64>,
    /// Memory needed with the model's KV pool and settings.
    #[ts(type = "number")]
    pub required_bytes: u64,
    /// How the model would load on this machine when nothing else is loaded.
    #[ts(type = "\"gpu\" | \"partial\" | \"cpu\" | \"none\"")]
    pub placement: String,
    /// Layers on the GPU: the loaded server's, else what loading it now would give (the
    /// planner's estimate under its settings, beside the models loaded).
    pub gpu_layers: Option<u32>,
    /// VRAM it holds (loaded) or would take if loaded now, weights and KV pool included.
    #[serde(default)]
    #[ts(type = "number")]
    pub vram_bytes: u64,
    /// System RAM it holds (loaded, prompt cache included) or would take if loaded now.
    #[serde(default)]
    #[ts(type = "number")]
    pub ram_bytes: u64,
    /// Layers the model has, when its header says.
    #[serde(default)]
    pub layers: Option<u32>,
    /// A fixed `pool_tokens` did not fit beside the weights on the GPU, so the pool was cut to
    /// what fits (`pool_tokens`) to keep the layers on the GPU.
    #[serde(default)]
    pub pool_clamped: bool,
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
    /// Most tokens one agent may use: what the model was trained for, its configured limit,
    /// and what fits in memory, whichever is least. Compaction is planned against it.
    #[serde(default)]
    pub context: u32,
    /// `context` is sized automatically (the `auto` setting) rather than configured.
    #[serde(default)]
    pub context_auto: bool,
    /// Tokens of KV cache shared by the model's slots: what the server reserves (or would
    /// start with). It starts small and grows on demand up to `context`.
    #[serde(default)]
    pub pool_tokens: u32,
    /// Of those, tokens reserved by calls running now.
    #[serde(default)]
    pub pool_reserved_tokens: u32,
    /// False when the user switched the model off ("Use this model" in the model configuration
    /// dialog, `models.disabled`): it is not routed to, offered to agents, or selectable.
    #[serde(default = "yes")]
    pub enabled: bool,
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
    /// The llama.cpp build in use, such as `windows-x64-cuda13`; `custom` for your own
    /// `llama-server`, `detected` for one found on this machine.
    pub pack: String,
    pub server_path: String,
    pub installed: bool,
    pub llama_cpp_ref: String,
    /// `runtime.llamacpp.auto_install`: with none installed, the official build is fetched the
    /// first time a model is loaded.
    #[serde(default)]
    pub auto_install: bool,
}

/// `models.add_folder`: adds a folder to `models.folders` in the user's config file and scans
/// the model folders again at once (no restart). Returns the new listing.
#[derive(Debug)]
pub struct ModelFolderAdd;

impl Method for ModelFolderAdd {
    const NAME: &'static str = "models.add_folder";
    type Params = ModelFolderParams;
    type Result = LocalModelListResult;
}

/// `models.remove_folder`: removes a folder from `models.folders` and scans again. Nothing in
/// the folder is touched.
#[derive(Debug)]
pub struct ModelFolderRemove;

impl Method for ModelFolderRemove {
    const NAME: &'static str = "models.remove_folder";
    type Params = ModelFolderParams;
    type Result = LocalModelListResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelFolderParams {
    /// An absolute path to a folder.
    pub path: String,
}

/// `models.use`: makes a model the default for new sessions (`models.default` in the user's
/// config file), applied at once.
#[derive(Debug)]
pub struct ModelUse;

impl Method for ModelUse {
    const NAME: &'static str = "models.use";
    type Params = ModelUseParams;
    type Result = ModelUseResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUseParams {
    /// A local model's id (from `models.local`), a model-table id, or an `<endpoint>:<model>`
    /// spec.
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUseResult {
    /// What `models.default` is now, such as `llamacpp:Qwen3-8B-Q4_K_M`.
    pub default: String,
}

/// `models.add_endpoint`: connects a hosted OpenAI-compatible API (`[endpoints.<id>]` in the
/// user's config file, its key in the OS keychain), applied at once.
#[derive(Debug)]
pub struct ModelAddEndpoint;

impl Method for ModelAddEndpoint {
    const NAME: &'static str = "models.add_endpoint";
    type Params = ModelAddEndpointParams;
    type Result = ModelAddEndpointResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelAddEndpointParams {
    /// A short name, such as `openrouter`: lowercase letters, digits, `_` or `-`.
    pub id: String,
    /// The URL before `/chat/completions`, such as `https://openrouter.ai/api/v1`.
    pub base_url: String,
    /// The API key, stored in the keychain as `<id>-api-key`; never written to a file.
    #[serde(default)]
    pub api_key: Option<String>,
    /// A model the endpoint serves, such as `openai/gpt-4.1-mini`.
    #[serde(default)]
    pub model: Option<String>,
    /// Make `<id>:<model>` the default for new sessions.
    #[serde(default)]
    pub make_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelAddEndpointResult {
    pub id: String,
    /// The new `models.default`, when the endpoint was made the default.
    pub default: Option<String>,
}

/// `models.test`: loads a model if needed and sends it one short prompt, reporting the reply
/// and its speed. For local and hosted models alike.
#[derive(Debug)]
pub struct ModelTest;

impl Method for ModelTest {
    const NAME: &'static str = "models.test";
    type Params = ModelTestParams;
    type Result = ModelTestResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelTestParams {
    /// As for `models.use`.
    pub model: String,
    /// The prompt; a short default asking for a one-line greeting when absent.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelTestResult {
    /// The spec that was tested.
    pub model: String,
    pub ok: bool,
    /// The model's reply, when it answered.
    pub reply: String,
    /// Why the test failed, in words, when it did.
    pub message: Option<String>,
    /// Seconds spent waiting for and loading the model before the request was sent.
    pub load_seconds: f64,
    /// Seconds the request took.
    pub seconds: f64,
    pub completion_tokens: u32,
    /// Generation speed: the server's own measurement when it reports one, else completion
    /// tokens over the request's time.
    pub tokens_per_s: Option<f64>,
}

/// `runtime.status`: the llama.cpp runtime local GGUF models use, whether it runs, and whether
/// it sees the GPU, with what to do when something is wrong. It runs `llama-server --version`
/// and `--list-devices`, which load no model.
#[derive(Debug)]
pub struct RuntimeStatus;

impl Method for RuntimeStatus {
    const NAME: &'static str = "runtime.status";
    type Params = RuntimeStatusParams;
    type Result = RuntimeStatusResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeStatusParams {}

/// `runtime.set_server`: uses an existing `llama-server` (`runtime.llamacpp.custom_path` in
/// the user's config file), or the runtime pack again when `path` is null. Applies to models
/// loaded from then on.
#[derive(Debug)]
pub struct RuntimeSetServer;

impl Method for RuntimeSetServer {
    const NAME: &'static str = "runtime.set_server";
    type Params = RuntimeSetServerParams;
    type Result = RuntimeStatusResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeSetServerParams {
    /// The `llama-server` executable; null to go back to the runtime pack.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeStatusResult {
    /// `custom` (your own `llama-server`), `pack` (llama.cpp's build installed by
    /// `runtime.install`, or the one it would install when `installed` is false), or
    /// `detected` (a `llama-server` found on this machine, used while none is installed).
    #[ts(type = "\"custom\" | \"pack\" | \"detected\"")]
    pub source: String,
    /// The installed build when `source` is `pack`, such as `windows-x64-cuda13`.
    pub pack: String,
    /// The build that suits this machine best, which `runtime.install` tries first.
    pub suggested_pack: String,
    pub server_path: String,
    /// The `llama-server` file exists.
    pub installed: bool,
    /// It starts and answers.
    pub runs: bool,
    /// Its `--version` line.
    pub version: Option<String>,
    pub devices: Vec<RuntimeDevice>,
    /// Whether it sees a GPU; null when that could not be told.
    pub gpu_visible: Option<bool>,
    /// What is wrong and what to do, when something is.
    pub problem: Option<String>,
    /// How models will run, in a sentence.
    pub summary: String,
    pub llama_cpp_ref: String,
    /// Other `llama-server` executables found on this machine (on `PATH`, common build
    /// folders, and unpacked downloads in the home and Downloads folders), offered to the
    /// user. None is run or chosen automatically; `runtime.set_server` adopts one.
    pub candidates: Vec<String>,
    /// The latest runtime pack installation since the daemon started.
    pub install: Option<RuntimeInstallJob>,
    /// The backend models run on, in words: `CUDA 13`, `CUDA`, `Vulkan`, `Metal`, `ROCm`, or
    /// `CPU` (an installed build's, else the GPUs the server lists).
    pub backend: String,
    /// `runtime.llamacpp.auto_install`: the official build is installed by itself the first
    /// time a local model needs one and none usable is there.
    pub auto_install: bool,
    /// The llama.cpp build number of the server in use (from `--version`), when known.
    #[ts(type = "number | null")]
    pub build: Option<u64>,
    /// The build number of the release Imperium pins (`llama_cpp_ref`).
    #[ts(type = "number | null")]
    pub pinned_build: Option<u64>,
    /// How the server in use was installed: `managed` (Imperium's build), `winget`, `scoop`,
    /// `chocolatey`, `homebrew`, `lmstudio`, `folder` (a checkout or unpacked archive), or
    /// `other`.
    #[ts(
        type = "\"managed\" | \"winget\" | \"scoop\" | \"chocolatey\" | \"homebrew\" | \"lmstudio\" | \"folder\" | \"other\""
    )]
    pub install_kind: String,
    /// An update the server in use can have, when it is older than the pinned release or
    /// lacks a flag Imperium passes.
    pub update: Option<RuntimeUpdateOffer>,
    /// The latest package-manager update since the daemon started.
    pub update_job: Option<RuntimeUpdateJob>,
}

/// How the `llama-server` in use can be brought up to date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeUpdateOffer {
    /// `package_manager`: `runtime.update_external` runs `command` (with
    /// `update_external = "auto"` the daemon runs it by itself while no model is loaded from
    /// the server); `use_managed`: a checkout or folder Imperium never modifies, so
    /// `runtime.install` installs Imperium's build, which is preferred from then on;
    /// `managed`: Imperium's build is installing the pinned release.
    #[ts(type = "\"package_manager\" | \"use_managed\" | \"managed\"")]
    pub action: String,
    /// The package manager's upgrade, such as `winget upgrade --id ggml.llamacpp ...`.
    pub command: Option<String>,
    /// Why, in a sentence.
    pub message: String,
}

/// `runtime.update_external`: runs the package manager's upgrade of the `llama-server` in use
/// (winget, scoop, Chocolatey, or Homebrew; that package only) in the background, reporting
/// `runtime.update_progress`. Refused while a model is loaded from it or for an install no
/// package manager owns.
#[derive(Debug)]
pub struct RuntimeUpdateExternal;

impl Method for RuntimeUpdateExternal {
    const NAME: &'static str = "runtime.update_external";
    type Params = RuntimeUpdateExternalParams;
    type Result = RuntimeUpdateJob;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeUpdateExternalParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeUpdateJob {
    /// The `llama-server` being updated.
    pub server_path: String,
    /// The command run.
    pub command: String,
    #[ts(type = "\"running\" | \"done\" | \"failed\"")]
    pub state: String,
    /// What the package manager printed (the last lines), or why it could not run.
    pub output: String,
}

/// Notification sent to every client when a package-manager update starts and ends. The
/// payload is a [`RuntimeUpdateJob`].
pub const RUNTIME_UPDATE_PROGRESS: &str = "runtime.update_progress";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeDevice {
    /// Such as `CUDA0` or `Vulkan0`.
    pub id: String,
    pub description: String,
    pub gpu: bool,
}

/// `runtime.install`: downloads and installs llama.cpp's own build for this machine from its
/// GitHub release in the background, verified against the release's SHA-256 digests and
/// falling back to the next build (CUDA 13, CUDA 12, Vulkan, CPU) when one does not run here,
/// reporting `runtime.install_progress`. Once done, new model loads use it (unless a custom
/// `llama-server` is set).
#[derive(Debug)]
pub struct RuntimeInstall;

impl Method for RuntimeInstall {
    const NAME: &'static str = "runtime.install";
    type Params = RuntimeInstallParams;
    type Result = RuntimeInstallJob;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeInstallParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeInstallJob {
    /// The build being installed; it changes when one does not run here and the next is tried.
    pub pack: String,
    #[ts(type = "\"running\" | \"done\" | \"failed\"")]
    pub state: String,
    #[ts(type = "number")]
    pub done_bytes: u64,
    /// Zero until the download's size is known.
    #[ts(type = "number")]
    pub total_bytes: u64,
    /// Why it failed, or where `llama-server` was installed.
    pub message: Option<String>,
}

/// Notification sent to every client as a runtime pack installs and when it ends. The payload
/// is a [`RuntimeInstallJob`].
pub const RUNTIME_INSTALL_PROGRESS: &str = "runtime.install_progress";

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
    #[ts(type = "\"llamacpp\" | \"vllm\" | \"ncm\"")]
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

/// `models.bench`: starts a benchmark of installed local models. For each model it loads the
/// model through the same launch and residency path every call uses (as a background call, so
/// it waits while the machine is saturated and never evicts a model in use), sends a small fixed prompt
/// set, and measures load time, time to first token, prompt-processing speed and generation
/// speed from the real stream. The run is reported by `models.bench_progress`; each model's
/// result is saved (per model, hardware fingerprint and date) and listed by
/// `models.bench_results`. Nothing runs unless this method is called.
#[derive(Debug)]
pub struct ModelBench;

impl Method for ModelBench {
    const NAME: &'static str = "models.bench";
    type Params = ModelBenchParams;
    type Result = BenchRun;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelBenchParams {
    /// One local model, by the id `models.local` lists; every local model that can run here
    /// when absent.
    #[serde(default)]
    pub model: Option<String>,
    /// Unload the model first when it is loaded and idle, so its load time is measured. A
    /// model in use is never unloaded: its load time is left out. On by default.
    #[serde(default)]
    pub cold: Option<bool>,
}

/// `models.bench_results`: the saved results, newest first, and the benchmark runs of this
/// daemon session.
#[derive(Debug)]
pub struct ModelBenchResults;

impl Method for ModelBenchResults {
    const NAME: &'static str = "models.bench_results";
    type Params = ModelBenchResultsParams;
    type Result = ModelBenchResultsResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelBenchResultsParams {
    /// Only this model's results.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelBenchResultsResult {
    /// This machine's fingerprint for llama.cpp models, so a client can tell which results are
    /// from it.
    pub hardware_fingerprint: String,
    /// The fingerprints of this machine for each runtime (llama.cpp, then vLLM); a result is from
    /// this machine when its fingerprint is one of them.
    pub hardware_fingerprints: Vec<String>,
    pub results: Vec<BenchResult>,
    /// Runs since the daemon started, oldest first; at most one is running.
    pub runs: Vec<BenchRun>,
}

/// `models.bench_cancel`: stops a running benchmark. Models measured before it keep their
/// saved results; the model being measured has none. The slot it held is released at once.
#[derive(Debug)]
pub struct ModelBenchCancel;

impl Method for ModelBenchCancel {
    const NAME: &'static str = "models.bench_cancel";
    type Params = ModelBenchCancelParams;
    type Result = ModelBenchCancelResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelBenchCancelParams {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelBenchCancelResult {
    /// False when no running benchmark has that id.
    pub found: bool,
}

/// Notification sent to every client as a benchmark progresses and when it ends. The payload
/// is a [`BenchRun`].
pub const MODEL_BENCH_PROGRESS: &str = "models.bench_progress";

/// A benchmark run: which models, how far it is, and the results so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BenchRun {
    /// Such as `bench-1`.
    pub run_id: String,
    /// `running`, `done`, `cancelled`, or `failed`.
    pub state: String,
    /// The local model ids the run covers, in order.
    pub models: Vec<String>,
    /// The model being measured now.
    #[serde(default)]
    pub current: Option<String>,
    /// What the run is doing now, such as "waiting while the machine is saturated" or "prompt 2 of 3".
    #[serde(default)]
    pub step: Option<String>,
    /// Models finished (measured or failed).
    pub models_done: u32,
    /// What has been measured so far in this run, also saved.
    pub results: Vec<BenchResult>,
    /// Why the run failed, or a note on how it ended.
    #[serde(default)]
    pub message: Option<String>,
}

/// One model's measurements on one machine at one time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BenchResult {
    pub model_id: String,
    /// Which run measured it.
    pub run_id: String,
    /// When it was measured, RFC 3339 UTC.
    pub date: String,
    /// A short hash of the hardware and the runtime build (the llama.cpp release and pack, or
    /// vLLM) the numbers belong to.
    pub hardware_fingerprint: String,
    /// The hardware in words, such as `NVIDIA RTX 4090 24 GB, 64 GB RAM, 16 cores`.
    pub hardware: String,
    /// The runtime that served it (`llamacpp` or `vllm`).
    pub runtime: String,
    /// How it was placed: `gpu`, `partial` or `cpu`.
    pub placement: String,
    /// The worse of the machine's load levels (`quiet`, `busy`, `saturated`) when the model's
    /// turn began and when its last prompt ended; a busy machine spoils the numbers.
    pub machine: String,
    /// Seconds to start the model's server and make it ready; absent when it was already
    /// loaded and in use.
    #[serde(default)]
    pub load_s: Option<f64>,
    /// Median time to first token over the prompts, in milliseconds.
    #[serde(default)]
    pub ttft_ms: Option<f64>,
    /// Prompt tokens per second of prompt processing, over all prompts.
    #[serde(default)]
    pub prompt_tok_s: Option<f64>,
    /// Generated tokens per second after the first token, over all prompts.
    #[serde(default)]
    pub gen_tok_s: Option<f64>,
    /// Highest system RAM the model's server process held while it ran, when it could be read.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub peak_ram_bytes: Option<u64>,
    /// Highest GPU memory the model's server process held while it ran, when it could be read.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub peak_vram_bytes: Option<u64>,
    pub prompts: Vec<BenchPrompt>,
    /// Why the model could not be measured, when it could not.
    #[serde(default)]
    pub error: Option<String>,
}

/// One prompt of the set, measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BenchPrompt {
    /// `short`, `medium` or `long`.
    pub name: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Milliseconds from sending the request to the first generated token.
    pub ttft_ms: f64,
    #[serde(default)]
    pub prompt_tok_s: Option<f64>,
    #[serde(default)]
    pub gen_tok_s: Option<f64>,
    /// `prompt_tokens` is an estimate (the server reported no usage), so no prompt speed is
    /// claimed for this prompt.
    #[serde(default)]
    pub prompt_tokens_estimated: bool,
}

/// `models.config`: every local model's llama.cpp settings and every model-table row's
/// settings, as saved from the app and as they take effect.
#[derive(Debug)]
pub struct ModelConfig;

impl Method for ModelConfig {
    const NAME: &'static str = "models.config";
    type Params = ModelConfigParams;
    type Result = ModelConfigResult;
}

/// `models.set_config`: saves model settings to the app's settings file and applies them.
/// Each update replaces that model's saved settings whole; models not named keep theirs.
#[derive(Debug)]
pub struct ModelSetConfig;

impl Method for ModelSetConfig {
    const NAME: &'static str = "models.set_config";
    type Params = ModelSetConfigParams;
    type Result = ModelConfigResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelConfigParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelConfigResult {
    /// Models found in `models.folders`, in id order.
    pub local: Vec<LocalModelConfig>,
    /// Rows of the model table, in table order.
    pub table: Vec<TableModelConfig>,
    /// What a local model gets for each setting it does not set.
    pub defaults: LocalLaunchEffective,
    /// KV cache types llama-server accepts for `cache_type_k` and `cache_type_v`.
    pub cache_types: Vec<String>,
    /// The file saved settings are written to.
    pub file: String,
    /// Specs of the models the user hid (`models.hidden`), which agents' `list_models` leaves
    /// out.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Specs of the models the user switched off (`models.disabled`); a model-table row
    /// whose spec is listed is off whatever its own `enabled` says.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// `models.default`, the model new sessions use, as a spec; null when none is set. A
    /// default model cannot be switched off.
    #[serde(default)]
    pub default_model: Option<String>,
}

/// A local model's llama.cpp settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelConfig {
    /// The id in `models.local`.
    pub id: String,
    #[ts(type = "\"llamacpp\" | \"vllm\" | \"ncm\"")]
    pub runtime: String,
    /// What the app's settings file sets; null fields take the effective value from elsewhere.
    pub saved: LocalLaunchSettings,
    /// What the model is started with.
    pub effective: LocalLaunchEffective,
    /// Memory the model needs with the effective settings and its KV pool at its ceiling.
    #[ts(type = "number")]
    pub required_bytes: u64,
    /// The part of `required_bytes` that is the KV cache pool, at its ceiling
    /// (`effective.pool_max` tokens), not at the size it starts with.
    #[ts(type = "number")]
    pub kv_pool_bytes: u64,
    /// How it would load on this machine when nothing else is loaded.
    #[ts(type = "\"gpu\" | \"partial\" | \"cpu\" | \"none\"")]
    pub placement: String,
    pub loaded: bool,
    /// Loaded with other settings: the new ones apply once no session is using it.
    pub reload_pending: bool,
    /// Pinned to the CPU lane by `placement = "cpu"` in the model table.
    pub cpu_lane: bool,
    /// The context the model was trained for, from its header.
    #[ts(type = "number | null")]
    pub context_length: Option<u64>,
    /// What the model's own metadata says (its GGUF header, a vision projector, a Hugging Face
    /// model card beside it); agents read it after the user's card.
    #[serde(default)]
    pub metadata_card: String,
    /// The draft model in use (loaded) or that would be used when the model loads; null when
    /// none is configured or it cannot be used.
    #[serde(default)]
    pub draft_in_use: Option<String>,
    /// Why a configured draft model is not used: the model loads without it. `required_bytes`
    /// counts the draft only while it is used.
    #[serde(default)]
    pub draft_notice: Option<String>,
    /// False when the user switched the model off (`models.disabled`).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Size of the model's weights on disk, which is what loading them takes.
    #[serde(default)]
    #[ts(type = "number")]
    pub weights_bytes: u64,
    /// Layers the model has, when its header says.
    #[serde(default)]
    pub layers: Option<u32>,
    /// Layers the planner puts on the GPU with the effective settings, when it can place the
    /// model at all (null otherwise).
    #[serde(default)]
    pub gpu_layers: Option<u32>,
    /// VRAM the model would take with the effective settings, weights and KV pool included;
    /// 0 when it cannot be placed. An estimate beside what is loaded now.
    #[serde(default)]
    #[ts(type = "number")]
    pub vram_bytes: u64,
    /// System RAM the model would take with the effective settings; 0 when it cannot be placed.
    #[serde(default)]
    #[ts(type = "number")]
    pub ram_bytes: u64,
}

/// Per-model llama.cpp settings; a null field takes the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalLaunchSettings {
    /// Display name shown instead of the id; null shows the id.
    #[serde(default)]
    pub alias: Option<String>,
    /// The model card: what the model is good at and notes, which agents read when they choose
    /// a model to delegate to; added to `metadata_card`.
    #[serde(default)]
    pub card: Option<String>,
    /// Most tokens one agent may use (vLLM `--max-model-len`), capped to what the model was
    /// trained for and what fits in memory. Ignored when `context_auto` is true.
    #[serde(default)]
    pub context: Option<u32>,
    /// Size the context automatically (`context = "auto"`): the most the model was trained for
    /// that fits in memory. False with a null `context` takes the default.
    #[serde(default)]
    pub context_auto: bool,
    /// Fixes the tokens of KV cache shared by every slot (`--ctx-size`), at least `context`;
    /// null lets the pool start small and grow on demand.
    #[serde(default)]
    pub pool_tokens: Option<u32>,
    /// Calls the server runs at once (`--parallel`; vLLM `--max-num-seqs`).
    #[serde(default)]
    pub slots: Option<u32>,
    /// `--cache-type-k`, one of `cache_types`.
    #[serde(default)]
    pub cache_type_k: Option<String>,
    /// `--cache-type-v`, one of `cache_types`; a quantised type needs flash attention.
    #[serde(default)]
    pub cache_type_v: Option<String>,
    /// `--flash-attn`.
    #[serde(default)]
    #[ts(type = "\"auto\" | \"on\" | \"off\" | null")]
    pub flash_attn: Option<String>,
    /// Most layers placed on the GPU; the planner may place fewer.
    #[serde(default)]
    pub max_gpu_layers: Option<u32>,
    /// Longest a call waits for a slot or for room to load the model.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_wait_s: Option<u64>,
    /// Most tokens one reply may generate before it is retried without a cap; 0 means no cap.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Arguments added after `runtime.llamacpp.extra_args` (or `runtime.vllm.extra_args`).
    #[serde(default)]
    pub extra_args: Option<Vec<String>>,
    /// The id of an installed local model that drafts tokens for this one (speculative
    /// decoding); null uses no draft. A smaller model with the same tokenizer family; llama.cpp
    /// models only.
    #[serde(default)]
    pub draft_model: Option<String>,
    /// Most tokens the draft proposes per step; null keeps llama.cpp's default. Needs
    /// `draft_model`.
    #[serde(default)]
    pub draft_max: Option<u32>,
    /// Fewest tokens the draft proposes per step; null keeps llama.cpp's default. Needs
    /// `draft_model`.
    #[serde(default)]
    pub draft_min: Option<u32>,
    /// Layers of the draft placed on the GPU; 0 keeps it in RAM, null leaves llama.cpp's `auto`. Needs
    /// `draft_model`.
    #[serde(default)]
    pub draft_gpu_layers: Option<u32>,
}

/// The llama.cpp settings a local model is started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalLaunchEffective {
    /// Most tokens one agent may use; 0 in `defaults` when the default is `auto`, which is
    /// sized per model.
    pub context: u32,
    /// The context is sized automatically.
    #[serde(default)]
    pub context_auto: bool,
    /// The KV pool the model starts with (or is running with), or the fixed pool.
    pub pool_tokens: u32,
    /// The most the pool may grow to: the tokens the memory estimate's KV cache is worked out
    /// for. The fixed pool when a setting fixes it. 0 in a `models.config` row whose model does
    /// not say how big its KV cache is per token (the estimate then does not depend on it).
    #[serde(default)]
    pub pool_max: u32,
    /// A setting fixes the pool, so it never grows.
    #[serde(default)]
    pub pool_fixed: bool,
    /// The fixed pool did not fit beside the weights on the GPU and was cut to `pool_tokens`,
    /// so the layers stay on the GPU.
    #[serde(default)]
    pub pool_clamped: bool,
    pub slots: u32,
    pub cache_type_k: String,
    pub cache_type_v: String,
    #[ts(type = "\"auto\" | \"on\" | \"off\"")]
    pub flash_attn: String,
    /// Absent: as many as the planner fits.
    pub max_gpu_layers: Option<u32>,
    #[ts(type = "number")]
    pub max_wait_s: u64,
    pub max_output_tokens: u32,
    /// Every extra argument, the global ones first.
    pub extra_args: Vec<String>,
    /// The draft model configured for speculative decoding, if any.
    #[serde(default)]
    pub draft_model: Option<String>,
    #[serde(default)]
    pub draft_max: Option<u32>,
    #[serde(default)]
    pub draft_min: Option<u32>,
    #[serde(default)]
    pub draft_gpu_layers: Option<u32>,
}

/// A model-table row's settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelConfig {
    pub id: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    #[ts(type = "\"local\" | \"cloud\"")]
    pub location: String,
    /// False when the row names an endpoint that is not configured.
    pub endpoint_configured: bool,
    /// What the app's settings file sets; null fields keep the row's value from the config.
    pub saved: TableModelSettings,
    pub effective: TableModelEffective,
}

/// Settings the app may change on a model-table row; a null field keeps the row's own value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelSettings {
    /// Display name shown instead of the id; null keeps the row's own `alias`.
    #[serde(default)]
    pub alias: Option<String>,
    /// The model card agents read when they choose a model; null keeps the row's own `card`.
    #[serde(default)]
    pub card: Option<String>,
    /// False leaves the row out of routing.
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    #[ts(type = "\"basic\" | \"standard\" | \"strong\" | \"frontier\" | null")]
    pub tier: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Tokens the model reads per request.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Set both prices or neither.
    #[serde(default)]
    pub input_usd_per_mtok: Option<f64>,
    #[serde(default)]
    pub output_usd_per_mtok: Option<f64>,
}

/// A model-table row as routing uses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelEffective {
    /// The display name in effect, if any.
    #[serde(default)]
    pub alias: Option<String>,
    /// The model card in effect, if any.
    #[serde(default)]
    pub card: Option<String>,
    pub enabled: bool,
    #[ts(type = "\"basic\" | \"standard\" | \"strong\" | \"frontier\"")]
    pub tier: String,
    pub tags: Vec<String>,
    pub context_window: Option<u32>,
    pub input_usd_per_mtok: Option<f64>,
    pub output_usd_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelSetConfigParams {
    #[serde(default)]
    pub local: Vec<LocalModelUpdate>,
    #[serde(default)]
    pub table: Vec<TableModelUpdate>,
    /// Replaces `models.hidden`, the specs of the models the user hid; null leaves it alone.
    #[serde(default)]
    pub hidden: Option<Vec<String>>,
    /// Replaces `models.disabled`, the specs of the models the user switched off ("Use this
    /// model"); null leaves it alone. Refused when it would disable the default model.
    #[serde(default)]
    pub disabled: Option<Vec<String>>,
    /// Check and compute the result without saving or applying anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// New saved settings for one local model; all-null settings remove its saved settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelUpdate {
    pub id: String,
    pub settings: LocalLaunchSettings,
}

/// New saved settings for one model-table row; all-null settings remove its saved settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelUpdate {
    pub id: String,
    pub settings: TableModelSettings,
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

/// Notification sent to every client for each move of local-model memory the daemon's residency
/// controller makes or the runtime makes for a call: a model loaded or unloaded, an agent's
/// slot saved or restored, a KV pool resized, or the memory budget changed. The payload is a
/// [`ModelResidency`].
pub const MODEL_RESIDENCY: &str = "model.residency";

/// One move of local-model memory and why it happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelResidency {
    /// `load`, `unload`, `save_slot`, `restore_slot`, `resize_pool`, or `set_budget`.
    pub action: String,
    /// The model moved; absent for `set_budget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model_id: Option<String>,
    /// The agent (`<session>/<agent>`) whose slot was saved or restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub holder: Option<String>,
    /// Why: `call` (a model call needed it), `resume` (an agent going on after waiting got its
    /// context back), `room` (another model did not fit beside it), `more_gpu` (another model
    /// gets more layers on the GPU without it), `swap_gain` (another model gets `swap_gain`
    /// times the GPU memory without it), `gpu_rule` (nothing generated, so models not
    /// generating gave way rather than let a call run on the CPU), `critic` (the CPU-lane
    /// critic took the GPU while its session waits on it), `aged` (held mid-turn, but loaded at
    /// least `min_residency_s` and nothing else fitted), `idle` (`idle_unload_s`), `config`
    /// (its settings changed), `grow` (a call needs a larger KV pool), `promote` (it ran on the
    /// CPU while the GPU was idle), `request` (`models.unload`), `yield` (a waiting agent gave
    /// its KV cells to another agent on the same model), `stopped` (the session was stopped),
    /// `budget` (the free memory measured changed), or `crashed` (its server exited on its own:
    /// it was taken out of memory and the next call loads it again, `detail` says how it
    /// ended). Clients word reasons they do not know from their name.
    pub reason: String,
    /// Whether it happened; when not, `detail` says why.
    pub ok: bool,
    /// More about it: the model an eviction made room for, a load's placement, or the error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    pub seconds: f64,
    /// Bytes moved: slot state written or read, or the memory a load took. For `set_budget`,
    /// the VRAM budget.
    #[ts(type = "number")]
    pub bytes: u64,
}

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
    /// Out: `room` (another model needed the memory), `idle`, `config` (its settings
    /// changed), `grow` (a call needs a larger KV pool), `promote` (it ran on the CPU while the
    /// GPU was idle and restarts on the GPU), `request` (`models.unload`), or `crashed` (its
    /// server exited on its own; nothing was saved). In: `swap` (it
    /// replaced a model) or `restore` (it came back with saved slots). `model.residency` says
    /// in more detail why each move happened.
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
