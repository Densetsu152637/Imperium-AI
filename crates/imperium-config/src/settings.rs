use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use imperium_api::Pricing;
use serde::{Deserialize, Serialize};

use crate::SecretName;

/// Built-in defaults: the first configuration layer.
pub(crate) const DEFAULTS: &str = r#"
[daemon]
# Port for the local WebSocket listener; 0 picks a free port at each start.
port = 0

[agents]
# Model calls an agent may make in one turn before it stops.
max_steps = 50
# Sessions active within this many days continue after the daemon restarts.
resume_days = 7

[runtime.llamacpp]
# Runtime pack: auto, cpu, vulkan, cuda12, cuda13, rocm, or metal. custom_path overrides it.
backend = "auto"
# Seconds a loaded model may sit unused before it is unloaded.
idle_unload_s = 900
# Context length and parallel slots each loaded model is started with.
context = 8192
parallel = 2
# KV cache type: f16, q8_0, or q4_0.
cache_type = "f16"
# System memory kept free for everything else when placing models.
ram_headroom_mb = 2048
"#;

/// Typed view of the merged configuration. Keys this build does not know are ignored, so a
/// config written for a newer version still loads.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Settings {
    pub daemon: DaemonSettings,
    pub agents: AgentSettings,
    #[serde(default)]
    pub models: ModelSettings,
    /// OpenAI-compatible endpoints by id.
    #[serde(default)]
    pub endpoints: BTreeMap<String, EndpointSettings>,
    #[serde(default)]
    pub routing: RoutingSettings,
    pub runtime: RuntimeSettings,
    #[serde(default)]
    pub hardware: HardwareSettings,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub extensions: ExtensionSettings,
    #[serde(default)]
    pub memory: MemorySettings,
    #[serde(default)]
    pub workflow: WorkflowSettings,
    #[serde(default)]
    pub github: GithubSettings,
    #[serde(default)]
    pub isolation: IsolationSettings,
    #[serde(default)]
    pub web: WebSettings,
    #[serde(default)]
    pub pets: PetSettings,
    #[serde(default)]
    pub media: MediaSettings,
    #[serde(default)]
    pub phone: PhoneSettings,
    #[serde(default)]
    pub resume: ResumeSettings,
    /// `[[projects]]`: per-project settings, such as the project agent.
    #[serde(default)]
    pub projects: Vec<ProjectSettings>,
    /// `[[schedules]]`: tasks the daemon runs on a timetable (see docs/14).
    #[serde(default)]
    pub schedules: Vec<ScheduleSettings>,
    /// `[[hooks]]`: commands run at points in a session (see docs/06-extensibility.md#hooks).
    #[serde(default)]
    pub hooks: Vec<HookSettings>,
}

/// The endpoint id that names models served by the supervised llama.cpp runtime, as in
/// `llamacpp:<local model id>`.
pub const LLAMACPP_ENDPOINT: &str = "llamacpp";

/// The endpoint id that names Hugging Face models served by supervised vLLM, as in
/// `vllm:<folder name>`.
pub const VLLM_ENDPOINT: &str = "vllm";

impl Settings {
    /// Checks rules that span keys: model table ids are unique, and routing rules name models
    /// that exist in the table.
    pub fn validate(&self) -> Result<(), String> {
        for reserved in [LLAMACPP_ENDPOINT, VLLM_ENDPOINT] {
            if self.endpoints.contains_key(reserved) {
                return Err(format!(
                    "endpoints.{reserved} is reserved for local models in models.folders"
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.models.catalog {
            if !seen.insert(entry.id.as_str()) {
                return Err(format!("models.catalog: id {:?} is used twice", entry.id));
            }
            let served_locally = entry.spec.split_once(':').is_some_and(|(endpoint, _)| {
                endpoint == LLAMACPP_ENDPOINT || endpoint == VLLM_ENDPOINT
            });
            if served_locally && entry.location != Location::Local {
                return Err(format!(
                    "models.catalog: {:?} runs on this machine, so its location must be local",
                    entry.id
                ));
            }
        }
        for (index, rule) in self.routing.rules.iter().enumerate() {
            if let Some(unknown) = rule
                .use_models
                .iter()
                .find(|id| !seen.contains(id.as_str()))
            {
                return Err(format!(
                    "routing.rules[{index}]: {unknown:?} is not an id in models.catalog"
                ));
            }
        }
        let mut schedule_ids = std::collections::BTreeSet::new();
        for schedule in &self.schedules {
            if !schedule_ids.insert(schedule.id.as_str()) {
                return Err(format!("schedules: id {:?} is used twice", schedule.id));
            }
            let triggers = [
                &schedule.trigger.cron,
                &schedule.trigger.every,
                &schedule.trigger.at,
            ];
            if triggers.iter().filter(|t| t.is_some()).count() != 1 {
                return Err(format!(
                    "schedules.{}: set exactly one of trigger.cron, trigger.every, or trigger.at",
                    schedule.id
                ));
            }
            let action = &schedule.action;
            let kinds = [&action.prompt, &action.command, &action.agent];
            if kinds.iter().filter(|kind| kind.is_some()).count() != 1 {
                return Err(format!(
                    "schedules.{}: set exactly one of action.prompt, action.command, or action.agent",
                    schedule.id
                ));
            }
        }
        if !matches!(
            self.resume.mode.as_str(),
            "wait_for_reset" | "at_time" | "retry_every" | "stop"
        ) {
            return Err("resume.mode must be wait_for_reset, at_time, retry_every, or stop".into());
        }
        if self.resume.mode == "at_time" && self.resume.at_time.is_none() {
            return Err("resume.at_time (HH:MM) is needed when resume.mode = \"at_time\"".into());
        }
        if !matches!(
            self.web.search.as_str(),
            "none" | "searxng" | "brave" | "tavily"
        ) {
            return Err("web.search must be none, searxng, brave, or tavily".into());
        }
        if !matches!(self.isolation.tier.as_str(), "policy" | "os" | "container") {
            return Err("isolation.tier must be policy, os, or container".into());
        }
        if !matches!(self.github.autofix.push.as_str(), "ask" | "auto") {
            return Err("github.autofix.push must be ask or auto".into());
        }
        if let Some(dir) = &self.models.download_dir
            && !dir.is_absolute()
        {
            return Err(format!(
                "models.download_dir must be an absolute path, not {:?}",
                dir.display().to_string()
            ));
        }
        for (name, server) in &self.mcp.servers {
            if server.command.is_some() == server.url.is_some() {
                return Err(format!(
                    "mcp.servers.{name}: set exactly one of command or url"
                ));
            }
        }
        Ok(())
    }
}

/// GitHub watching and auto-fix (see docs/14-projects-and-automation.md#github-auto-fix).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct GithubSettings {
    #[serde(default)]
    pub watch: Vec<GithubWatch>,
    #[serde(default)]
    pub autofix: AutofixSettings,
}

/// One `[[github.watch]]` entry: a repository and the local clone fixes are made from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GithubWatch {
    /// `owner/name`.
    pub repo: String,
    /// A local clone whose `origin` is the repository.
    pub project: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct AutofixSettings {
    pub enabled: bool,
    /// `ask` (default): pushes wait for approval; `auto` pushes without asking.
    pub push: String,
    pub max_attempts_per_pr: u32,
    pub poll_s: u64,
    /// Model for fix sessions; defaults to `models.default`.
    pub model: Option<String>,
}

impl Default for AutofixSettings {
    fn default() -> Self {
        AutofixSettings {
            enabled: false,
            push: "ask".into(),
            max_attempts_per_pr: 3,
            poll_s: 120,
            model: None,
        }
    }
}

/// How sessions wait when a provider's usage or rate limit is hit (each session can override).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct ResumeSettings {
    /// `wait_for_reset`, `at_time`, `retry_every`, or `stop`.
    pub mode: String,
    /// `HH:MM` local time, for `at_time`.
    pub at_time: Option<String>,
    pub retry_every_minutes: u32,
    pub max_wait_hours: u32,
}

impl Default for ResumeSettings {
    fn default() -> Self {
        ResumeSettings {
            mode: "wait_for_reset".into(),
            at_time: None,
            retry_every_minutes: 5,
            max_wait_hours: 12,
        }
    }
}

/// The phone listener (see docs/15-mobile-and-remote.md). Off unless `bind` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PhoneSettings {
    /// Address to serve phones on, such as a Tailscale address `100.101.102.103:7681`. It adds
    /// no encryption of its own, so use a private overlay network or a trusted LAN.
    #[serde(default)]
    pub bind: Option<String>,
}

/// Media tools (see docs/09-media-generation.md).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct MediaSettings {
    #[serde(default)]
    pub image: Option<ImageSettings>,
    #[serde(default)]
    pub speech: Option<MediaServiceSettings>,
    #[serde(default)]
    pub transcription: Option<MediaServiceSettings>,
}

/// Image generation through an `[endpoints]` entry's OpenAI-compatible images API, or ComfyUI.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ImageSettings {
    /// `openai` (an images API) or `comfyui`.
    pub backend: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub comfyui_url: Option<String>,
    /// An API-format ComfyUI workflow containing `{{prompt}}`.
    #[serde(default)]
    pub workflow: Option<PathBuf>,
}

/// An OpenAI-compatible speech or transcription service from `[endpoints]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MediaServiceSettings {
    pub endpoint: String,
    pub model: String,
    /// Speech only.
    #[serde(default)]
    pub voice: Option<String>,
}

/// Pets (see docs/16-pets.md).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct PetSettings {
    pub enabled: bool,
    /// The pet's id; the first found when unset.
    pub pet: Option<String>,
    /// Folders of Pixel Pets v3 packages, besides `<config>/pets`.
    pub folders: Vec<PathBuf>,
    pub sleep_after_minutes: u64,
    pub fidget: bool,
}

impl Default for PetSettings {
    fn default() -> Self {
        PetSettings {
            enabled: true,
            pet: None,
            folders: Vec::new(),
            sleep_after_minutes: 5,
            fidget: true,
        }
    }
}

/// Web tools (see docs/05-tools-and-isolation.md#built-in-tools).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct WebSettings {
    /// Offer `web_fetch` (and `web_search` with a provider) to agents.
    pub enabled: bool,
    /// `none`, `searxng`, `brave`, or `tavily`.
    pub search: String,
    /// A SearXNG instance with the JSON format enabled.
    pub searxng_url: String,
    /// The Brave or Tavily key, from the keychain.
    pub api_key: Option<SecretRef>,
    /// Domains `web_fetch` may read without asking (subdomains included).
    pub allow_domains: Vec<String>,
    /// Offer browser tools (Chrome, Chromium, or Edge over CDP).
    pub browser: bool,
    /// The browser to drive; found automatically when empty.
    pub browser_path: Option<PathBuf>,
    /// Run the browser without a window (the desktop app mirrors it either way).
    pub headless: bool,
}

impl Default for WebSettings {
    fn default() -> Self {
        WebSettings {
            enabled: true,
            search: "none".into(),
            searxng_url: String::new(),
            api_key: None,
            allow_domains: Vec::new(),
            browser: true,
            browser_path: None,
            headless: true,
        }
    }
}

/// Where agents' commands run (see docs/05-tools-and-isolation.md#isolation-tiers).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct IsolationSettings {
    /// `policy` (tier 0), `os` (tier 1), or `container` (tier 2).
    pub tier: String,
    pub container_runtime: String,
    pub container_image: String,
    /// Whether sandboxed and container commands may use the network.
    pub network: bool,
}

impl Default for IsolationSettings {
    fn default() -> Self {
        IsolationSettings {
            tier: "policy".into(),
            container_runtime: "docker".into(),
            container_image: "ubuntu:24.04".into(),
            network: true,
        }
    }
}

/// One `[[projects]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProjectSettings {
    pub path: PathBuf,
    /// Run a project agent for this project while the daemon runs (off by default).
    #[serde(default)]
    pub agent: bool,
    /// Model for the agent itself; defaults to `models.default`.
    #[serde(default)]
    pub agent_model: Option<String>,
    /// Sessions the agent may have running at once.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    /// Sessions the agent may start per day.
    #[serde(default = "default_max_sessions_per_day")]
    pub max_sessions_per_day: u32,
}

fn default_max_sessions() -> usize {
    2
}

fn default_max_sessions_per_day() -> u32 {
    20
}

/// One `[[schedules]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ScheduleSettings {
    pub id: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub trigger: TriggerSettings,
    pub action: ActionSettings,
    /// `run_once_on_wake` (default) or `skip`.
    #[serde(default)]
    pub missed: Option<String>,
    /// `skip` (default): do not start a run while the previous one is going; `allow`.
    #[serde(default)]
    pub overlap: Option<String>,
    /// `os` shows a system notification when a run finishes.
    #[serde(default)]
    pub notify: Vec<String>,
}

/// Exactly one of `cron` (five fields, local time), `every` (`30m`), or `at` (a local time).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct TriggerSettings {
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub every: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
}

/// A prompt for a new session in `project`, or a shell command run there.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ActionSettings {
    pub project: PathBuf,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    /// A message for the project's agent (the project needs `agent = true`).
    #[serde(default)]
    pub agent: Option<String>,
    /// Model for prompt runs, as `<endpoint>:<model>`; defaults to `models.default`.
    #[serde(default)]
    pub model: Option<String>,
    /// Longest a run may take; default one hour.
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Workflow packs and context retrieval (see docs/18-workflow-contract.md).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkflowSettings {
    /// Off turns off workflow packs and project instruction files entirely.
    pub enabled: bool,
    /// Workflow folders, such as a clone of Agent-Custom-Workflow.
    pub packs: Vec<PathBuf>,
    pub enforce: Enforce,
    /// With no `packs`, use the bundled Agent-Custom-Workflow snapshot.
    pub default_pack: bool,
}

impl Default for WorkflowSettings {
    fn default() -> Self {
        WorkflowSettings {
            enabled: true,
            packs: Vec::new(),
            enforce: Enforce::Warn,
            default_pack: true,
        }
    }
}

/// Whether agents must load their required context before writing or running anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforce {
    RequiredBeforeAction,
    Warn,
    Off,
}

/// Global and project memory (see docs/17-memory.md).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct MemorySettings {
    pub enabled: bool,
    /// Whether agents may save memories in each scope: `auto`, `ask`, or `off`.
    pub write: MemoryWrite,
    /// Add the best-matching memories to each message before the root agent sees it.
    pub recall: bool,
    pub max_index_tokens: usize,
    pub max_recall_tokens: usize,
}

impl Default for MemorySettings {
    fn default() -> Self {
        MemorySettings {
            enabled: true,
            write: MemoryWrite::default(),
            recall: true,
            max_index_tokens: 1500,
            max_recall_tokens: 1500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct MemoryWrite {
    pub global: WritePolicy,
    pub project: WritePolicy,
    pub project_private: WritePolicy,
}

impl Default for MemoryWrite {
    fn default() -> Self {
        MemoryWrite {
            global: WritePolicy::Ask,
            project: WritePolicy::Ask,
            project_private: WritePolicy::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    Auto,
    Ask,
    Off,
}

/// Where plugins are loaded from, besides `<config>/plugins`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ExtensionSettings {
    /// Plugin folders, or folders of plugins, in the Claude Code layout.
    #[serde(default)]
    pub plugin_dirs: Vec<PathBuf>,
    /// Plugins, by name, that are found but not loaded.
    #[serde(default)]
    pub disabled_plugins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct HookSettings {
    /// `session.start`, `prompt.submit`, `tool.before`, `tool.after`, `agent.spawn`, or
    /// `agent.complete` (Claude Code names such as `PreToolUse` also work).
    pub event: String,
    /// Tool names, `|`-separated; empty matches every tool.
    #[serde(default)]
    pub matcher: String,
    pub command: String,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// MCP servers available to every session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpSettings {
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerSettings>,
    /// Project folders whose `.mcp.json` servers may start. Other projects' files are reported
    /// but not run, since a cloned repository could otherwise run any command.
    #[serde(default)]
    pub trusted_projects: Vec<PathBuf>,
}

/// One `[mcp.servers.<name>]` entry: a local command or a streamable-HTTP URL.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpServerSettings {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Environment variables read from the keychain, such as API tokens:
    /// `secret_env = { GITHUB_TOKEN = { secret = "github" } }`.
    #[serde(default)]
    pub secret_env: BTreeMap<String, SecretRef>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Sent as `Authorization: Bearer <secret>` to an HTTP server.
    #[serde(default)]
    pub bearer_token: Option<SecretRef>,
    /// Tools that run without asking, by MCP tool name, or `"*"` for all of them.
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub timeout_s: Option<u64>,
    /// The connector catalogue entry it was added from, if any.
    #[serde(default)]
    pub catalog: Option<String>,
}

impl Default for McpServerSettings {
    fn default() -> Self {
        McpServerSettings {
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
            bearer_token: None,
            allow: Vec::new(),
            enabled: true,
            timeout_s: None,
            catalog: None,
        }
    }
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DaemonSettings {
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentSettings {
    pub max_steps: u32,
    pub resume_days: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ModelSettings {
    /// Model for new sessions, as `<endpoint>:<model>`, for example `openrouter:qwen/qwen3-coder`.
    #[serde(default)]
    pub default: Option<String>,
    /// Models subagents may be routed to.
    #[serde(default)]
    pub catalog: Vec<CatalogEntry>,
    /// Folders scanned recursively for GGUF models.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    /// Where models downloaded from Hugging Face go; see [`ModelSettings::download_dir`].
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
    /// Sessions may offer to download a suitable local model from Hugging Face when the best
    /// model available is poor for the task (see docs/04-models-and-routing.md). Nothing is
    /// downloaded without the user's consent.
    #[serde(default = "enabled")]
    pub suggest_downloads: bool,
    /// `[[models.suggestions]]`: the repositories suggestions choose from. Setting it replaces
    /// the built-in list ([`default_suggestions`]).
    #[serde(default = "default_suggestions")]
    pub suggestions: Vec<SuggestionCandidate>,
}

impl Default for ModelSettings {
    fn default() -> Self {
        ModelSettings {
            default: None,
            catalog: Vec::new(),
            folders: Vec::new(),
            download_dir: None,
            suggest_downloads: true,
            suggestions: default_suggestions(),
        }
    }
}

/// A Hugging Face GGUF repository that model suggestions may offer, written as
/// `[[models.suggestions]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SuggestionCandidate {
    /// `owner/name` of a GGUF repository.
    pub repo: String,
    /// What it is good for: `chat`, `code`, `tools` (reliable tool calling), `vision` (needs
    /// `mmproj`).
    pub capabilities: Vec<String>,
    /// `small`, `medium`, or `large`: among choices that fit equally well, larger is preferred.
    #[serde(default = "default_size_class")]
    pub size: String,
    /// The context length the model supports, in tokens.
    pub context: u32,
    /// Download the multimodal projector too (vision models).
    #[serde(default)]
    pub mmproj: bool,
}

fn default_size_class() -> String {
    "small".into()
}

/// The built-in suggestion list: well-known GGUF repositories for general chat (small and
/// medium), code, and vision.
pub fn default_suggestions() -> Vec<SuggestionCandidate> {
    let row = |repo: &str, capabilities: &[&str], size: &str, context: u32, mmproj: bool| {
        SuggestionCandidate {
            repo: repo.into(),
            capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
            size: size.into(),
            context,
            mmproj,
        }
    };
    vec![
        row(
            "Qwen/Qwen3-4B-GGUF",
            &["chat", "tools"],
            "small",
            32_768,
            false,
        ),
        row(
            "Qwen/Qwen3-14B-GGUF",
            &["chat", "tools"],
            "medium",
            32_768,
            false,
        ),
        row(
            "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF",
            &["code", "chat", "tools"],
            "small",
            32_768,
            false,
        ),
        row(
            "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF",
            &["code", "chat", "tools"],
            "medium",
            262_144,
            false,
        ),
        row(
            "unsloth/gemma-3-4b-it-GGUF",
            &["vision", "chat"],
            "small",
            131_072,
            true,
        ),
        row(
            "unsloth/gemma-3-12b-it-GGUF",
            &["vision", "chat"],
            "medium",
            131_072,
            true,
        ),
    ]
}

impl ModelSettings {
    /// Where downloads go: `download_dir`, else the first of `folders`, else
    /// `<data_dir>/models`. Each repository lands in `<owner>/<repo>/` below it.
    pub fn download_dir(&self, data_dir: &Path) -> PathBuf {
        self.download_dir
            .clone()
            .or_else(|| self.folders.first().cloned())
            .unwrap_or_else(|| data_dir.join("models"))
    }
}

/// Inference runtimes the daemon supervises.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RuntimeSettings {
    pub llamacpp: LlamaCppSettings,
    #[serde(default)]
    pub vllm: VllmSettings,
}

/// vLLM serves Hugging Face model folders (Linux, WSL2, or a remote host).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct VllmSettings {
    /// The `vllm` executable (from a managed environment, or on `PATH`).
    pub command: String,
    /// Upper bound for vLLM's `--gpu-memory-utilization`.
    pub max_gpu_memory_utilization: f64,
    pub extra_args: Vec<String>,
}

impl Default for VllmSettings {
    fn default() -> Self {
        VllmSettings {
            command: "vllm".into(),
            max_gpu_memory_utilization: 0.9,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct LlamaCppSettings {
    pub backend: String,
    /// A `llama-server` executable to use instead of a runtime pack.
    #[serde(default)]
    pub custom_path: Option<PathBuf>,
    pub idle_unload_s: u64,
    pub context: u32,
    pub parallel: u32,
    pub cache_type: String,
    pub ram_headroom_mb: u64,
    /// Extra arguments passed to every `llama-server`.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

/// Overrides for the hardware probe, for machines it cannot read (or to plan as if memory were
/// smaller). Values are in MiB.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct HardwareSettings {
    #[serde(default)]
    pub vram_mb: Option<u64>,
    #[serde(default)]
    pub ram_mb: Option<u64>,
}

/// One row of the model table, written as `[[models.catalog]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CatalogEntry {
    /// Short, unique name that delegation rules and agents use to refer to the model.
    pub id: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    /// Whether the model runs on this machine (or one you control) or at a hosted provider.
    pub location: Location,
    pub tier: Tier,
    /// Free-form labels, such as `code` or `vision`, that rules can match.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Tokens the model reads per request, for context breakdowns. llama.cpp models report
    /// their own; for other models the window is unknown unless given here.
    #[serde(default)]
    pub context_window: Option<u32>,
}

/// Where a model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    Local,
    Cloud,
}

impl Location {
    pub fn as_str(self) -> &'static str {
        match self {
            Location::Local => "local",
            Location::Cloud => "cloud",
        }
    }
}

impl std::str::FromStr for Location {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "local" => Ok(Location::Local),
            "cloud" => Ok(Location::Cloud),
            other => Err(format!("unknown location {other:?}; use local or cloud")),
        }
    }
}

/// How subagents are assigned to models.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RoutingSettings {
    /// Tie-breaker among suitable models: the cheapest, or local or cloud models first.
    #[serde(default)]
    pub prefer: Preference,
    /// Checked in order; the first rule matching a task decides its candidates.
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Preference {
    #[default]
    Cheapest,
    Local,
    Cloud,
}

impl Preference {
    pub fn as_str(self) -> &'static str {
        match self {
            Preference::Cheapest => "cheapest",
            Preference::Local => "local",
            Preference::Cloud => "cloud",
        }
    }
}

/// A delegation rule, written as `[[routing.rules]]`. Absent conditions match every task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RoutingRule {
    /// Matches tasks asking for exactly this tier.
    #[serde(default)]
    pub when_tier: Option<Tier>,
    /// Matches tasks carrying this tag.
    #[serde(default)]
    pub when_tag: Option<String>,
    /// Restricts matching tasks to local or cloud models.
    #[serde(default)]
    pub location: Option<Location>,
    /// Model table ids to use, in order of preference.
    #[serde(default, rename = "use")]
    pub use_models: Vec<String>,
}

/// How capable a model is; tasks ask for a minimum tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Basic,
    #[default]
    Standard,
    Strong,
    Frontier,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Basic => "basic",
            Tier::Standard => "standard",
            Tier::Strong => "strong",
            Tier::Frontier => "frontier",
        }
    }
}

impl std::str::FromStr for Tier {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "basic" => Ok(Tier::Basic),
            "standard" => Ok(Tier::Standard),
            "strong" => Ok(Tier::Strong),
            "frontier" => Ok(Tier::Frontier),
            other => Err(format!(
                "unknown tier {other:?}; use basic, standard, strong, or frontier"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct EndpointSettings {
    /// Base URL ending before `/chat/completions`, for example `https://openrouter.ai/api/v1`.
    pub base_url: String,
    /// Keychain secret holding the API key, written as `api_key = { secret = "name" }`.
    #[serde(default)]
    pub api_key: Option<SecretRef>,
    /// Prices per model id, for cost accounting. Models without prices report no cost.
    #[serde(default)]
    pub pricing: BTreeMap<String, Pricing>,
    /// The API the endpoint speaks, naming its model client: `openai` (OpenAI-compatible Chat
    /// Completions, the default) or `llamacpp` (the same, with llama.cpp's tokenizer).
    #[serde(default = "default_endpoint_kind")]
    pub kind: String,
}

fn default_endpoint_kind() -> String {
    "openai".into()
}

/// A reference to a keychain secret by name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SecretRef {
    pub secret: SecretName,
}
