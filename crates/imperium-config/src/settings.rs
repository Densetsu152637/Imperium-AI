use std::collections::BTreeMap;
use std::path::PathBuf;

use imperium_models::Pricing;
use serde::Deserialize;

use crate::SecretName;

/// Built-in defaults: the first configuration layer.
pub(crate) const DEFAULTS: &str = r#"
[daemon]
# Port for the local WebSocket listener; 0 picks a free port at each start.
port = 0

[agents]
# Model calls an agent may make in one turn before it stops.
max_steps = 50

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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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

impl Settings {
    /// Checks rules that span keys: model table ids are unique, and routing rules name models
    /// that exist in the table.
    pub fn validate(&self) -> Result<(), String> {
        if self.endpoints.contains_key(LLAMACPP_ENDPOINT) {
            return Err(format!(
                "endpoints.{LLAMACPP_ENDPOINT} is reserved for local models in models.folders"
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.models.catalog {
            if !seen.insert(entry.id.as_str()) {
                return Err(format!("models.catalog: id {:?} is used twice", entry.id));
            }
            let served_locally = entry
                .spec
                .split_once(':')
                .is_some_and(|(endpoint, _)| endpoint == LLAMACPP_ENDPOINT);
            if served_locally && entry.location != Location::Local {
                return Err(format!(
                    "models.catalog: {:?} runs on llama.cpp, so its location must be local",
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
            if schedule.action.prompt.is_some() == schedule.action.command.is_some() {
                return Err(format!(
                    "schedules.{}: set exactly one of action.prompt or action.command",
                    schedule.id
                ));
            }
        }
        if !matches!(self.github.autofix.push.as_str(), "ask" | "auto") {
            return Err("github.autofix.push must be ask or auto".into());
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct GithubSettings {
    #[serde(default)]
    pub watch: Vec<GithubWatch>,
    #[serde(default)]
    pub autofix: AutofixSettings,
}

/// One `[[github.watch]]` entry: a repository and the local clone fixes are made from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GithubWatch {
    /// `owner/name`.
    pub repo: String,
    /// A local clone whose `origin` is the repository.
    pub project: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

/// One `[[schedules]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TriggerSettings {
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub every: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
}

/// A prompt for a new session in `project`, or a shell command run there.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActionSettings {
    pub project: PathBuf,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    /// Model for prompt runs, as `<endpoint>:<model>`; defaults to `models.default`.
    #[serde(default)]
    pub model: Option<String>,
    /// Longest a run may take; default one hour.
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Workflow packs and context retrieval (see docs/18-workflow-contract.md).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct WorkflowSettings {
    /// Off turns off workflow packs and project instruction files entirely.
    pub enabled: bool,
    /// Workflow folders, such as a clone of Agent-Custom-Workflow.
    pub packs: Vec<PathBuf>,
    pub enforce: Enforce,
}

impl Default for WorkflowSettings {
    fn default() -> Self {
        WorkflowSettings {
            enabled: true,
            packs: Vec::new(),
            enforce: Enforce::Warn,
        }
    }
}

/// Whether agents must load their required context before writing or running anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforce {
    RequiredBeforeAction,
    Warn,
    Off,
}

/// Global and project memory (see docs/17-memory.md).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    Auto,
    Ask,
    Off,
}

/// Where plugins are loaded from, besides `<config>/plugins`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ExtensionSettings {
    /// Plugin folders, or folders of plugins, in the Claude Code layout.
    #[serde(default)]
    pub plugin_dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct McpSettings {
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerSettings>,
    /// Project folders whose `.mcp.json` servers may start. Other projects' files are reported
    /// but not run, since a cloned repository could otherwise run any command.
    #[serde(default)]
    pub trusted_projects: Vec<PathBuf>,
}

/// One `[mcp.servers.<name>]` entry: a local command or a streamable-HTTP URL.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerSettings {
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
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DaemonSettings {
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentSettings {
    pub max_steps: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
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
}

/// Inference runtimes the daemon supervises.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RuntimeSettings {
    pub llamacpp: LlamaCppSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct HardwareSettings {
    #[serde(default)]
    pub vram_mb: Option<u64>,
    #[serde(default)]
    pub ram_mb: Option<u64>,
}

/// One row of the model table, written as `[[models.catalog]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
}

/// Where a model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RoutingSettings {
    /// Tie-breaker among suitable models: the cheapest, or local or cloud models first.
    #[serde(default)]
    pub prefer: Preference,
    /// Checked in order; the first rule matching a task decides its candidates.
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EndpointSettings {
    /// Base URL ending before `/chat/completions`, for example `https://openrouter.ai/api/v1`.
    pub base_url: String,
    /// Keychain secret holding the API key, written as `api_key = { secret = "name" }`.
    #[serde(default)]
    pub api_key: Option<SecretRef>,
    /// Prices per model id, for cost accounting. Models without prices report no cost.
    #[serde(default)]
    pub pricing: BTreeMap<String, Pricing>,
}

/// A reference to a keychain secret by name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SecretRef {
    pub secret: SecretName,
}
