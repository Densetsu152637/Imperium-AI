//! Session events, pushed to subscribed clients as `session.event` notifications.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Notification method carrying [`SessionEventParams`].
pub const SESSION_EVENT: &str = "session.event";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionEventParams {
    pub session_id: String,
    #[serde(flatten)]
    pub agent_event: AgentEvent,
}

/// The id of a session's root agent; subagents use their task id (`t1`, `t2`, ...).
pub const ROOT_AGENT: &str = "root";

/// An event and the agent it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentEvent {
    pub agent_id: String,
    pub event: SessionEvent,
}

/// Something that happened in a session, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum SessionEvent {
    /// The agent started working on a user message.
    TurnStarted { text: String },
    /// A fragment of the agent's reply.
    TextDelta { text: String },
    /// A tool call is about to run (after any approval).
    ToolStarted {
        call_id: String,
        tool: String,
        summary: String,
    },
    /// A tool call needs the user's approval; answer with `session.respond`.
    PermissionRequested {
        #[ts(type = "number")]
        request_id: u64,
        tool: String,
        summary: String,
    },
    /// A tool call finished, failed, or was denied. `output` is truncated for display.
    ToolFinished {
        call_id: String,
        ok: bool,
        output: String,
    },
    /// One model call finished; emitted after every call, so a turn can have several.
    ModelCallCompleted {
        model: String,
        usage: TokenUsage,
        #[ts(type = "number")]
        duration_ms: u64,
        /// Time to the first streamed text; absent when the call produced no text.
        #[ts(type = "number | null")]
        time_to_first_token_ms: Option<u64>,
        /// Completion tokens per second after the first token (or over the whole call).
        tokens_per_sec: Option<f64>,
        cost_usd: Option<f64>,
    },
    /// The agent replaced its plan.
    PlanUpdated { plan: Plan },
    /// The agent delegated a task to a subagent, whose events carry `task_id` as agent id.
    TaskSpawned {
        task_id: String,
        brief: String,
        model: String,
    },
    /// A subagent finished; its parent receives `report` when it awaits the task.
    TaskFinished {
        task_id: String,
        ok: bool,
        /// The subagent's final reply, truncated for display.
        report: String,
        usage: TokenUsage,
        cost_usd: Option<f64>,
    },
    /// The agent finished the turn.
    TurnCompleted {
        usage: TokenUsage,
        /// Absent when the model has no configured prices.
        cost_usd: Option<f64>,
        /// Model calls made in this turn.
        steps: u32,
    },
    /// The turn stopped because of an error.
    TurnFailed { message: String },
    /// Something the user should know that is not part of a turn, such as an MCP server that
    /// could not start.
    Notice { message: String },
    /// Memories recalled into the agent's context for this turn.
    MemoryUsed { names: Vec<String> },
    /// The agent saved a memory.
    MemorySaved { name: String, scope: String },
    /// What the workflow says this agent should load for its role and task.
    ContextManifest {
        role: String,
        items: Vec<ContextItem>,
        documentation: Vec<ContextPointer>,
    },
    /// The agent loaded workflow or documentation references.
    ContextLoaded { references: Vec<String> },
    /// The agent's browser after an action: page, title, and a JPEG screenshot (base64), so
    /// clients can mirror it.
    BrowserView {
        url: String,
        title: String,
        screenshot: String,
    },
    /// A model call hit a usage or rate limit; the turn waits until `until_unix_ms` (or until
    /// resumed early), following the session's resume policy.
    Paused {
        #[ts(type = "number")]
        until_unix_ms: u64,
        reason: String,
    },
    /// The wait ended and the turn continues.
    Resumed {},
    /// The session's status changed. `detail` names what it watches, such as `owner/repo#12`.
    StatusChanged {
        status: SessionStatus,
        detail: Option<String>,
    },
    /// An agent generated a media file (relative to the project), with what produced it.
    MediaCreated {
        path: String,
        #[ts(type = "\"image\" | \"speech\"")]
        kind: String,
        prompt: String,
    },
}

/// An instruction file or section in a context manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextItem {
    pub reference: String,
    pub reason: String,
    pub required: bool,
    pub tokens: u32,
}

/// A pointer to project documentation in a context manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextPointer {
    pub reference: String,
    pub reason: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub prompt_tokens: u64,
    #[ts(type = "number")]
    pub completion_tokens: u64,
    #[ts(type = "number")]
    pub cached_tokens: u64,
}

/// A goal and the steps toward it, kept by an agent with the `update_plan` tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Plan {
    pub goal: String,
    pub steps: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanStep {
    pub title: String,
    pub status: StepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Blocked,
    Skipped,
}

/// The user's answer to a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionDecision {
    Allow,
    /// Allow this tool for the rest of the session.
    AllowForSession,
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_are_tagged_by_type() {
        let event = SessionEvent::PermissionRequested {
            request_id: 3,
            tool: "write_file".into(),
            summary: "write a.txt".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"type":"permission_requested","request_id":3,"tool":"write_file","summary":"write a.txt"})
        );
        assert_eq!(
            serde_json::to_value(PermissionDecision::AllowForSession).unwrap(),
            json!("allow_for_session")
        );
    }
}

/// What a session is doing, as clients show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SessionStatus {
    /// Nothing running; ready for a message.
    Idle,
    /// A turn is running.
    Working,
    /// Waiting on the user: a permission request, or a turn that failed.
    NeedsAttention,
    /// Waiting for a usage limit to reset.
    Paused,
    /// Idle, watching a pull request's checks so failures are fixed reactively.
    Watching,
    /// Its pull request merged.
    Merged,
}

/// How a session waits when a provider says usage has run out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResumePolicy {
    /// `wait_for_reset` (the provider's reset time, else retry every interval), `at_time`
    /// (wait for a local time such as 03:00, then retry every interval), `retry_every`, or
    /// `stop` (fail the turn).
    #[ts(type = "\"wait_for_reset\" | \"at_time\" | \"retry_every\" | \"stop\"")]
    pub mode: String,
    /// `HH:MM`, local time, for `at_time`.
    pub at_time: Option<String>,
    pub retry_every_minutes: u32,
    /// Give up (fail the turn) after waiting this long in one turn.
    pub max_wait_hours: u32,
}
