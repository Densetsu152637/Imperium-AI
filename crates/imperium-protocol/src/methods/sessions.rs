use super::*;

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

// `session.agent_knowledge`: the user keeps a finished, read-only subagent as a knowledge agent
// (`topic`) or retires one (`retire`), as the root agent does with `keep_as_knowledge`
// (docs/21-knowledge-agents.md). Refused with the reason when the agent cannot be kept.
#[derive(Debug)]
pub struct SessionAgentKnowledge;

impl Method for SessionAgentKnowledge {
    const NAME: &'static str = "session.agent_knowledge";
    type Params = SessionAgentKnowledgeParams;
    type Result = SessionAgentKnowledgeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentKnowledgeParams {
    pub session_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub topic: Option<String>,
    #[serde(default)]
    pub retire: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentKnowledgeResult {
    pub agent_id: String,
    // The card after the change; absent once the agent is retired.
    pub card: Option<crate::session::KnowledgeCard>,
}

// `session.agent_ask`: the user asks a knowledge agent a question (docs/21-knowledge-agents.md),
// as an agent does with `ask_agent`: it runs in a fork, may time out (`timeout_s`, default 300, at
// most 600), is journalled as `question_asked` and `question_answered`, and answers with the
// outcome and the answer text (the reason when it was not answered).
#[derive(Debug)]
pub struct SessionAgentAsk;

impl Method for SessionAgentAsk {
    const NAME: &'static str = "session.agent_ask";
    type Params = SessionAgentAskParams;
    type Result = SessionAgentAskResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentAskParams {
    pub session_id: String,
    // A knowledge agent's id or title.
    pub agent_id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub timeout_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAgentAskResult {
    pub question_id: String,
    pub outcome: crate::session::QuestionOutcome,
    pub answer: String,
    // The marks the answer carries: its sources changed since they were read, and it may hold
    // web material or tool results.
    pub degraded: bool,
    pub untrusted_web: bool,
    pub untrusted_tools: bool,
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

/// `session.tools`: the tools `session.run_tool` can run for an outside caller, with their
/// definitions. Used by the MCP server bridge (`imperium mcp-serve --tools`); not part of the
/// desktop app's protocol, so it has no generated TypeScript type, and a paired phone cannot
/// call it.
#[derive(Debug)]
pub struct SessionTools;

impl Method for SessionTools {
    const NAME: &'static str = "session.tools";
    type Params = SessionToolsParams;
    type Result = SessionToolsResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolsParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionToolsResult {
    /// Read tools first.
    pub tools: Vec<BridgedToolInfo>,
}

/// A tool the bridge can run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BridgedToolInfo {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
    /// Whether it changes files, runs commands or saves memory: only run when `allow_write`.
    pub writes: bool,
}

/// `session.run_tool`: starts one of the session's own tools for an outside caller (the MCP
/// server bridge), between turns, and returns its id at once. The call runs exactly as a call
/// the session's agent makes does: the launcher's tool limits, the agent's permission profile
/// and local-files-only lock, the permission rules and safety floor, the user's approval (a
/// `permission_requested` event nobody but a person or the caller's `session.respond` answers),
/// hooks and the journal; no model is called. Collect the result with `session.tool_result`.
/// Not part of the desktop app's protocol, so it has no generated TypeScript type, and a paired
/// phone cannot call it.
#[derive(Debug)]
pub struct SessionRunTool;

impl Method for SessionRunTool {
    const NAME: &'static str = "session.run_tool";
    type Params = SessionRunToolParams;
    type Result = SessionRunToolResult;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRunToolParams {
    pub session_id: String,
    pub tool: String,
    /// The tool's arguments, an object.
    #[serde(default)]
    pub arguments: serde_json::Value,
    /// Allow the tools that change things (`writes` in `session.tools`). Off, only read tools
    /// run.
    #[serde(default)]
    pub allow_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRunToolResult {
    /// Names the call in `session.tool_result` and in the session's `tool_started` and
    /// `tool_finished` events.
    pub call_id: String,
}

/// `session.tool_result`: waits up to `wait_ms` (at most 5000) for a call started with
/// `session.run_tool` and takes its result: it is handed over once. A call still running
/// (including one waiting for a permission answer) answers `done: false`.
#[derive(Debug)]
pub struct SessionToolResult;

impl Method for SessionToolResult {
    const NAME: &'static str = "session.tool_result";
    type Params = SessionToolResultParams;
    type Result = SessionToolResultResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolResultParams {
    pub session_id: String,
    pub call_id: String,
    #[serde(default)]
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolResultResult {
    pub done: bool,
    /// Whether the tool ran and succeeded; a refusal or denial is `false` with the reason in
    /// `output`. Meaningless until `done`.
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub output: String,
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
    /// Set when a persistent agent started the session (or the user started one of its
    /// suggestions): shown as "started by <agent>" with the reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub started_by: Option<crate::initiative::StartedBy>,
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
