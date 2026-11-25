use super::*;

/// `session.export`: a session's transcript, read from its journal, so it works for running
/// and archived sessions alike: messages, tool calls with their arguments and results, the
/// subagent task tree with each subagent's own log, and the files agents attached or created.
#[derive(Debug)]
pub struct SessionExport;

impl Method for SessionExport {
    const NAME: &'static str = "session.export";
    type Params = SessionExportParams;
    type Result = SessionExportResult;
}

/// The formats `session.export` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ExportFormat {
    /// Readable text, for sharing or reading offline.
    Markdown,
    /// The same transcript as a structured document, for tools.
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionExportParams {
    pub session_id: String,
    pub format: ExportFormat,
    /// Include each subagent's own log under the task that started it; `true` when left out.
    /// Off, subagents appear only as tasks with their briefs and reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub include_subagents: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionExportResult {
    pub format: ExportFormat,
    /// A file name to save it under, such as `fix-the-parser.md`.
    pub filename: String,
    /// The transcript as text (a JSON document for `json`).
    pub content: String,
    /// Set when the transcript looks like it holds a credential. Exports are verbatim: they
    /// include whatever the agents read and ran, so check before sharing one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub warning: Option<String>,
}

/// `session.search`: searches what was said and done in sessions, running and archived. The
/// daemon indexes session journals in the background; ranking is by keyword, fused with vector
/// similarity when `memory.embedding_model` is set (see docs/17-memory.md#session-search).
#[derive(Debug)]
pub struct SessionSearch;

impl Method for SessionSearch {
    const NAME: &'static str = "session.search";
    type Params = SessionSearchParams;
    type Result = SessionSearchResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSearchParams {
    pub query: String,
    /// Only sessions in this environment; every session when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment_id: Option<String>,
    /// `true`: archived sessions only; `false`: running ones only; left out: both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub archived: Option<bool>,
    /// At most this many hits (one per session); 20 when left out, at most 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSearchResult {
    /// The best match in each matching session, best first.
    pub hits: Vec<SessionSearchHit>,
    /// Ranking used vector similarity as well as keywords.
    pub semantic: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSearchHit {
    pub session_id: String,
    pub title: String,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub archived: bool,
    /// What matched: `user`, `assistant`, `tool`, `task`, `report`, or `title`.
    pub kind: String,
    /// The index in the session's `session.subscribe` history of the event the match starts
    /// at, to open the session there.
    #[ts(type = "number")]
    pub event_index: u32,
    /// When that event happened, in milliseconds since the Unix epoch.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub at_ms: Option<u64>,
    /// An excerpt around the match.
    pub snippet: String,
    /// Matched words in `snippet` as `[start, end)` UTF-16 offsets (JavaScript string indexes).
    pub highlights: Vec<SearchHighlight>,
    pub score: f64,
    /// Other matches in the same session.
    #[serde(default)]
    pub more: u32,
    /// Found by meaning, without the query's words.
    #[serde(default)]
    pub semantic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SearchHighlight {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateParams {
    /// The environment the session works in: its folders are the sandbox, the first is the
    /// working directory. Give this or `project_dir`; with neither, the session starts in the
    /// built-in Home environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment_id: Option<String>,
    /// Absolute path of a folder to work in, for clients that predate environments. It maps to
    /// the environment whose first folder it is, created (named after the folder) if needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_dir: Option<String>,
    /// A model table id or `<endpoint>:<model>`; defaults to `models.default` from the
    /// configuration. With neither, the session is still created, without a model: messages
    /// sent to it wait (status `needs_model`) until `session.control` `set_model` gives it one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// Start on git worktrees: every folder of the environment that is in a git repository is
    /// replaced by a new worktree on a new branch (`imperium/<session id>` unless
    /// `worktree_branch` names one). Other folders are used as they are. Local folders only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub worktree: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub worktree_branch: Option<String>,
    /// Narrow the session to one folder inside the environment's folders: an absolute path, or
    /// a path relative to the first folder. The folder is then the session's only folder and
    /// working directory; the environment's other folders are closed to it. Not with
    /// `worktree`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub subfolder: Option<String>,
    /// Mount the session's folders read-only: its file tools read and list but never write,
    /// edit or delete there (its scratch folder stays writable), and commands run only under a
    /// tier that can keep them from writing there (`os` and `container`; tier 0 refuses them).
    /// Not with `worktree`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub read_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateResult {
    pub session_id: String,
    /// `None` when no model was given and `models.default` is not set.
    pub model: Option<String>,
    pub isolation: String,
    /// The environment the session works in.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// The session's folders, the working directory first.
    #[serde(default)]
    pub folders: Vec<String>,
    /// The worktrees it was started on.
    #[serde(default)]
    pub worktrees: Vec<SessionWorktree>,
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
    /// Absolute folder paths, the working directory first. For a remote runtime they are paths
    /// inside it.
    pub folders: Vec<String>,
    /// Pinned environments are listed first.
    #[serde(default)]
    pub pinned: bool,
    /// Where the environment's file and command tools run.
    #[serde(default)]
    pub runtime: EnvironmentRuntime,
    /// Deprecated (since protocol 0.22, removed in a later version): whether the environment has any
    /// persistent agent. Read-only in effect: derived from `agents`.
    #[serde(default)]
    pub persistent_agent: bool,
    /// The environment's default pet (an installed pet's id) for the nursery; none lets the
    /// nursery decide.
    #[serde(default)]
    pub pet: Option<String>,
    /// Deprecated (since protocol 0.22, removed in a later version): the first agent's session id.
    #[serde(default)]
    pub agent_session_id: Option<String>,
    /// The environment's persistent agents, none unless created (`environments.agent_create`).
    #[serde(default)]
    pub agents: Vec<crate::agents::EnvironmentAgent>,
    /// Paths outside the folders the tools may use, and allow / ask / deny rules, for this
    /// environment's sessions (on top of the global ones).
    #[serde(default)]
    pub permissions: crate::permissions::PermissionSet,
    /// Whether sessions also get the git worktrees of the repositories its local folders are
    /// in (the ones `git worktree list` shows, including ones added while a session runs), as
    /// writable as the folders. On unless turned off. Sessions started on their own worktree
    /// (`session.create` `worktree`) and remote runtimes do not get them.
    #[serde(default = "yes")]
    pub project_worktrees: bool,
    /// This environment's own folder for its sessions' git worktrees, as
    /// `<folder>/<repo>/<branch>`. None: the global `sessions.worktree_dir` applies.
    #[serde(default)]
    pub worktree_dir: Option<String>,
    /// The built-in Home environment: always present, listed first, never deleted, and the
    /// default for a session started with no environment or project chosen. It has no project
    /// folders unless the user adds some; its sessions work in their scratch folders.
    #[serde(default)]
    pub builtin: bool,
    /// Which plugins this environment turns on or off, and its plugin suggestions' state.
    #[serde(default)]
    pub plugins: EnvironmentPlugins,
}

/// An environment's plugin lists, by plugin name. `enabled` turns on a plugin the user turned
/// off for everyone (never one a trusted repository's file disables); `disabled` turns one off
/// here. A plugin the load-time gate withholds stays withheld.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentPlugins {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Suggested plugins the user dismissed (stage 7).
    #[serde(default)]
    pub dismissed_suggestions: Vec<String>,
    /// Folders whose suggestions are muted (stage 7).
    #[serde(default)]
    pub muted_suggestion_sources: Vec<String>,
}

/// Where an environment's tools run. `local` is this machine (under the configured isolation
/// tier); the others run them inside a Docker container, a WSL distribution, or over SSH, and the
/// environment's folders are paths there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum EnvironmentRuntime {
    #[default]
    Local,
    /// With `container`, commands run in that running container (`docker exec`) and folders are
    /// paths inside it. Without, each command runs in a fresh container from `image` with the
    /// environment's (local) folders mounted, as the container isolation tier does.
    Docker {
        image: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        container: Option<String>,
    },
    /// A WSL distribution (Windows only); Windows folder paths become `/mnt/<drive>/…`.
    Wsl { distro: String },
    /// A host reached with the system `ssh` client: an alias from the SSH config, or
    /// `user@host[:port]`. Authentication uses the user's keys and agent.
    Ssh { host: String },
}

impl EnvironmentRuntime {
    /// Whether the environment's folders are paths inside the runtime rather than on this
    /// machine.
    pub fn is_remote(&self) -> bool {
        match self {
            EnvironmentRuntime::Local => false,
            EnvironmentRuntime::Docker { container, .. } => container.is_some(),
            EnvironmentRuntime::Wsl { .. } | EnvironmentRuntime::Ssh { .. } => true,
        }
    }
}
