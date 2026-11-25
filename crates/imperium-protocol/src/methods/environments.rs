use super::*;

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
    /// Ids of the local environments with a folder inside a git repository: those a session can
    /// start on worktrees (`session.create` `worktree`).
    #[serde(default)]
    pub git: Vec<String>,
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
    /// Existing folders to start with (may be empty); paths inside the runtime when it is
    /// remote.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Where tools run; `local` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime: Option<EnvironmentRuntime>,
    /// Deprecated (since protocol 0.22, removed in a later version): `true` adds one agent named
    /// "Persistent agent". Use `environments.agent_create`. Nothing is created when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub persistent_agent: Option<bool>,
    /// The environment's default pet, an installed pet's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pet: Option<String>,
    /// Path grants and permission rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permissions: Option<crate::permissions::PermissionSet>,
    /// Whether sessions get the project's git worktrees (on when left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_worktrees: Option<bool>,
    /// Where this environment's sessions put their git worktrees, as `<folder>/<repo>/<branch>`
    /// (an absolute path on this machine). Left out or empty: the global `sessions.worktree_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub worktree_dir: Option<String>,
}

/// `environments.update`: rename an environment, change its icon, its runtime, or turn its
/// persistent agent on or off. Fields left out keep their value; an empty `icon`, `color` or
/// `pet` clears it.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pinned: Option<bool>,
    /// A new runtime. Folders are kept as they are (Windows paths are translated for WSL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime: Option<EnvironmentRuntime>,
    /// Deprecated (since protocol 0.22, removed in a later version): `true` adds an agent named
    /// "Persistent agent" when the environment has none; `false` deletes all its agents. Use
    /// `environments.agent_create` and `environments.agent_delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub persistent_agent: Option<bool>,
    /// The default pet; an empty id clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pet: Option<String>,
    /// Replaces the path grants and permission rules. Rules apply to running sessions from
    /// their next call; grants to sessions started afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permissions: Option<crate::permissions::PermissionSet>,
    /// Turns the project's git worktrees on or off for sessions started afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_worktrees: Option<bool>,
    /// Sets the folder this environment's sessions put their git worktrees in, for sessions
    /// started afterwards; an empty path clears it, so the global `sessions.worktree_dir`
    /// applies again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub worktree_dir: Option<String>,
}

/// `environments.test_runtime`: checks that an environment's runtime can be reached: Docker is
/// running and has the image (or the container is running), the WSL distribution exists, or
/// SSH connects without asking for anything.
#[derive(Debug)]
pub struct EnvironmentTestRuntime;

impl Method for EnvironmentTestRuntime {
    const NAME: &'static str = "environments.test_runtime";
    type Params = EnvironmentRef;
    type Result = RuntimeTestResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeTestResult {
    pub ok: bool,
    /// What was found, or what to fix, in a sentence.
    pub message: String,
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

/// `environments.config`: what the environment's own `.imperium/config.toml` (and
/// `config.local.toml`) in its first folder changes for the sessions started there, and what was
/// refused. The files can only narrow the user's settings.
#[derive(Debug)]
pub struct EnvironmentConfigGet;

impl Method for EnvironmentConfigGet {
    const NAME: &'static str = "environments.config";
    type Params = EnvironmentRef;
    type Result = EnvironmentConfigResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentConfigResult {
    /// The folder whose `.imperium` folder holds the files: the environment's first folder.
    /// None when it has no folder on this machine.
    pub folder: Option<String>,
    /// The files that were read.
    pub files: Vec<String>,
    /// Whether the folder is in the user's `mcp.trusted_projects`, so the servers and plugins
    /// the files name may start.
    pub trusted: bool,
    /// What the files changed, one line each.
    pub applied: Vec<String>,
    /// What was ignored or refused, and why, one line each.
    pub refused: Vec<String>,
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
    /// Go ahead although a budget in the session's scope is used up and says to ask first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confirm_budget: Option<bool>,
    /// Files staged with `session.attach`. The daemon checks each again (it must still be in
    /// the session's attachments folder or inside its folders and grants) and sets `vision`
    /// from the session's model. A message with attachments may have empty text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<crate::session::Attachment>>", optional)]
    pub attachments: Vec<crate::session::Attachment>,
}
