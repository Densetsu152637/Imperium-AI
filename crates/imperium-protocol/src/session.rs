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
    /// What the model read on the call just completed, by category, from the request exactly as
    /// sent; follows that call's `model_call_completed`. When the provider reported
    /// `prompt_tokens`, the categories add up to it.
    ContextBreakdown {
        model: String,
        /// Tokens the model can read per request; absent when unknown.
        #[ts(type = "number | null")]
        context_window: Option<u64>,
        method: TokenCountMethod,
        /// The request's parts in a fixed order, leaving out empty ones. The last is
        /// `overhead` (chat template and formatting), which can be negative when the template
        /// drops text the request carried, such as earlier reasoning.
        categories: Vec<ContextCategory>,
        /// The prompt size the provider reported; absent when it reported no usage.
        #[ts(type = "number | null")]
        prompt_tokens: Option<u64>,
        /// Prompt tokens served from the provider's cache.
        #[ts(type = "number")]
        cached_tokens: u64,
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
    /// The agent's conversation was compacted to fit `model`'s context window: the system
    /// prompt and the most recent turns were kept verbatim and the older messages replaced by a
    /// summary that `summary_model` wrote.
    ContextCompacted {
        /// The model whose window the conversation was fitted to.
        model: String,
        /// The conversation's size for `model` before and after, counted by `method`.
        #[ts(type = "number")]
        before_tokens: u64,
        #[ts(type = "number")]
        after_tokens: u64,
        /// Messages replaced by the summary.
        summarised_messages: u32,
        /// The tokens the conversation may use: the window less room for the reply.
        #[ts(type = "number")]
        budget: u64,
        #[ts(type = "number | null")]
        context_window: Option<u64>,
        method: TokenCountMethod,
        summary_model: String,
    },
    /// The session's model changed; turns from now on use `to`. `compacted` says whether the
    /// conversation was compacted first (a `context_compacted` event precedes this one).
    ModelChanged {
        from: String,
        to: String,
        compacted: bool,
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

/// One part of a model request in a context breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextCategory {
    /// A stable id: `system_base`, `system_skills`, `system_memory`, `system_citations`,
    /// `system_project`, `tools_builtin`, `tools_mcp`, `summary` (a compacted conversation's
    /// summary), `manifest`, `memories`, `user`, `assistant`, `tool_results`, or `overhead`.
    /// Clients show unknown keys by their label.
    pub key: String,
    pub label: String,
    /// Only `overhead` can be negative.
    #[ts(type = "number")]
    pub tokens: i64,
}

/// How a context breakdown was counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TokenCountMethod {
    /// Each part counted with the model's own tokenizer; the overhead is the provider's
    /// reported prompt size minus the parts.
    Exact,
    /// Each part estimated, then scaled so the parts add up to the provider's reported prompt
    /// size.
    Calibrated,
    /// The provider reported no usage: parts are counted with the tokenizer where one is
    /// available, else estimated, and the formatting overhead is estimated.
    Estimated,
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

    #[test]
    fn context_breakdowns_serialise_with_their_method() {
        let event = SessionEvent::ContextBreakdown {
            model: "llamacpp:q".into(),
            context_window: Some(8192),
            method: TokenCountMethod::Exact,
            categories: vec![ContextCategory {
                key: "overhead".into(),
                label: "Chat template and formatting".into(),
                tokens: -3,
            }],
            prompt_tokens: None,
            cached_tokens: 0,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "context_breakdown");
        assert_eq!(value["method"], "exact");
        assert_eq!(value["categories"][0]["tokens"], -3);
        assert_eq!(value["prompt_tokens"], serde_json::Value::Null);
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
