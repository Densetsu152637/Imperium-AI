//! Environments' working folders: the `workspaces.*` methods (see
//! docs/14-projects-and-automation.md#environment-working-folders-implemented). One folder holds a folder
//! per environment with its `Memory.md` and a scratch folder for each session.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::methods::{Accepted, Method};

/// Notification sent to every client while `workspaces.move` (or a changed
/// `workspaces.root`) moves the working folders; the payload is a [`WorkspacesProgress`].
pub const WORKSPACES_PROGRESS: &str = "workspaces.progress";

/// `workspaces.where`: where the working folders are.
#[derive(Debug)]
pub struct WorkspacesWhere;

impl Method for WorkspacesWhere {
    const NAME: &'static str = "workspaces.where";
    type Params = WorkspacesWhereParams;
    type Result = WorkspacesWhereResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspacesWhereParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspacesWhereResult {
    /// The folder in use (`workspaces.root`, else the default).
    pub root: String,
    /// The folder used when `workspaces.root` is unset: `<data>/workspaces`.
    pub default_root: String,
    /// The environments' folders, in the environments' order.
    pub environments: Vec<EnvironmentWorkspace>,
    /// Why the folder in use is not the one the configuration names, such as a move that failed
    /// at start; the folders stay where they were until it is fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub warning: Option<String>,
}

/// One environment's working folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EnvironmentWorkspace {
    pub environment_id: String,
    /// `<root>/<environment-id>`; not made until something needs it.
    pub dir: String,
    /// `<dir>/Memory.md`.
    pub memory_file: String,
    /// Whether `Memory.md` exists yet.
    pub memory_exists: bool,
    /// How many session scratch folders exist now.
    pub session_folders: u32,
}

/// `workspaces.move`: moves the working folders to `path` and makes it `workspaces.root`
/// (the same as `config.set` of that key). Refused when `path` is not empty, is inside the
/// current folder, or a session is working; nothing is lost when it fails.
#[derive(Debug)]
pub struct WorkspacesMove;

impl Method for WorkspacesMove {
    const NAME: &'static str = "workspaces.move";
    type Params = WorkspacesMoveParams;
    type Result = WorkspacesMoveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspacesMoveParams {
    /// An absolute folder. Empty goes back to the default, `<data>/workspaces`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspacesMoveResult {
    pub from: String,
    pub to: String,
    /// `nothing` (there was nothing to move, or the same folder), `rename`, or `copy`.
    #[ts(type = "\"nothing\" | \"rename\" | \"copy\"")]
    pub method: String,
    pub files: u64,
    pub bytes: u64,
    /// Problems after the data was safely in place, such as an old folder that could not be
    /// deleted completely.
    pub warnings: Vec<String>,
    /// Idle sessions that were restarted so their scratch folder points at the new place.
    pub restarted_sessions: Vec<String>,
}

/// The progress of a move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspacesProgress {
    #[ts(type = "\"scanning\" | \"copying\" | \"verifying\" | \"cleaning\"")]
    pub phase: String,
    pub files_done: u64,
    pub files_total: u64,
}

/// `workspaces.open`: opens an environment's working folder or `Memory.md` in the computer's
/// own file manager or editor (on the machine the daemon runs on). `Memory.md` is created, empty,
/// when it does not exist yet.
#[derive(Debug)]
pub struct WorkspacesOpen;

impl Method for WorkspacesOpen {
    const NAME: &'static str = "workspaces.open";
    type Params = WorkspacesOpenParams;
    type Result = Accepted;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspacesOpenParams {
    pub environment_id: String,
    #[ts(type = "\"folder\" | \"memory\"")]
    pub target: String,
}
