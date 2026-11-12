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

/// `session.agents`: a session's agents (the root and every subagent) with their kept state.
#[derive(Debug)]
pub struct SessionAgents;

impl Method for SessionAgents {
    const NAME: &'static str = "session.agents";
    type Params = SessionAgentsParams;
    type Result = SessionAgentsResult;
}

/// `session.agent_cancel`: ends a paused (or otherwise finished) subagent without resuming it.
/// It emits `task_finished` (not ok, reported as cancelled) and is no longer listed as paused;
/// its conversation is kept, so it can still be resumed later. Fails while it is running.
#[derive(Debug)]
pub struct SessionAgentCancel;

impl Method for SessionAgentCancel {
    const NAME: &'static str = "session.agent_cancel";
    type Params = SessionAgentCancelParams;
    type Result = SessionAgentCancelResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentCancelParams {
    pub session_id: String,
    /// A subagent's task id, such as `t1`.
    pub agent_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentCancelResult {}

/// `session.agent_resume`: gives a finished (idle, failed, interrupted, or paused) subagent a
/// follow-up. It works again with its earlier conversation, emitting `task_resumed` and then
/// `task_finished`; while it runs, `session.send` with its `agent_id` steers or queues for it.
#[derive(Debug)]
pub struct SessionAgentResume;

impl Method for SessionAgentResume {
    const NAME: &'static str = "session.agent_resume";
    type Params = SessionAgentResumeParams;
    type Result = SessionAgentResumeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentsParams {
    pub session_id: String,
}

/// The root first, then subagents in the order they were started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentsResult {
    pub agents: Vec<crate::session::AgentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentResumeParams {
    pub session_id: String,
    /// A subagent's task id, such as `t1`.
    pub agent_id: String,
    /// The follow-up message.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentResumeResult {
    pub agent_id: String,
    /// The model it runs on.
    pub model: String,
}

/// `session.respond`: answers a `permission_requested` event.
#[derive(Debug)]
pub struct SessionRespond;

impl Method for SessionRespond {
    const NAME: &'static str = "session.respond";
    type Params = SessionRespondParams;
    type Result = Accepted;
}

/// `session.answer`: answers a `questions_asked` event, one answer per question in order.
#[derive(Debug)]
pub struct SessionAnswer;

impl Method for SessionAnswer {
    const NAME: &'static str = "session.answer";
    type Params = SessionAnswerParams;
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
    /// The root agent's model spec; `None` while the session waits for one to be chosen.
    pub model: Option<String>,
    /// The session's reasoning effort (`session.control` `set_effort`), kept across restarts.
    /// Sent only to models that can reason; absent from daemons that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "\"low\" | \"medium\" | \"high\" | \"max\"")]
    pub effort: Option<String>,
    /// How the session's calls are approved beside the permission rules (`session.control`
    /// `set_permission_mode`), kept across restarts; absent from daemons that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        optional,
        type = "\"ask\" | \"default\" | \"accept_edits\" | \"plan\" | \"full\""
    )]
    pub permission_mode: Option<String>,
    /// Whether the session's model can reason, so its effort applies (clients show the effort
    /// control only then).
    #[serde(default)]
    pub reasoning: bool,
    /// Where the session's commands run, such as `tier 0 · folder policy`.
    pub isolation: String,
    /// The session's title: one the user gave it (`session.control` `rename`), else one
    /// generated after its first exchange, else its first message.
    pub title: String,
    pub status: crate::session::SessionStatus,
    /// What it watches, when watching or merged.
    pub status_detail: Option<String>,
    pub resume_policy: crate::session::ResumePolicy,
    /// Started by a schedule or a project agent: it gives way to the user's own sessions.
    #[serde(default)]
    pub background: bool,
    /// Why background work is waiting (deferred before it started, or paused at a step
    /// boundary), until the machine is quiet or the user runs it now.
    #[serde(default)]
    pub hold: Option<SessionHold>,
    /// True for an environment's persistent agent session, which is always running, resumes
    /// after restarts, and is listed first among its environment's sessions.
    #[serde(default)]
    pub persistent: bool,
    /// Git worktrees the session works in instead of its environment's folders.
    #[serde(default)]
    pub worktrees: Vec<SessionWorktree>,
    /// Pinned sessions are listed first; set with `session.control` `set_pinned`.
    #[serde(default)]
    pub pinned: bool,
    /// Archived: the session no longer runs or resumes, but its journal is kept and
    /// `session.subscribe` still returns its history. `session.control` `unarchive` resumes it.
    #[serde(default)]
    pub archived: bool,
    /// When it was archived, in milliseconds since the Unix epoch.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub archived_at: Option<u64>,
    /// When the session was created, in milliseconds since the Unix epoch; none when unknown.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub created_at: Option<u64>,
    /// The session this one was branched from (`session.branch`, `session.compare`), and the
    /// event it was branched at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branched_from: Option<BranchOrigin>,
    /// Set on the branches of one `session.compare`: the same id on each.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub comparison_id: Option<String>,
    /// The session's folders are a read-only mount (`session.create` `read_only`).
    #[serde(default)]
    pub read_only: bool,
}

/// Where a branched session came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BranchOrigin {
    pub session_id: String,
    /// The index, in the source's `session.subscribe` history, of the event it was branched at.
    pub at: u32,
}

/// A background session that is waiting for a quiet machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionHold {
    pub cause: crate::session::PauseCause,
    pub reason: String,
}

/// A git worktree a session was started on: its tools use `path` instead of `folder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionWorktree {
    /// The environment folder it stands in for.
    pub folder: String,
    /// The worktree's folder.
    pub path: String,
    /// The new branch it is on, such as `imperium/3fa2c1d0`.
    pub branch: String,
    /// The repository it belongs to.
    pub repo: String,
}

/// `session.delete`: stops a session and forgets it, so it does not continue after a restart.
/// With `remove_worktrees`, its git worktrees are removed too (their branches are kept). An
/// environment's persistent agent is turned off with `environments.update` instead.
#[derive(Debug)]
pub struct SessionDelete;

impl Method for SessionDelete {
    const NAME: &'static str = "session.delete";
    type Params = SessionDeleteParams;
    type Result = SessionDeleteResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionDeleteParams {
    pub session_id: String,
    #[serde(default)]
    pub remove_worktrees: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionDeleteResult {
    /// Worktree folders removed.
    pub removed_worktrees: Vec<String>,
    /// Worktrees that could not be removed, with why.
    pub failed: Vec<String>,
}

/// Notification sent to every client when a session is deleted; the payload is
/// [`SessionDeleteParams`] (without `remove_worktrees`' effect).
pub const SESSION_DELETED: &str = "session.deleted";

// ---- Checkpoints and rewinding ----

/// A checkpoint of a session's files: one commit per git repository the session's folders are
/// in, on a private ref (`refs/imperium/checkpoints/<session>/<number>`), taken without touching
/// the repository's index, HEAD, or branches. It holds every file git would see (tracked and
/// untracked), not ignored ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpoint {
    /// 1 for the session's first checkpoint, counting up.
    pub number: u32,
    /// When it was taken, in Unix milliseconds.
    #[ts(type = "number")]
    pub at_unix_ms: u64,
    /// `turn`: the files before a root turn that changed them. `rewind`: the files just before
    /// a rewind, so the rewind can be undone by rewinding to it.
    #[ts(type = "\"turn\" | \"rewind\"")]
    pub kind: String,
    /// For `turn`: the message the turn started with.
    #[serde(default)]
    pub prompt: String,
    /// One commit per repository.
    pub repos: Vec<CheckpointRepo>,
    /// The conversation as it was can be rewound to (`turn` checkpoints).
    #[serde(default)]
    pub conversation: bool,
}

/// One repository's commit in a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointRepo {
    /// The top folder of the repository's working tree (a linked worktree's own folder).
    pub root: String,
    /// The checkpoint commit (its parent is HEAD as it was then).
    pub commit: String,
    /// The private ref that keeps it.
    pub reference: String,
}

/// Notification sent to every client when a session records a checkpoint; the payload is
/// [`SessionCheckpointParams`].
pub const SESSION_CHECKPOINT: &str = "session.checkpoint";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpointParams {
    pub session_id: String,
    pub checkpoint: SessionCheckpoint,
}

/// `session.checkpoints`: a session's checkpoints, oldest first.
#[derive(Debug)]
pub struct SessionCheckpoints;

impl Method for SessionCheckpoints {
    const NAME: &'static str = "session.checkpoints";
    type Params = SessionCheckpointsParams;
    type Result = SessionCheckpointsResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpointsParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpointsResult {
    /// Whether the session records checkpoints: its folders are on this machine, at least one
    /// is in a git repository, and `sessions.checkpoints` is on.
    pub available: bool,
    /// Why not, when `available` is false.
    #[serde(default)]
    pub reason: Option<String>,
    pub checkpoints: Vec<SessionCheckpoint>,
}

/// What `session.rewind` puts back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RewindMode {
    /// The files and the conversation.
    #[default]
    Both,
    /// Only the files; the conversation goes on as it is.
    Files,
    /// Only the conversation; the files stay as they are.
    Conversation,
}

/// `session.rewind`: puts a session back to a checkpoint. Files: every file git would see in
/// the session's repositories is restored to the checkpoint (changed and deleted files written
/// back, files added since removed); ignored files, the index, HEAD, and branches are left
/// alone, and the files as they were are first recorded as a `rewind` checkpoint, so the rewind
/// can be undone. Conversation: the session is forked at the checkpoint into a new session
/// whose conversation ends just before that turn's message (returned, to edit and send again);
/// the original session is kept as it is. Refused while a turn is running.
#[derive(Debug)]
pub struct SessionRewind;

impl Method for SessionRewind {
    const NAME: &'static str = "session.rewind";
    type Params = SessionRewindParams;
    type Result = SessionRewindResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionRewindParams {
    pub session_id: String,
    /// The checkpoint's number.
    pub checkpoint: u32,
    #[serde(default)]
    pub mode: RewindMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionRewindResult {
    /// Files written back (changed or deleted since the checkpoint), absolute.
    pub restored: Vec<String>,
    /// Files removed (added since the checkpoint), absolute.
    pub removed: Vec<String>,
    /// The `rewind` checkpoint holding the files as they were before, when files were rewound.
    #[serde(default)]
    pub undo_checkpoint: Option<u32>,
    /// The new session holding the conversation up to the checkpoint, when the conversation
    /// was rewound.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The message the rewound turn started with, to edit and send again.
    #[serde(default)]
    pub prompt: Option<String>,
}

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

// ---- Reviewing a session's changes ----

/// One hunk of a file's unified diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReviewHunk {
    /// Names this hunk as it is now, for `session.review_hunk`; a hunk that changed since it was
    /// listed no longer matches.
    pub id: String,
    /// `@@ -old_start,old_lines +new_start,new_lines @@`.
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Each line with its sign first: ` ` (context), `-` (removed), or `+` (added), without its
    /// line ending.
    pub lines: Vec<String>,
}

/// A file the session changed, against the review's base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReviewFile {
    /// Names the file as it is now (both sides), for a whole-file `session.review_hunk`
    /// (`hunk: "file:<id>"`); a file that changed since it was listed no longer matches.
    pub id: String,
    /// The repository's top folder.
    pub repo: String,
    /// Relative to `repo`, with `/`.
    pub path: String,
    #[ts(type = "\"added\" | \"modified\" | \"deleted\"")]
    pub status: String,
    /// Binary (or over 2 MiB): no hunks; accepted or rejected whole.
    pub binary: bool,
    pub hunks: Vec<ReviewHunk>,
}

/// `session.review`: the files a session changed since one of its checkpoints (its first, the
/// session's start, by default), as unified diff hunks. Hunks already accepted are not shown.
#[derive(Debug)]
pub struct SessionReview;

impl Method for SessionReview {
    const NAME: &'static str = "session.review";
    type Params = SessionReviewParams;
    type Result = SessionReviewResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewParams {
    pub session_id: String,
    /// The checkpoint to compare against; the session's first when left out.
    #[serde(default)]
    pub since: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewResult {
    /// Whether the session has checkpoints to review against (see `session.checkpoints`).
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// The checkpoint compared against; none before the session's first.
    #[serde(default)]
    pub since: Option<u32>,
    /// The session's checkpoints, oldest first, to pick another.
    pub checkpoints: Vec<SessionCheckpoint>,
    pub files: Vec<ReviewFile>,
    /// A commit message for the changes: the session's title and the files.
    pub suggested_message: String,
    /// The session's repositories (top folders), for commits and pull requests.
    pub repos: Vec<String>,
    /// Whether `gh` was found, so a pull request can be opened from the app.
    pub gh: bool,
}

/// What `session.review_hunk` does with a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewAction {
    /// Keep it: it joins the review's base and is no longer shown.
    Accept,
    /// Revert it in the working tree.
    Reject,
}

/// `session.review_hunk`: accepts or rejects one hunk, or a whole file with `hunk: "file:<id>"`.
/// Refused while a turn is running.
#[derive(Debug)]
pub struct SessionReviewHunk;

impl Method for SessionReviewHunk {
    const NAME: &'static str = "session.review_hunk";
    type Params = SessionReviewHunkParams;
    type Result = SessionReviewHunkResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewHunkParams {
    pub session_id: String,
    /// The review's checkpoint, as `session.review` returned it.
    #[serde(default)]
    pub since: Option<u32>,
    pub repo: String,
    pub path: String,
    /// A hunk's `id`, or `file:<id>` with the file's `id` for the whole file.
    pub hunk: String,
    pub action: ReviewAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewHunkResult {
    /// The file as it is now; none when it has nothing left to review.
    #[serde(default)]
    pub file: Option<ReviewFile>,
}

/// `session.review_commit`: commits the files the session changed since the review's
/// checkpoint in one repository, as they now are, on its current branch (hooks off). Gated by
/// the `git_commit` permission rule: denied, it fails; without an allow rule it returns
/// `needs_confirmation` until called again with `confirmed`. The committed changes are then
/// accepted.
#[derive(Debug)]
pub struct SessionReviewCommit;

impl Method for SessionReviewCommit {
    const NAME: &'static str = "session.review_commit";
    type Params = SessionReviewCommitParams;
    type Result = SessionReviewCommitResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewCommitParams {
    pub session_id: String,
    #[serde(default)]
    pub since: Option<u32>,
    pub repo: String,
    pub message: String,
    /// The user confirmed it (asked by default).
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewCommitResult {
    pub committed: bool,
    /// Nothing was done: ask the user, then call again with `confirmed`.
    pub needs_confirmation: bool,
    /// The new commit.
    #[serde(default)]
    pub commit: Option<String>,
    /// The files committed (or that would be).
    pub files: Vec<String>,
}

/// `session.review_pull_request`: pushes the repository's current branch to `origin` and opens
/// a pull request with `gh`. Without `gh` it opens nothing and returns the commands to run.
/// Gated like `session.review_commit`, by the `open_pull_request` permission rule.
#[derive(Debug)]
pub struct SessionReviewPullRequest;

impl Method for SessionReviewPullRequest {
    const NAME: &'static str = "session.review_pull_request";
    type Params = SessionReviewPullRequestParams;
    type Result = SessionReviewPullRequestResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewPullRequestParams {
    pub session_id: String,
    pub repo: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// The branch to merge into; the repository's default when left out.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewPullRequestResult {
    pub opened: bool,
    pub needs_confirmation: bool,
    /// The pull request, when `gh` printed it.
    #[serde(default)]
    pub url: Option<String>,
    /// The branch pushed.
    pub branch: String,
    /// The commands that do it, to run by hand when `gh` is not installed.
    pub command: String,
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
    /// Deprecated (since protocol 0.22, removed in 0.23): whether the environment has any
    /// persistent agent. Read-only in effect: derived from `agents`.
    #[serde(default)]
    pub persistent_agent: bool,
    /// The environment's default pet (an installed pet's id) for the nursery; none lets the
    /// nursery decide.
    #[serde(default)]
    pub pet: Option<String>,
    /// Deprecated (since protocol 0.22, removed in 0.23): the first agent's session id.
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
    /// The built-in Home environment: always present, listed first, never deleted, and the
    /// default for a session started with no environment or project chosen. It has no project
    /// folders unless the user adds some; its sessions work in their scratch folders.
    #[serde(default)]
    pub builtin: bool,
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
    /// Deprecated (since protocol 0.22, removed in 0.23): `true` adds one agent named
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
    /// Deprecated (since protocol 0.22, removed in 0.23): `true` adds an agent named
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

/// `session.attach`: stages a file for the next message. Give `path` (a file on the daemon's
/// machine) or `data` (the file's bytes, base64). A file inside the session's folders or path
/// grants is referenced where it is; anything else is copied into the session's attachments
/// folder under the data directory, which the session's agents may read. Files over
/// `sessions.attachment_max_mib` are refused.
#[derive(Debug)]
pub struct SessionAttach;

impl Method for SessionAttach {
    const NAME: &'static str = "session.attach";
    type Params = SessionAttachParams;
    type Result = SessionAttachResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAttachParams {
    pub session_id: String,
    /// The file name to show; for `path`, its last component when empty.
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// The file's bytes, base64 (standard alphabet, padded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAttachResult {
    pub attachment: crate::session::Attachment,
    /// Whether the session's model can view images now; without vision an image is sent as a
    /// path reference.
    pub vision: bool,
    /// For an image of at most 4 MiB given by `path`: its bytes as a `data:` URL, so the client
    /// can draw a thumbnail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_data: Option<String>,
}

/// `voice.transcribe`: turns recorded speech into text through `[media.transcription]` (an
/// OpenAI-compatible `/audio/transcriptions` endpoint). The text is returned, not sent.
#[derive(Debug)]
pub struct VoiceTranscribe;

impl Method for VoiceTranscribe {
    const NAME: &'static str = "voice.transcribe";
    type Params = VoiceTranscribeParams;
    type Result = VoiceTranscribeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VoiceTranscribeParams {
    /// The recording, base64.
    pub audio: String,
    /// Its media type, such as `audio/webm` or `audio/wav`.
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VoiceTranscribeResult {
    pub text: String,
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
    /// `list`, `edit` (with `id` and `text`), `remove` (with `id`), or `steer` (with `id`).
    /// `steer` turns a queued message into steering for the running turn, in place: it leaves
    /// the queue when the agent reads it at its next safe point (or runs as the next turn,
    /// ahead of the other queued messages, when the turn ended first). Editing or removing a
    /// message that was already delivered fails, and editing one that is steering fails.
    #[ts(type = "\"list\" | \"edit\" | \"remove\" | \"steer\"")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAnswerParams {
    pub session_id: String,
    #[ts(type = "number")]
    pub request_id: u64,
    pub answers: Vec<String>,
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
    /// `llamacpp` for GGUF files, `vllm` for Hugging Face folders.
    #[ts(type = "\"llamacpp\" | \"vllm\"")]
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
}

/// A local model's llama.cpp settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelConfig {
    /// The id in `models.local`.
    pub id: String,
    #[ts(type = "\"llamacpp\" | \"vllm\"")]
    pub runtime: String,
    /// What the app's settings file sets; null fields take the effective value from elsewhere.
    pub saved: LocalLaunchSettings,
    /// What the model is started with.
    pub effective: LocalLaunchEffective,
    /// Memory the model needs with the effective settings.
    #[ts(type = "number")]
    pub required_bytes: u64,
    /// The part of `required_bytes` that is the KV cache pool.
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
    /// Where it was installed from, when an agent installed it (`install_extension`).
    #[serde(default)]
    pub installed_from: Option<InstallSource>,
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
    /// The environment the schedule runs in (`action.environment`); none for a schedule that
    /// works in a bare `project` folder.
    #[serde(default)]
    pub environment: Option<String>,
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

/// `api.get`: the OpenAI-compatible local API server's state and the models it serves.
#[derive(Debug)]
pub struct ApiGet;

impl Method for ApiGet {
    const NAME: &'static str = "api.get";
    type Params = ApiGetParams;
    type Result = ApiStatus;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiGetParams {}

/// `api.set`: changes the API server's settings (saved to the user config) and applies them at
/// once: enabling starts the server, disabling stops it. The first enable picks and saves a
/// free port. A `bind` is refused unless it is loopback or `allow_lan` is set.
#[derive(Debug)]
pub struct ApiSet;

impl Method for ApiSet {
    const NAME: &'static str = "api.set";
    type Params = ApiSetParams;
    type Result = ApiStatus;
}

/// What to change; absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiSetParams {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 0 to have a free port chosen.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub bind: Option<String>,
    #[serde(default)]
    pub allow_lan: Option<bool>,
    /// Specs of the hosted models to serve (replaces the list).
    #[serde(default)]
    pub expose: Option<Vec<String>>,
}

/// `api.token`: the API token, made on first use. With `rotate` a new one replaces it, and the
/// old one stops working at once. The token is kept in the secret store, never in the config.
#[derive(Debug)]
pub struct ApiToken;

impl Method for ApiToken {
    const NAME: &'static str = "api.token";
    type Params = ApiTokenParams;
    type Result = ApiTokenResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiTokenParams {
    #[serde(default)]
    pub rotate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiTokenResult {
    pub token: String,
}

/// The API server as `api.get` and `api.set` report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiStatus {
    pub enabled: bool,
    /// Whether the server is listening now.
    pub running: bool,
    pub bind: String,
    /// 0 until a port is chosen.
    pub port: u16,
    pub allow_lan: bool,
    /// Where other tools point at (`http://127.0.0.1:<port>/v1`), while the server runs.
    pub base_url: Option<String>,
    /// The hosted models' specs the user chose to serve.
    pub expose: Vec<String>,
    /// Why the server is not running although it is enabled.
    pub error: Option<String>,
    /// Every model the server serves or could serve with an opt-in.
    pub models: Vec<ApiModel>,
}

/// A model the API server serves, or could.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiModel {
    /// The id other tools use: the model's spec.
    pub id: String,
    /// Its alias, else its name.
    pub name: String,
    /// A hosted model spends the user's keys and money; it is served only when listed in
    /// `expose`.
    pub hosted: bool,
    /// Whether the server serves it now.
    pub exposed: bool,
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

/// `session.browser_control`: the user takes over the session's browser (`user` true) or gives
/// it back to the agent (`user` false). While the user drives, the agent's browser tools are
/// refused with a message saying so, and `browser_control` events tell every client. Taking
/// over starts the browser if it is not running, and returns its page.
#[derive(Debug)]
pub struct SessionBrowserControl;

impl Method for SessionBrowserControl {
    const NAME: &'static str = "session.browser_control";
    type Params = BrowserControlParams;
    type Result = BrowserControlResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserControlParams {
    pub session_id: String,
    pub user: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserControlResult {
    /// The browser's page after taking over; absent when giving back.
    #[serde(default)]
    pub view: Option<BrowserPage>,
}

/// The browser's page: URL, title, and a JPEG screenshot (base64).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserPage {
    pub url: String,
    pub title: String,
    pub screenshot: String,
}

/// `session.browser_act`: an action of the user in the browser they have taken over
/// (`session.browser_control`); refused when the user is not driving. Returns the page after
/// the action, which is also sent as a `browser_view` event.
#[derive(Debug)]
pub struct SessionBrowserAct;

impl Method for SessionBrowserAct {
    const NAME: &'static str = "session.browser_act";
    type Params = BrowserActParams;
    type Result = BrowserActResult;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserActParams {
    pub session_id: String,
    pub action: BrowserAction,
}

/// What the user does in the browser. `click` positions are fractions (0 to 1) of the page's
/// visible width and height, so a client scales a click on the screenshot without knowing the
/// page's size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum BrowserAction {
    /// Go to an http or https URL.
    Navigate {
        url: String,
    },
    Back,
    Forward,
    Reload,
    Click {
        x: f64,
        y: f64,
    },
    /// Type text into the focused element.
    Type {
        text: String,
    },
    /// Press a named key such as `Enter`, `Tab`, `Backspace`, or `Escape`.
    Key {
        key: String,
    },
    /// Scroll the page by `dy` pixels (positive is down).
    Scroll {
        dy: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BrowserActResult {
    pub view: BrowserPage,
}

/// Notification sent to every client when a session starts, whoever started it (a client, a
/// schedule, a project agent, or auto-fix). The payload is a [`SessionSummary`].
pub const SESSION_CREATED: &str = "session.created";

/// Notification sent to every client when a session's listing changes outside its event
/// stream (such as being pinned). The payload is a [`SessionSummary`].
pub const SESSION_UPDATED: &str = "session.updated";

/// `session.control`: change how a session waits for usage resets, resume it now, set what
/// pull request it watches, switch its model, compact its conversation, pin it, or rename it.
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
    /// for `models.default`; refused while a turn runs), `set_effort` (with `effort`; applies
    /// from every agent's next model call, also while a turn runs; kept across restarts),
    /// `set_permission_mode` (with `permission_mode`: `ask`, `default`, `accept_edits`, `plan`,
    /// or `full`; applies from every agent's next call, also while a turn runs; kept across
    /// restarts), `compact` (summarise older turns
    /// now; refused while a turn runs), `set_pinned` (with `pinned`; kept across restarts),
    /// `rename` (with `title`; kept across restarts and never replaced by a generated title; an
    /// empty title lets the session be titled automatically again), `archive` (stop the session and keep its journal; announced with `session.updated`),
    /// `unarchive` (resume an archived session), `stop` (halt the root and every subagent at
    /// their next safe point; queued messages wait; kept across restarts; see `work_stopped`),
    /// or `resume` (continue a stopped session; not to be confused with `resume_now`, which ends
    /// a usage-limit wait).
    #[ts(
        type = "\"set_resume_policy\" | \"resume_now\" | \"watch\" | \"unwatch\" | \"set_model\" | \"set_effort\" | \"set_permission_mode\" | \"compact\" | \"set_pinned\" | \"rename\" | \"archive\" | \"unarchive\" | \"stop\" | \"resume\""
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
    /// For `set_effort`: how hard models that can reason think.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "\"low\" | \"medium\" | \"high\" | \"max\"")]
    pub effort: Option<String>,
    /// For `set_permission_mode`: the session's new permission mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        optional,
        type = "\"ask\" | \"default\" | \"accept_edits\" | \"plan\" | \"full\""
    )]
    pub permission_mode: Option<String>,
    /// For `set_pinned`: whether the session is pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pinned: Option<bool>,
    /// For `rename`: the new title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionControlResult {
    /// For `set_model` and `compact`: the session's model afterwards.
    #[serde(default)]
    pub model: Option<String>,
    /// For `set_effort`: the session's effort afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "\"low\" | \"medium\" | \"high\" | \"max\"")]
    pub effort: Option<String>,
    /// For `set_permission_mode`: the session's permission mode afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        optional,
        type = "\"ask\" | \"default\" | \"accept_edits\" | \"plan\" | \"full\""
    )]
    pub permission_mode: Option<String>,
    /// For `set_model` and `set_effort`: whether the session's model can reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning: Option<bool>,
    /// For `set_model` and `compact`: the conversation measured for that model's window. For
    /// `set_model` it is an estimate (`method` `estimated`) against the window the model is
    /// expected to have, as the model is not loaded or called until the session's next turn,
    /// which measures it exactly and compacts the conversation then if it does not fit.
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
    #[ts(type = "\"default\" | \"user\" | \"app\" | \"project\" | \"env\"")]
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

/// `personalities.list`: the personalities `policy.personality` may name, for a picker. Local
/// personalities (`<config>/personalities/`) come first in precedence, then workflow packs',
/// then the built-in ones; each id is listed once, as the one that wins.
#[derive(Debug)]
pub struct PersonalitiesList;

impl Method for PersonalitiesList {
    const NAME: &'static str = "personalities.list";
    type Params = PersonalitiesListParams;
    type Result = PersonalitiesListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PersonalitiesListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PersonalitiesListResult {
    /// Sorted by name.
    pub personalities: Vec<PersonalitySummary>,
    /// The id used when `policy.personality` is unset or names one that is not found.
    pub default: String,
}

/// One selectable personality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PersonalitySummary {
    /// The file stem, which `policy.personality` takes.
    pub id: String,
    /// Its display name, from the file's heading.
    pub name: String,
    /// Where the winning file is.
    #[ts(type = "\"local\" | \"pack\" | \"builtin\"")]
    pub source: String,
    /// For `pack`: the pack's folder name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pack: Option<String>,
    /// A local personality that hides a pack or built-in one with the same id.
    pub overrides: bool,
}

// ---- Changes agents make to the app config ----

/// Where an installed mod, plugin, or skill came from, recorded in its folder as
/// `.imperium-source.json` when an agent installs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InstallSource {
    /// `git` (cloned from a URL) or `path` (copied from a folder on this machine).
    #[ts(type = "\"git\" | \"path\"")]
    pub kind: String,
    /// The git URL or the folder, as resolved.
    pub location: String,
    /// The commit installed, for `git`.
    #[serde(default)]
    pub revision: Option<String>,
    /// The branch, tag, or commit asked for, for `git`.
    #[serde(default)]
    pub requested: Option<String>,
    /// The folder inside the source that was installed, when not its top.
    #[serde(default)]
    pub subdir: Option<String>,
    /// When it was installed, in Unix milliseconds.
    #[ts(type = "number")]
    pub installed_at_unix_ms: u64,
    /// The session whose agent installed it.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// One change an agent made to the app config, as the change journal keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigChange {
    #[ts(type = "number")]
    pub id: u64,
    #[ts(type = "number")]
    pub at_unix_ms: u64,
    /// The session whose agent made it.
    #[serde(default)]
    pub session_id: Option<String>,
    /// `model_card` (a card written), `install`, or `uninstall` (a mod, plugin, or skill).
    #[ts(type = "\"model_card\" | \"install\" | \"uninstall\"")]
    pub kind: String,
    /// One line saying what changed.
    pub summary: String,
    /// For `model_card`: the model table row or local model.
    #[serde(default)]
    pub model: Option<String>,
    /// For `model_card`: `table` (`models.overrides."<id>".card`) or `local`
    /// (`runtime.llamacpp.models."<id>".card`), both in the app settings file.
    #[serde(default)]
    pub model_kind: Option<String>,
    /// For `model_card`: the card the app settings file held before, if any.
    #[serde(default)]
    pub before: Option<String>,
    /// For `model_card`: the card written; none when it was cleared.
    #[serde(default)]
    pub after: Option<String>,
    /// For `install` and `uninstall`: `mod`, `plugin`, or `skill`.
    #[serde(default)]
    pub extension_kind: Option<String>,
    /// For `install` and `uninstall`: the extension's name.
    #[serde(default)]
    pub name: Option<String>,
    /// For `install` and `uninstall`: its folder.
    #[serde(default)]
    pub path: Option<String>,
    /// For `install`, and an `uninstall` of something an agent installed: where it came from.
    #[serde(default)]
    pub source: Option<InstallSource>,
    /// For `install` of a plugin with hooks or MCP servers: it was added to
    /// `extensions.disabled_plugins`, so its hooks and servers do not run until the user turns
    /// it on.
    #[serde(default)]
    pub disabled: bool,
    /// For `uninstall`: where the folder was moved, so undo can put it back.
    #[serde(default)]
    pub kept_at: Option<String>,
    /// When it was undone, in Unix milliseconds.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub undone_at_unix_ms: Option<u64>,
}

/// `config.changes`: the changes agents made to the app config, newest first.
#[derive(Debug)]
pub struct ConfigChanges;

impl Method for ConfigChanges {
    const NAME: &'static str = "config.changes";
    type Params = ConfigChangesParams;
    type Result = ConfigChangesResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigChangesParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigChangesResult {
    pub changes: Vec<ConfigChange>,
}

/// `config.undo`: undoes one change from `config.changes`: a card goes back to what it was
/// (refused when it was edited since), an install is removed, and an uninstalled extension is
/// put back.
#[derive(Debug)]
pub struct ConfigUndo;

impl Method for ConfigUndo {
    const NAME: &'static str = "config.undo";
    type Params = ConfigUndoParams;
    type Result = ConfigUndoResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigUndoParams {
    #[ts(type = "number")]
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigUndoResult {
    /// The change, now marked undone.
    pub change: ConfigChange,
    /// What undoing it did.
    pub message: String,
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
    /// Where it was installed from, when an agent installed it (`install_extension`).
    #[serde(default)]
    pub installed_from: Option<InstallSource>,
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
    /// Where it was installed from, when an agent installed it (`install_extension`).
    #[serde(default)]
    pub installed_from: Option<InstallSource>,
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

/// `system.resources`: what the machine and the sessions are using right now.
#[derive(Debug)]
pub struct SystemResources;

impl Method for SystemResources {
    const NAME: &'static str = "system.resources";
    type Params = SystemResourcesParams;
    type Result = ResourceSnapshot;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemResourcesParams {}

/// `system.resources_subscribe`: start (`on: true`) or stop `system.resources_changed`
/// notifications on this connection. The daemon samples only while a connection is subscribed;
/// closing the connection unsubscribes it.
#[derive(Debug)]
pub struct SystemResourcesSubscribe;

impl Method for SystemResourcesSubscribe {
    const NAME: &'static str = "system.resources_subscribe";
    type Params = SystemResourcesSubscribeParams;
    type Result = SystemResourcesSubscribeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemResourcesSubscribeParams {
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SystemResourcesSubscribeResult {
    pub on: bool,
    /// How often snapshots arrive while subscribed.
    pub interval_ms: u32,
}

/// Notification carrying a [`ResourceSnapshot`], sent about every two seconds to connections
/// subscribed with `system.resources_subscribe`.
pub const RESOURCES_CHANGED: &str = "system.resources_changed";

/// What the machine and the sessions are using.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResourceSnapshot {
    /// When it was taken, in milliseconds since the Unix epoch.
    #[ts(type = "number")]
    pub taken_at_ms: u64,
    pub cpu: CpuUsage,
    pub memory: MemoryUsage,
    /// Empty when no supported GPU (NVIDIA, through its driver) is present.
    pub gpus: Vec<GpuUsage>,
    /// The daemon itself, then each model server it runs.
    pub processes: Vec<ProcessUsage>,
    /// Local models loaded now, in use or idle (cached until they unload).
    pub local_models: Vec<ActiveLocalModel>,
    /// Hosted endpoints that live sessions use, with their last 15 minutes of calls.
    pub endpoints: Vec<ActiveEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CpuUsage {
    /// Busy share of every core together, 0 to 100.
    pub total_percent: f32,
    /// Busy share of each logical core, 0 to 100.
    pub per_core: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MemoryUsage {
    #[ts(type = "number")]
    pub ram_used_bytes: u64,
    #[ts(type = "number")]
    pub ram_total_bytes: u64,
    #[ts(type = "number")]
    pub swap_used_bytes: u64,
    #[ts(type = "number")]
    pub swap_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GpuUsage {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    /// Busy share, 0 to 100; absent when the driver does not report it.
    pub utilization_percent: Option<f32>,
    #[ts(type = "number")]
    pub vram_used_bytes: u64,
    #[ts(type = "number")]
    pub vram_total_bytes: u64,
    pub temperature_c: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProcessUsage {
    pub pid: u32,
    pub name: String,
    /// `daemon` for imperium-core, otherwise the runtime serving `model` (`llamacpp`, `vllm`).
    pub role: String,
    pub model: Option<String>,
    /// Share of the whole machine (every core), 0 to 100.
    pub cpu_percent: f32,
    #[ts(type = "number")]
    pub ram_bytes: u64,
    /// Absent when the driver does not report GPU memory per process.
    #[ts(type = "number | null")]
    pub vram_bytes: Option<u64>,
}

/// A local model loaded by the supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActiveLocalModel {
    pub model: String,
    /// The runtime serving it: `llamacpp` or `vllm`.
    pub runtime: String,
    /// `gpu`, `partial` (layers split between GPU and RAM), or `cpu`.
    #[ts(type = "\"gpu\" | \"partial\" | \"cpu\"")]
    pub placement: String,
    pub gpu_layers: u32,
    /// Tokens one session may use.
    pub context: u32,
    /// Tokens of KV cache its slots share (the KV pool).
    pub pool_tokens: u32,
    /// Of those, tokens reserved by calls running now.
    pub pool_reserved_tokens: u32,
    /// Memory planned for it.
    #[ts(type = "number")]
    pub vram_bytes: u64,
    #[ts(type = "number")]
    pub ram_bytes: u64,
    pub slots: u32,
    pub slots_in_use: u32,
    /// Calls waiting for a slot or for room in the pool.
    pub queued: u32,
    /// Pinned to the CPU lane.
    pub cpu_pinned: bool,
    /// The draft model it runs with for speculative decoding; its memory is in `vram_bytes` and
    /// `ram_bytes`.
    #[serde(default)]
    pub draft_model: Option<String>,
    /// Why a configured draft model is not in use: the model runs without it.
    #[serde(default)]
    pub draft_notice: Option<String>,
    /// An agent is mid-turn on it (between its calls), so it stays loaded.
    pub claimed: bool,
    /// Live sessions on this model: as their model, or with calls on it in the last 15 minutes.
    pub sessions: Vec<String>,
    pub pid: Option<u32>,
    /// How long it has been idle; absent while it is in use (a slot in use, a call queued, or
    /// an agent mid-turn on it).
    #[ts(type = "number | null")]
    pub idle_ms: Option<u64>,
    /// When it unloads if it stays idle; absent while it is in use.
    #[ts(type = "number | null")]
    pub unload_in_ms: Option<u64>,
    /// `models.unload` can release it now (nothing is using it).
    pub can_unload: bool,
}

/// A hosted model live sessions use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ActiveEndpoint {
    /// The `[endpoints.<id>]` it is served from.
    pub endpoint: String,
    /// The endpoint's host, from its base URL.
    pub host: Option<String>,
    pub model: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    pub sessions: Vec<String>,
    /// Model calls in the last 15 minutes.
    pub requests: u32,
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    /// How long the latest call took.
    #[ts(type = "number | null")]
    pub last_latency_ms: Option<u64>,
    /// Failed turns on this model in the last 15 minutes, and the latest message.
    pub errors: u32,
    pub last_error: Option<String>,
}

/// `models.unload`: unload an idle local model now, freeing its memory.
#[derive(Debug)]
pub struct ModelUnload;

impl Method for ModelUnload {
    const NAME: &'static str = "models.unload";
    type Params = ModelUnloadParams;
    type Result = ModelUnloadResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUnloadParams {
    /// The local model id, as `local_models[].model` names it.
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelUnloadResult {
    pub unloaded: bool,
}
