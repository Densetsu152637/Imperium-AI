//! The settings a user can change from an app: one table listing each key with its label,
//! description, and kind, plus reading their effective values and writing them to the user's
//! `config.toml`.
//!
//! Structured lists and maps (`endpoints`, `models.catalog`, `routing.rules`, `mcp.servers`,
//! `projects`, `schedules`, `hooks`, `github.watch`, `media.speech`, `media.transcription`)
//! are not listed; they are edited in the file. `permissions` (grants and rules) has its own
//! methods, `permissions.get` and `permissions.set`; so has `budgets` (`budgets.get`,
//! `budgets.set`). Add a new scalar or list setting by adding a
//! row to [`SETTINGS`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value as Json;
use toml_edit::{DocumentMut, Item, TableLike};

use crate::layers::{Config, ConfigError, Layer, Source, load};
use crate::{Paths, SecretName};

/// The type of a setting's value, which decides how an app edits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    Integer,
    Number,
    String,
    /// A file or folder path, as a string.
    Path,
    PathList,
    StringList,
    /// One of the setting's `options`.
    Enum,
    /// The name of a keychain secret, written as `{ secret = "<name>" }`; never the secret.
    SecretRef,
}

impl SettingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingKind::Bool => "bool",
            SettingKind::Integer => "integer",
            SettingKind::Number => "number",
            SettingKind::String => "string",
            SettingKind::Path => "path",
            SettingKind::PathList => "path_list",
            SettingKind::StringList => "string_list",
            SettingKind::Enum => "enum",
            SettingKind::SecretRef => "secret_ref",
        }
    }
}

/// One user-settable key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingDef {
    /// Dotted path, such as `runtime.llamacpp.context`.
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: SettingKind,
    /// Allowed values of an [`SettingKind::Enum`].
    pub options: &'static [&'static str],
    /// Whether the daemon must restart before a change applies. The daemon reads its
    /// configuration once at start, so this is true for nearly every setting; the few it
    /// applies at once are marked with `live`.
    pub restart: bool,
}

impl SettingDef {
    /// The top-level table, for grouping.
    pub fn section(&self) -> &'static str {
        self.key.split('.').next().unwrap_or(self.key)
    }
}

const fn def(
    key: &'static str,
    label: &'static str,
    kind: SettingKind,
    description: &'static str,
) -> SettingDef {
    SettingDef {
        key,
        label,
        description,
        kind,
        options: &[],
        restart: true,
    }
}

const fn choice(
    key: &'static str,
    label: &'static str,
    options: &'static [&'static str],
    description: &'static str,
) -> SettingDef {
    SettingDef {
        key,
        label,
        description,
        kind: SettingKind::Enum,
        options,
        restart: true,
    }
}

/// A setting the running daemon applies at once, without a restart.
const fn live(def: SettingDef) -> SettingDef {
    SettingDef {
        restart: false,
        ..def
    }
}

use SettingKind::{Bool, Integer, Number, Path as PathKind, PathList, SecretRef, StringList};
const STRING: SettingKind = SettingKind::String;
const WRITE_POLICY: &[&str] = &["auto", "ask", "off"];
const SESSION_SEARCH_SCOPE: &[&str] = &["environment", "all"];
const LIST_MODELS: &[&str] = &["root", "all", "off"];

/// Every setting an app may show and change, in display order.
#[rustfmt::skip]
pub static SETTINGS: &[SettingDef] = &[
    def("daemon.port", "Port", Integer, "Port of the local WebSocket listener; 0 picks a free port at each start."),
    def("daemon.socket", "Local socket", Bool, "Also listen on a local socket only your user can open (a Unix domain socket, or a Windows named pipe), speaking the same protocol with the same token, so another machine can reach the daemon through an SSH forward. Off by default."),
    def("agents.max_steps", "Steps per turn", Integer, "Model calls an agent may make in one turn before it stops."),
    def("agents.resume_days", "Resume sessions (days)", Integer, "Sessions active within this many days continue after the daemon restarts."),
    live(def("sessions.worktree_dir", "Worktree folder", PathKind, "Where sessions started in a git worktree put it, as <folder>/<repo>/<branch>. Empty: inside the repository, in .worktrees/<branch> (excluded from git).")),
    live(def("workspaces.root", "Working folders", PathKind, "The folder that holds a working folder for every environment: its Memory.md and a scratch folder per session (deleted when the session is archived). Changing it moves what is there. Empty: <data>/workspaces.")),
    live(def("models.default", "Default model", STRING, "Model for new sessions, as <endpoint>:<model>.")),
    live(def("models.folders", "Model folders", PathList, "Folders scanned for GGUF models, LM Studio and Hugging Face cache folders, and Hugging Face model folders; scanned again at once when changed.")),
    def("models.download_dir", "Download folder", PathKind, "Where Hugging Face downloads go (absolute). Defaults to the first model folder, else <data>/models."),
    live(choice("models.effort", "Reasoning effort", &["low", "medium", "high", "max"], "How hard models that can reason think before answering, for new sessions; each session can change its own. Models that cannot reason are sent nothing.")),
    live(def("models.suggest_downloads", "Suggest model downloads", Bool, "Sessions offer to download a suitable local model from Hugging Face when the best one available is poor for the task. Nothing downloads without your consent.")),
    live(choice("models.list_models", "Model list for agents", LIST_MODELS, "Which agents of new sessions get list_models, which lists the models they may delegate to with their cards and whether each fits in memory: root (the agent that delegates), all (subagents too), or off.")),
    choice("routing.prefer", "Routing preference", &["cheapest", "local", "cloud"], "Tie-breaker among suitable models for subagents."),
    def("routing.builtin_cpu_lane", "CPU lane for critiques", Bool, "Send critique, review, verify and proof-check subagent tasks to a model pinned to the CPU lane when one can take them now, unless a rule of yours matches the task."),
    def("routing.max_queue_wait_s", "Longest queue wait (seconds)", Integer, "A local model whose expected wait for a slot is longer than this is skipped for the next eligible route, with a notice; 0 turns the check off."),
    def("routing.long_context_tokens", "Long context (tokens)", Integer, "Context window a model needs to count as long-context for a task that requires it."),
    def("routing.escalation_attempts", "Escalation retries", Integer, "Times a failed subagent is retried on a stronger route (a rule's escalate list, else the next tier up); Off by default (0): each retry is another model run and is charged to its budget. An unsure local model is never retried on a hosted one."),
    choice("runtime.llamacpp.backend", "llama.cpp backend", &["auto", "cpu", "vulkan", "cuda12", "cuda13", "rocm", "metal"], "Runtime pack used to run local GGUF models."),
    live(def("runtime.llamacpp.custom_path", "Custom llama-server", PathKind, "Your own llama-server executable, used instead of an installed runtime for models loaded from then on.")),
    def("runtime.llamacpp.find_existing", "Use a llama-server found here", Bool, "With no runtime installed and no custom llama-server, use one already on this computer (on PATH or in a llama.cpp build in your home folder) that starts and supports the flags Imperium passes."),
    def("runtime.llamacpp.context", "Max context per agent", STRING, "Most tokens one agent may use on a local model: auto (what the model was trained for, capped to what fits in memory) or a number of tokens. The KV cache starts small and grows on demand up to it."),
    def("runtime.llamacpp.auto_install", "Install llama.cpp automatically", Bool, "With no usable llama-server (or only a CPU build on a machine with a GPU), download and verify llama.cpp's official build for this computer the first time a local model is needed."),
    choice("runtime.llamacpp.update_external", "Update a package-manager llama.cpp", &["off", "ask", "auto"], "When a llama-server installed with winget, scoop, Chocolatey, or Homebrew is older than the llama.cpp release Imperium pins: off, ask (offer Update in Local models), or auto (upgrade it while no model is loaded from it)."),
    def("runtime.llamacpp.parallel", "Parallel slots", Integer, "Slots per loaded model (agents served at once)."),
    choice("runtime.llamacpp.cache_type", "KV cache type", &["f16", "q8_0", "q4_0"], "Precision of the KV cache."),
    def("runtime.llamacpp.idle_unload_s", "Idle unload (seconds)", Integer, "Seconds an unused model stays loaded."),
    def("runtime.llamacpp.ram_headroom_mb", "RAM headroom (MB)", Integer, "System memory kept free when placing models."),
    def("runtime.llamacpp.extra_args", "Extra llama-server arguments", StringList, "Arguments added to every llama-server, such as --flash-attn on."),
    def("runtime.vllm.command", "vLLM command", STRING, "The vllm executable for Hugging Face model folders (Linux and WSL2)."),
    def("runtime.vllm.max_gpu_memory_utilization", "vLLM GPU memory share", Number, "Upper bound for vLLM's share of GPU memory (0-1)."),
    def("runtime.vllm.extra_args", "Extra vLLM arguments", StringList, "Arguments added to vllm serve."),
    def("hardware.vram_mb", "VRAM override (MB)", Integer, "Overrides the probed GPU memory; empty uses the probe."),
    def("hardware.ram_mb", "RAM override (MB)", Integer, "Overrides the probed system memory; empty uses the probe."),
    def("mcp.trusted_projects", "Trusted MCP projects", PathList, "Project folders whose .mcp.json servers may start."),
    def("extensions.plugin_dirs", "Plugin folders", PathList, "More plugin folders (Claude Code layout), besides <config>/plugins."),
    def("extensions.disabled_plugins", "Disabled plugins", StringList, "Plugins, by name, whose skills, commands, hooks, and servers are not loaded."),
    def("extensions.lazy", "Load extensions on use", Bool, "Agents start with each skill's and plugin's name and description, and load the rest on first use. Off loads every skill's instructions and starts every plugin's servers up front."),
    def("extensions.eager", "Always-loaded extensions", StringList, "Skills and plugins, by name, loaded up front even when extensions load on use."),
    def("memory.enabled", "Memory", Bool, "Global memory and each environment's Memory.md for agents."),
    choice("memory.write.global", "Save global memories", WRITE_POLICY, "Whether agents may save global memories (about you, everywhere)."),
    choice("memory.write.environment", "Save environment memory", WRITE_POLICY, "Whether agents may write to an environment's Memory.md, which holds what is true of that environment and its projects. Every later session there reads it, so ask shows the whole entry."),
    def("memory.recall", "Recall memories", Bool, "Put the best-matching memories before each message."),
    def("memory.max_index_tokens", "Memory index budget", Integer, "Tokens for the memory index in agent instructions."),
    def("memory.max_recall_tokens", "Recall budget", Integer, "Tokens for recalled memories per message."),
    def("memory.session_search", "Session search", Bool, "Index sessions in the background so they can be searched, and let agents search earlier sessions."),
    choice("memory.session_search_scope", "Agents search sessions in", SESSION_SEARCH_SCOPE, "environment: only the agent's own environment; all: every environment."),
    def("memory.embedding_model", "Embedding model", STRING, "Model table id or <endpoint>:<model> of an embedding model for search by meaning; empty searches by keyword only."),
    live(def("policy.personality", "Personality", STRING, "The root agent's voice with the user, by id: a file in <config>/personalities/ (which wins) or a workflow pack's personalities/ folder. none turns it off; an id that is not found uses marcus_aurelius. Applies to new sessions; subagents never get one.")),
    def("workflow.enabled", "Workflow packs", Bool, "Serve workflow packs and project instruction files to agents."),
    def("workflow.packs", "Workflow folders", PathList, "Workflow folders, such as a clone of Agent-Custom-Workflow."),
    def("workflow.default_pack", "Bundled workflow", Bool, "With no workflow folders, use the bundled Agent-Custom-Workflow snapshot."),
    choice("workflow.enforce", "Enforce workflow", &["required-before-action", "warn", "off"], "Whether writes and commands wait until required context is loaded."),
    def("github.autofix.enabled", "GitHub auto-fix", Bool, "Fix your pull requests whose checks fail."),
    choice("github.autofix.push", "Auto-fix pushes", &["ask", "auto"], "Ask before each push, or push without asking."),
    def("github.autofix.max_attempts_per_pr", "Auto-fix attempts", Integer, "Failing commits per pull request before auto-fix stops."),
    def("github.autofix.poll_s", "Auto-fix polling (seconds)", Integer, "How often pull requests are checked."),
    def("github.autofix.model", "Auto-fix model", STRING, "Model for fix sessions; empty uses the default model."),
    live(choice("permissions.mode", "Permission mode", &["ask", "default", "accept_edits", "plan", "full"], "How new sessions approve their agents' actions: ask (before anything that is not read-only, whatever the allow rules say), default (writes and commands ask unless a rule allows them), accept_edits (file edits run without asking), plan (read and plan only: no writes or commands), or full (everything runs except what deny rules or the safety floor stop). Each session can change its own.")),
    choice("isolation.tier", "Isolation tier", &["policy", "os", "container"], "Where agents' commands run."),
    def("isolation.container_runtime", "Container runtime", STRING, "Container runtime for the container tier, such as docker."),
    def("isolation.container_image", "Container image", STRING, "Image commands run in for the container tier."),
    def("isolation.network", "Network in sandbox", Bool, "Whether sandboxed and container commands may use the network."),
    def("web.enabled", "Web tools", Bool, "Offer web_fetch (and web_search with a provider) to agents."),
    choice("web.search", "Web search provider", &["none", "searxng", "brave", "tavily", "kagi", "exa"], "Provider behind web_search."),
    def("web.searxng_url", "SearXNG URL", STRING, "A SearXNG instance with the JSON format enabled."),
    def("web.api_key", "Search API key", SecretRef, "Keychain secret holding the Brave, Tavily, Kagi, or Exa key."),
    def("web.allow_domains", "Allowed domains", StringList, "Domains web_fetch reads without asking (subdomains included)."),
    def("web.cache_ttl_s", "Fetch cache lifetime (seconds)", Integer, "How long web_fetch reuses a page it has read; 0 turns the cache off. Pages with Cache-Control no-store, private or no-cache, cookies, or authentication are never cached."),
    def("web.cache_max_mb", "Fetch cache size (MB)", Integer, "The most disk the web_fetch cache may use; the oldest pages go first. 0 turns the cache off."),
    choice("web.robots", "robots.txt", &["respect", "off"], "respect: web_fetch refuses pages the host's robots.txt disallows for Imperium; off: it does not check."),
    choice("web.robots_unreachable", "Unreadable robots.txt", &["allow", "deny"], "When a host's robots.txt cannot be read (a server error or no connection): allow fetching (fail open) or refuse it (fail closed). A missing file (404) always allows."),
    def("web.browser", "Browser tools", Bool, "Offer browser tools (Chrome, Chromium, or Edge over CDP)."),
    def("web.browser_path", "Browser path", PathKind, "The browser to drive; found automatically when empty."),
    def("web.headless", "Headless browser", Bool, "Run the browser without a window."),
    choice("media.image.backend", "Image backend", &["openai", "comfyui"], "Image generation through an images API endpoint or ComfyUI."),
    def("media.image.endpoint", "Image endpoint", STRING, "The [endpoints] entry for the openai backend."),
    def("media.image.model", "Image model", STRING, "Model for the openai backend."),
    def("media.image.comfyui_url", "ComfyUI URL", STRING, "ComfyUI server for the comfyui backend."),
    def("media.image.workflow", "ComfyUI workflow", PathKind, "API-format ComfyUI workflow containing {{prompt}}."),
    def("phone.bind", "Phone listener address", STRING, "Address to serve paired phones on, such as a Tailscale address with a port; empty is off."),
    live(def("api.openai_compatible.enabled", "OpenAI-compatible API", Bool, "Serve your local models to other tools as an OpenAI-compatible API on this computer. Requests need the API token shown in Settings → API; the server starts and stops at once.")),
    live(def("api.openai_compatible.port", "API port", Integer, "Port of the API server; 0 picks a free one the first time the API is enabled and keeps it.")),
    live(def("api.openai_compatible.bind", "API address", STRING, "Address the API server listens on. Only loopback addresses (127.0.0.1) are accepted unless the next setting allows more.")),
    live(def("api.openai_compatible.allow_lan", "Allow the API on the network", Bool, "Lets the API address be one that other computers can reach. Risky: the API uses plain HTTP, so anyone on the network who learns the token can use your models (and the hosted ones you expose).")),
    live(def("api.openai_compatible.max_concurrent", "API calls at once", Integer, "Most calls the API serves at once, running or waiting for a model; one more is refused with a 429 error so a runaway tool cannot pile up work.")),
    live(def("api.openai_compatible.expose", "Hosted models in the API", StringList, "Specs (<endpoint>:<model>) of hosted models other tools may use through the API, spending your keys and money. Local models need no entry; hosted ones are never served unless listed.")),
    def("pets.enabled", "Pets", Bool, "Show a desktop pet."),
    def("pets.pet", "Pet", STRING, "The pet's id; the first found when empty."),
    def("pets.folders", "Pet folders", PathList, "More folders of Pixel Pets v3 packages, besides <config>/pets."),
    def("pets.sleep_after_minutes", "Pet sleeps after (minutes)", Integer, "Idle minutes before the pet sleeps."),
    def("pets.fidget", "Pet fidgets", Bool, "Idle fidget animations."),
    choice("resume.mode", "When usage runs out", &["wait_for_reset", "at_time", "retry_every", "stop"], "How sessions wait when a provider's usage or rate limit is hit."),
    def("resume.at_time", "Resume at", STRING, "Local HH:MM time to resume, for at_time."),
    def("resume.retry_every_minutes", "Retry every (minutes)", Integer, "Retry interval, for retry_every."),
    def("resume.max_wait_hours", "Longest wait (hours)", Integer, "Longest a session waits in one turn."),
    live(def("context.auto_compact_at", "Compact at", Number, "Compact an agent's conversation mid-turn when the next model call would use this share of its model's context window (0.85 is 85%); 0 never does. Needs a known context window.")),
    def("sessions.auto_archive_after_days", "Archive idle sessions after (days)", Integer, "Archive a session after this many days without activity; 0 never does. Working, waiting, pinned, and project-agent sessions are never archived."),
    def("sessions.auto_archive_on_pr_close", "Archive when the pull request closes", Bool, "Archive a session once the pull request it watches is merged or closed."),
    def("sessions.agent_state_keep", "Keep agent state", Bool, "Keep each agent's conversation, plan, and usage beside the session's files, so finished subagents can be resumed with a follow-up, also after a restart. Deleted with the session."),
    def("sessions.checkpoints", "Checkpoints", Bool, "Before each turn that changes files in a git repository, record the session's files as a commit on a private ref (refs/imperium/checkpoints/<session>/<n>), so the files and the conversation can be rewound. The index, HEAD, and branches are never touched."),
    live(def("sessions.attachment_max_mib", "Largest attachment (MiB)", Integer, "The largest file that can be attached to a message. Files pasted or dropped from outside the session's folders are copied into its attachments folder; uploads from the app travel over the daemon connection, which takes about 12 MiB at most.")),
];

/// The setting with this dotted key.
pub fn setting(key: &str) -> Option<&'static SettingDef> {
    SETTINGS.iter().find(|def| def.key == key)
}

/// A setting with its default and effective values (JSON) and the layer that set it.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingState {
    pub def: &'static SettingDef,
    pub default: Json,
    /// The effective value; for a secret reference, the secret's name.
    pub value: Json,
    pub source: Source,
}

impl SettingState {
    /// Whether a layer above the user file (the app's model settings, a project, or an
    /// environment variable) sets it, so the user file's value does not apply.
    pub fn overridden(&self) -> bool {
        matches!(
            self.source,
            Source::App(_) | Source::Project(_) | Source::Environment(_)
        )
    }
}

/// Every setting's state in `config`.
pub fn describe(config: &Config) -> Vec<SettingState> {
    let defaults = default_values();
    SETTINGS
        .iter()
        .map(|def| state(def, config, &defaults))
        .collect()
}

/// One setting's state in `config`, or `None` for an unknown key.
pub fn describe_key(config: &Config, key: &str) -> Option<SettingState> {
    setting(key).map(|def| state(def, config, &default_values()))
}

fn default_values() -> Json {
    let settings = Config::from_layers([Layer::defaults()])
        .settings()
        .expect("built-in defaults are valid (covered by tests)");
    serde_json::to_value(settings).expect("settings serialise to JSON")
}

fn state(def: &'static SettingDef, config: &Config, defaults: &Json) -> SettingState {
    let default = lookup(defaults, def.key);
    let default = match def.kind {
        SettingKind::SecretRef => secret_name(&default),
        _ => default,
    };
    let leaf = match def.kind {
        SettingKind::SecretRef => format!("{}.secret", def.key),
        _ => def.key.to_string(),
    };
    let explained = config.explain(def.key);
    let source = config
        .explain(&leaf)
        .and_then(|explained| explained.source.cloned())
        .unwrap_or(Source::Default);
    let value = match explained {
        Some(explained) => match def.kind {
            SettingKind::SecretRef => secret_name(&toml_to_json(explained.value)),
            _ => toml_to_json(explained.value),
        },
        None => default.clone(),
    };
    SettingState {
        def,
        default,
        value,
        source,
    }
}

fn lookup(value: &Json, key: &str) -> Json {
    key.split('.')
        .try_fold(value, |value, part| value.get(part))
        .cloned()
        .unwrap_or(Json::Null)
}

fn secret_name(value: &Json) -> Json {
    match value {
        Json::Object(table) => table.get("secret").cloned().unwrap_or(Json::Null),
        other => other.clone(),
    }
}

fn toml_to_json(value: &toml::Value) -> Json {
    match value {
        toml::Value::String(text) => Json::String(text.clone()),
        toml::Value::Integer(number) => Json::from(*number),
        toml::Value::Float(number) => Json::from(*number),
        toml::Value::Boolean(flag) => Json::Bool(*flag),
        toml::Value::Datetime(datetime) => Json::String(datetime.to_string()),
        toml::Value::Array(items) => Json::Array(items.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => Json::Object(
            table
                .iter()
                .map(|(key, value)| (key.clone(), toml_to_json(value)))
                .collect(),
        ),
    }
}

/// Why a setting could not be changed.
#[derive(Debug, thiserror::Error)]
pub enum SetError {
    #[error("unknown setting {0:?}")]
    UnknownKey(String),
    #[error("{key}: {message}")]
    InvalidValue { key: String, message: String },
    #[error("cannot edit {}: {message}", path.display())]
    UserFile { path: PathBuf, message: String },
    #[error("could not write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file was restored to what it was before.
    #[error("the change was not saved: {0}")]
    Rejected(ConfigError),
}

/// Serialises edits of the user file within this process.
static USER_FILE: Mutex<()> = Mutex::new(());

/// Writes `value` for `key` into the user's `config.toml` (creating it when missing), or
/// removes the key when `value` is null so a lower layer applies. Comments, formatting, and
/// keys this build does not know are kept. The whole configuration (user file and `env`, no
/// project) is then loaded and validated; if that fails the file is restored and the error
/// returned. Returns the setting's new state.
pub fn set_user_setting(
    paths: &Paths,
    key: &str,
    value: &Json,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<SettingState, SetError> {
    let def = setting(key).ok_or_else(|| SetError::UnknownKey(key.to_string()))?;
    let new = match value {
        Json::Null => None,
        value => Some(
            to_toml(def, value).map_err(|message| SetError::InvalidValue {
                key: key.to_string(),
                message,
            })?,
        ),
    };
    let config = edit_user_file(paths, env, |document| edit(document, key, new))?;
    Ok(state(def, &config, &default_values()))
}

/// Applies `change` to the user's `config.toml` (creating it when missing) and saves it, keeping
/// comments, formatting, and keys this build does not know. The whole configuration (user file
/// and `env`, no project) is then loaded and validated; if that fails the file is restored and
/// the error returned. Returns the configuration as loaded after the change.
pub(crate) fn edit_user_file(
    paths: &Paths,
    env: impl IntoIterator<Item = (String, String)>,
    change: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
) -> Result<Config, SetError> {
    let _guard = USER_FILE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = paths.user_config_file();
    let previous = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(SetError::Write { path, source }),
    };
    let mut document: DocumentMut = previous.as_deref().unwrap_or_default().parse().map_err(
        |error: toml_edit::TomlError| SetError::UserFile {
            path: path.clone(),
            message: format!("it is not valid TOML: {error}"),
        },
    )?;
    change(&mut document).map_err(|message| SetError::UserFile {
        path: path.clone(),
        message,
    })?;
    write(&path, &document.to_string())?;
    let checked = load(paths, None, env).and_then(|config| {
        config.settings()?;
        Ok(config)
    });
    match checked {
        Ok(config) => Ok(config),
        Err(error) => {
            let restored = match &previous {
                Some(text) => write(&path, text),
                None => std::fs::remove_file(&path).map_err(|source| SetError::Write {
                    path: path.clone(),
                    source,
                }),
            };
            restored?;
            Err(SetError::Rejected(error))
        }
    }
}

fn to_toml(def: &SettingDef, value: &Json) -> Result<toml_edit::Value, String> {
    let expected = |what: &str| format!("expected {what}, not {value}");
    let text = || value.as_str().ok_or_else(|| expected("a string"));
    let list = || -> Result<toml_edit::Value, String> {
        let items = value
            .as_array()
            .ok_or_else(|| expected("a list of strings"))?
            .iter()
            .map(|item| item.as_str().ok_or_else(|| expected("a list of strings")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(toml_edit::Array::from_iter(items).into())
    };
    if def.key == "runtime.llamacpp.context" {
        // `auto` or a whole number, written as the number.
        let limit = match value {
            Json::String(text) => crate::ContextLimit::parse(text)?,
            _ => crate::ContextLimit::Tokens(
                value
                    .as_u64()
                    .and_then(|tokens| u32::try_from(tokens).ok())
                    .ok_or_else(|| expected("\"auto\" or a number of tokens"))?,
            ),
        };
        return Ok(match limit {
            crate::ContextLimit::Auto => "auto".into(),
            crate::ContextLimit::Tokens(tokens) => i64::from(tokens).into(),
        });
    }
    Ok(match def.kind {
        SettingKind::Bool => value
            .as_bool()
            .ok_or_else(|| expected("true or false"))?
            .into(),
        SettingKind::Integer => value
            .as_i64()
            .ok_or_else(|| expected("a whole number"))?
            .into(),
        SettingKind::Number => value.as_f64().ok_or_else(|| expected("a number"))?.into(),
        SettingKind::String | SettingKind::Path => text()?.into(),
        SettingKind::Enum => {
            let choice = text()?;
            if !def.options.contains(&choice) {
                return Err(format!(
                    "{choice:?} is not one of {}",
                    def.options.join(", ")
                ));
            }
            choice.into()
        }
        SettingKind::PathList | SettingKind::StringList => list()?,
        SettingKind::SecretRef => {
            let name = text()?;
            name.parse::<SecretName>()
                .map_err(|error| error.to_string())?;
            let mut table = toml_edit::InlineTable::new();
            table.insert("secret", name.into());
            toml_edit::Value::InlineTable(table)
        }
    })
}

/// Sets (or, with `None`, removes) a dotted key in `document`, creating tables as needed.
fn edit(
    document: &mut DocumentMut,
    key: &str,
    new: Option<toml_edit::Value>,
) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    let (leaf, parents) = parts.split_last().expect("keys are not empty");
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for (depth, part) in parents.iter().enumerate() {
        if table.get(part).is_none() {
            if new.is_none() {
                return Ok(());
            }
            let mut child = toml_edit::Table::new();
            child.set_implicit(true);
            table.insert(part, Item::Table(child));
        }
        table = table
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("{} is not a table", parts[..=depth].join(".")))?;
    }
    match new {
        Some(mut value) => match table.get_mut(leaf).and_then(Item::as_value_mut) {
            Some(existing) => {
                *value.decor_mut() = existing.decor().clone();
                *existing = value;
            }
            None => {
                table.insert(leaf, Item::Value(value));
            }
        },
        None => {
            table.remove(leaf);
        }
    }
    Ok(())
}

/// Replaces `path` through a temporary file beside it, so a reader never sees half a file.
fn write(path: &Path, text: &str) -> Result<(), SetError> {
    let failed = |source| SetError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, text).map_err(failed)?;
    std::fs::rename(&temporary, path).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, Paths) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        (home, paths)
    }

    fn entry(paths: &Paths, key: &str, env: &[(&str, &str)]) -> SettingState {
        let config = load(paths, None, owned(env)).unwrap();
        describe_key(&config, key).unwrap()
    }

    fn owned(env: &[(&str, &str)]) -> Vec<(String, String)> {
        env.iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn every_setting_exists_in_the_settings_and_has_a_matching_default() {
        let defaults = default_values();
        let mut keys = std::collections::BTreeSet::new();
        for def in SETTINGS {
            assert!(keys.insert(def.key), "{} is listed twice", def.key);
            assert!(
                !def.label.is_empty() && !def.description.is_empty(),
                "{}",
                def.key
            );
            assert_eq!(
                def.kind == SettingKind::Enum,
                !def.options.is_empty(),
                "{}",
                def.key
            );
            // The key's parent is an object holding the key, unless a parent is unset.
            let (parent, leaf) = def.key.rsplit_once('.').unwrap();
            match lookup(&defaults, parent) {
                Json::Object(table) => assert!(table.contains_key(leaf), "{}", def.key),
                Json::Null => {}
                other => panic!("{}: parent is {other}", def.key),
            }
            let default = lookup(&defaults, def.key);
            let fits = match def.kind {
                _ if default.is_null() => true,
                SettingKind::Bool => default.is_boolean(),
                SettingKind::Integer => default.is_i64() || default.is_u64(),
                SettingKind::Number => default.is_number(),
                SettingKind::String | SettingKind::Path => default.is_string(),
                SettingKind::Enum => default.as_str().is_some_and(|v| def.options.contains(&v)),
                SettingKind::PathList | SettingKind::StringList => default.is_array(),
                SettingKind::SecretRef => default.is_object(),
            };
            assert!(
                fits,
                "{}: default {default} is not a {:?}",
                def.key, def.kind
            );
        }
    }

    #[test]
    fn describe_lists_known_keys_with_defaults_and_sources() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(
            paths.user_config_file(),
            "[web]\nsearch = \"brave\"\napi_key = { secret = \"brave\" }\n",
        )
        .unwrap();
        let config = load(&paths, None, []).unwrap();
        let entries = describe(&config);
        let get = |key: &str| entries.iter().find(|e| e.def.key == key).unwrap();

        let port = get("daemon.port");
        assert_eq!(
            (port.default.clone(), port.value.clone()),
            (0.into(), 0.into())
        );
        assert_eq!(port.source, Source::Default);
        assert_eq!(port.def.section(), "daemon");

        let context = get("runtime.llamacpp.context");
        assert_eq!(context.default, Json::from("auto"));
        assert_eq!(context.def.section(), "runtime");

        let pets = get("pets.enabled");
        assert_eq!(
            (pets.default.clone(), pets.source.kind()),
            (true.into(), "default")
        );
        assert_eq!(get("models.default").value, Json::Null);
        assert_eq!(get("models.folders").default, Json::Array(Vec::new()));
        assert_eq!(get("workflow.enforce").default, Json::from("warn"));

        let search = get("web.search");
        assert_eq!(search.default, Json::from("none"));
        assert_eq!(search.value, Json::from("brave"));
        assert_eq!(search.source.kind(), "user");
        assert_eq!(
            search.def.options,
            ["none", "searxng", "brave", "tavily", "kagi", "exa"]
        );

        let key = get("web.api_key");
        assert_eq!(key.def.kind, SettingKind::SecretRef);
        assert_eq!(key.value, Json::from("brave"), "only the secret's name");
        assert_eq!(key.source.kind(), "user");
    }

    #[test]
    fn set_writes_the_user_file_and_keeps_comments_and_unknown_keys() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        let original = "# my settings\n[daemon]\nport = 7000 # fixed port\n\n[future]\nthing = 1\n";
        std::fs::write(paths.user_config_file(), original).unwrap();

        let state = set_user_setting(&paths, "daemon.port", &Json::from(7100), []).unwrap();
        assert_eq!(
            (state.value.clone(), state.source.kind()),
            (Json::from(7100), "user")
        );
        assert!(state.def.restart);
        let state =
            set_user_setting(&paths, "runtime.llamacpp.context", &Json::from(16384), []).unwrap();
        assert_eq!(state.value, Json::from(16384));
        set_user_setting(&paths, "web.api_key", &Json::from("tavily"), []).unwrap();
        set_user_setting(&paths, "models.folders", &serde_json::json!(["/m"]), []).unwrap();

        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(
            text.starts_with("# my settings\n[daemon]\nport = 7100 # fixed port\n"),
            "{text}"
        );
        assert!(text.contains("[future]\nthing = 1"), "{text}");
        assert!(
            text.contains("[runtime.llamacpp]\ncontext = 16384"),
            "{text}"
        );
        assert!(
            !text.contains("[runtime]\n"),
            "no empty parent header: {text}"
        );
        assert!(text.contains("api_key = { secret = \"tavily\" }"), "{text}");
        assert_eq!(
            entry(&paths, "web.api_key", &[]).value,
            Json::from("tavily")
        );
        assert_eq!(
            entry(&paths, "models.folders", &[]).value,
            serde_json::json!(["/m"])
        );
    }

    #[test]
    fn set_creates_a_missing_user_file() {
        let (_home, paths) = home();
        set_user_setting(&paths, "pets.enabled", &Json::from(false), []).unwrap();
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert_eq!(text, "[pets]\nenabled = false\n");
    }

    #[test]
    fn the_personality_defaults_to_marcus_aurelius_and_can_be_set() {
        let (_home, paths) = home();
        let state = entry(&paths, "policy.personality", &[]);
        assert_eq!(state.default, Json::from("marcus_aurelius"));
        assert_eq!(
            (state.value, state.source.kind()),
            (Json::from("marcus_aurelius"), "default")
        );
        for id in ["luxion", "none"] {
            let state =
                set_user_setting(&paths, "policy.personality", &Json::from(id), []).unwrap();
            assert_eq!((state.value, state.source.kind()), (Json::from(id), "user"));
        }
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert_eq!(text, "[policy]\npersonality = \"none\"\n");
    }

    #[test]
    fn set_null_removes_the_key_so_the_default_applies() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(
            paths.user_config_file(),
            "[web]\n# keep\nsearch = \"brave\"\nenabled = false\n",
        )
        .unwrap();
        let state = set_user_setting(&paths, "web.search", &Json::Null, []).unwrap();
        assert_eq!(
            (state.value.clone(), state.source.kind()),
            (Json::from("none"), "default")
        );
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(!text.contains("search"), "{text}");
        assert!(text.contains("enabled = false"), "{text}");
        // Removing a key that is not there changes nothing.
        set_user_setting(&paths, "runtime.vllm.command", &Json::Null, []).unwrap();
        assert_eq!(
            std::fs::read_to_string(paths.user_config_file()).unwrap(),
            text
        );
    }

    #[test]
    fn invalid_values_are_rejected_and_the_file_is_unchanged() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        let original = "[daemon]\nport = 7000\n";
        std::fs::write(paths.user_config_file(), original).unwrap();
        let unchanged = || {
            assert_eq!(
                std::fs::read_to_string(paths.user_config_file()).unwrap(),
                original
            );
        };

        let error = set_user_setting(&paths, "web.search", &Json::from("bing"), []).unwrap_err();
        assert!(matches!(error, SetError::InvalidValue { .. }), "{error}");
        unchanged();
        let error = set_user_setting(&paths, "daemon.port", &Json::from("high"), []).unwrap_err();
        assert!(matches!(error, SetError::InvalidValue { .. }), "{error}");
        unchanged();
        let error =
            set_user_setting(&paths, "web.api_key", &Json::from("Bad Name"), []).unwrap_err();
        assert!(matches!(error, SetError::InvalidValue { .. }), "{error}");
        unchanged();
        let error = set_user_setting(&paths, "nope.key", &Json::from(1), []).unwrap_err();
        assert!(matches!(error, SetError::UnknownKey(_)), "{error}");
        unchanged();

        // Well-typed but rejected by the loaded configuration: the file is restored.
        let error = set_user_setting(&paths, "daemon.port", &Json::from(70000), []).unwrap_err();
        assert!(matches!(error, SetError::Rejected(_)), "{error}");
        unchanged();
        let error = set_user_setting(&paths, "models.download_dir", &Json::from("relative"), [])
            .unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");
        unchanged();

        // A rejected change to a missing file leaves no file behind.
        let (_home, fresh) = home();
        set_user_setting(&fresh, "resume.mode", &Json::from("at_time"), []).unwrap_err();
        assert!(!fresh.user_config_file().exists());
    }

    #[test]
    fn an_environment_variable_overrides_the_user_file() {
        let (_home, paths) = home();
        let env = [("IMPERIUM_DAEMON__PORT", "8080")];
        let before = entry(&paths, "daemon.port", &env);
        assert_eq!(
            (before.value.clone(), before.source.kind()),
            (Json::from(8080), "env")
        );
        assert!(before.overridden());

        let state =
            set_user_setting(&paths, "daemon.port", &Json::from(7000), owned(&env)).unwrap();
        assert_eq!(state.value, Json::from(8080), "the environment still wins");
        assert_eq!(state.source.kind(), "env");
        assert!(state.overridden());
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.contains("port = 7000"), "{text}");
        assert_eq!(entry(&paths, "daemon.port", &[]).value, Json::from(7000));
    }
}
