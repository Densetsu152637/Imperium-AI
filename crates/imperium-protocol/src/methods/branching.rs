use super::*;

// ---- Branching and comparison ----

/// `session.branch`: starts a new session whose conversation is a copy of the source's up to
/// an event (a message, a reply, or any event of the turn): the turn containing the event is kept
/// whole, unless the event is the user message that started it, in which case that turn is left
/// out and its text comes back as `prompt` to edit and send again. The root conversation is
/// copied; subagents are not carried past that point, and attachments are referenced, not
/// copied. The source is left as it is, and the branch records `branched_from`. Folders,
/// environment, and settings are the source's. With `worktree`, the branch gets a git worktree of
/// its own holding the files as they were at that point (from the source's checkpoint, else its
/// files now), so the source's working tree is never touched. The source may be running or
/// archived; the point's turn must have ended.
#[derive(Debug)]
pub struct SessionBranch;

impl Method for SessionBranch {
    const NAME: &'static str = "session.branch";
    type Params = SessionBranchParams;
    type Result = SessionBranchResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionBranchParams {
    pub session_id: String,
    /// The index of an event in the source's `session.subscribe` history; the session's current
    /// point when absent.
    #[serde(default)]
    #[ts(optional)]
    pub at: Option<u32>,
    /// The branch's title; by default the source's with " (branch)".
    #[serde(default)]
    #[ts(optional)]
    pub title: Option<String>,
    /// Give the branch its own git worktree, with the files as they were at that point.
    #[serde(default)]
    pub worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionBranchResult {
    /// The new session.
    pub session_id: String,
    /// When the branch point was a user message: its text, left out of the branch.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Whether the branch has a worktree of its own.
    #[serde(default)]
    pub worktree: bool,
}

/// `session.compare`: best-of-N. Branches the session at its current point once per model
/// (two to four), sends each the same message, and runs them in parallel (local models still
/// load through the memory residency controller). Each branch is an ordinary session with
/// `branched_from` and a shared `comparison_id`. Branches get a git worktree each when the
/// session's folders are in a git repository on this machine (`worktree` can force it on or
/// off), so they do not edit the same files. Refused while a turn is running.
#[derive(Debug)]
pub struct SessionCompare;

impl Method for SessionCompare {
    const NAME: &'static str = "session.compare";
    type Params = SessionCompareParams;
    type Result = SessionCompareResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCompareParams {
    pub session_id: String,
    /// Sent to every branch.
    pub message: String,
    /// Two to four model specs (model table ids or `<endpoint>:<model>`).
    pub models: Vec<String>,
    /// Whether each branch gets its own worktree; by default whenever the folders allow it.
    #[serde(default)]
    #[ts(optional)]
    pub worktree: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCompareResult {
    pub comparison_id: String,
    pub branches: Vec<CompareBranch>,
}

/// One branch of a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CompareBranch {
    pub session_id: String,
    pub model: String,
    /// Whether it has a git worktree of its own.
    #[serde(default)]
    pub worktree: bool,
}

/// `session.compare_pick`: keeps one branch of a comparison and archives the others (which
/// stops them; they stay in the archive and can be unarchived).
#[derive(Debug)]
pub struct SessionComparePick;

impl Method for SessionComparePick {
    const NAME: &'static str = "session.compare_pick";
    type Params = SessionComparePickParams;
    type Result = SessionComparePickResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionComparePickParams {
    pub comparison_id: String,
    /// The branch to keep.
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionComparePickResult {
    pub kept: String,
    /// The branches archived.
    pub archived: Vec<String>,
}
