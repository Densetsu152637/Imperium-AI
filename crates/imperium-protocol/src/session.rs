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
        /// The rule "always allow" would add (`permissions.add_rule`), when there is a sensible
        /// one: the command and its subcommand, the file's folder, or the whole tool.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        suggested_rule: Option<crate::permissions::PermissionRule>,
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
        /// Time the call waited for a slot on a local model before it started, which
        /// `duration_ms` leaves out; absent for hosted models.
        #[ts(type = "number | null")]
        queue_wait_ms: Option<u64>,
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
    /// A finished subagent was given a follow-up (`resume_agent`, or `session.agent_resume`)
    /// and works again with its earlier conversation; `task_finished` follows as for a new
    /// task. Emitted by the agent that resumed it (`root`).
    TaskResumed {
        task_id: String,
        /// The follow-up message.
        text: String,
        model: String,
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
    /// The session waits: a model call hit a usage or rate limit (the turn waits until
    /// `until_unix_ms`, or until resumed early, following the session's resume policy), or
    /// background work is giving way to the user (`until_unix_ms` is 0; it resumes when the
    /// machine is quiet or when the user says to run it now).
    Paused {
        #[ts(type = "number")]
        until_unix_ms: u64,
        reason: String,
        /// Why it waits; older journals have no cause and mean a usage limit.
        #[serde(default)]
        cause: PauseCause,
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
    /// conversation was compacted first (a `context_compacted` event precedes this one). `from`
    /// is empty when the session had no model before.
    ModelChanged {
        from: String,
        to: String,
        compacted: bool,
    },
    /// A message arrived while the agent was busy and waits for it: a queued message runs as
    /// the agent's next turn, a steering one joins its running turn at the next safe point.
    /// `queue_changed` follows with the whole queue.
    MessageQueued {
        id: String,
        text: String,
        steer: bool,
    },
    /// The agent's queue after a message was queued, edited, removed, delivered, or joined the
    /// running turn. The latest one is the queue; a resumed session restores it.
    QueueChanged { messages: Vec<QueuedMessage> },
    /// A steering message joined the running turn: the model reads it, as a user message
    /// marked as steering, on its next call. The turn goes on; nothing was cancelled.
    Steered { id: String, text: String },
    /// The daemon offers to download a local model better suited to what the session needs.
    /// Nothing is downloaded unless the user accepts with `session.suggestion_respond`.
    ModelSuggestion {
        /// Names the suggestion in `session.suggestion_respond` and `model_suggestion_update`.
        id: String,
        /// `local` (a general local model), `vision`, `tools`, `code`, or `context`.
        need: String,
        /// The Hugging Face repository, `owner/name`.
        repo: String,
        /// The GGUF quantisation chosen to fit this machine.
        quant: String,
        /// What the session can switch to once it is downloaded, such as `llamacpp:Qwen3-4B-Q4_K_M`.
        model: String,
        /// The download's size, a vision projector included.
        #[ts(type = "number")]
        size_bytes: u64,
        /// How it would load here: `gpu`, `partial` (split with system memory), or `cpu`.
        #[ts(type = "\"gpu\" | \"partial\" | \"cpu\"")]
        fit: String,
        /// Why it is suggested, in words for the user.
        reason: String,
        /// The multimodal projector is downloaded too.
        mmproj: bool,
    },
    /// What became of a model suggestion.
    ModelSuggestionUpdate {
        id: String,
        /// `downloading` (with `job_id`; follow `models.download_progress`), `ready` (with
        /// `model`; switch with `session.control` `set_model`), `failed` or `cancelled` (with
        /// `message`), `dismissed` (not now), or `disabled` (don't ask again: suggestions are
        /// off).
        #[ts(
            type = "\"downloading\" | \"ready\" | \"failed\" | \"cancelled\" | \"dismissed\" | \"disabled\""
        )]
        state: String,
        job_id: Option<String>,
        model: Option<String>,
        message: Option<String>,
    },
    /// The agent paused to ask the user a batch of questions; answer with `session.answer`.
    /// Subagents already running keep going meanwhile.
    QuestionsAsked {
        #[ts(type = "number")]
        request_id: u64,
        questions: Vec<Question>,
    },
    /// A batch of questions was answered, from any client, so the rest can close it.
    QuestionsAnswered {
        #[ts(type = "number")]
        request_id: u64,
    },
}

/// A message waiting for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QueuedMessage {
    /// `q1`, `q2`, ...: unique in the session.
    pub id: String,
    pub text: String,
    /// Joins the agent's running turn at its next safe point instead of waiting for the turn
    /// to end; when no turn is running it starts one.
    pub steer: bool,
}

/// How `session.send` delivers a message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SendMode {
    /// Start a turn now when the agent is idle; otherwise wait in its queue and run as a new
    /// turn after the current one, in order.
    #[default]
    Queue,
    /// Join the running turn after its current model response and tool calls, before the next
    /// model call, without cancelling it. Like `queue` when no turn is running.
    Steer,
}

/// One question for the user, with the answer the agent suggests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Question {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    pub recommended: String,
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

/// Where an agent stands, in `session.agents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentStatus {
    /// Working on a turn.
    Running,
    /// Finished its last turn; a subagent can be resumed with a follow-up.
    Idle,
    /// Its last turn failed; a subagent can still be resumed.
    Failed,
    /// The daemon stopped while it was working; it can be resumed from where it was saved.
    Interrupted,
}

/// An agent of a session and its kept state, from `session.agents`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentSummary {
    /// `root`, or a subagent's task id (`t1`, ...).
    pub agent_id: String,
    /// The agent that started it; `None` for the root.
    pub parent: Option<String>,
    /// The workflow role it loads files for: `root`, `leaf`, or what its delegator asked.
    pub role: String,
    /// The first line of its brief, for lists.
    pub title: String,
    /// Its brief (empty for the root).
    pub task: String,
    /// The model spec it last ran on.
    pub model: String,
    /// Served by a local runtime, which it takes a slot on for each turn.
    pub local: bool,
    pub status: AgentStatus,
    /// Its conversation is kept, so `resume_agent` / `session.agent_resume` can continue it.
    /// Always false for the root, which is continued by sending the session a message.
    pub resumable: bool,
    /// Turns it has run (the brief and every follow-up).
    pub turns: u32,
    /// Messages in its kept conversation.
    pub messages: u32,
    /// Every model call it made, across turns.
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
    /// Its latest plan, when it kept one.
    pub plan: Option<Plan>,
    /// Its latest report (a subagent's final reply), truncated.
    pub last_report: Option<String>,
    /// When its state last changed, in milliseconds since the Unix epoch.
    #[ts(type = "number")]
    pub updated_at_ms: u64,
}

/// A goal and the steps toward it, kept by an agent with the `update_plan` tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Plan {
    pub goal: String,
    pub steps: Vec<PlanStep>,
    /// Gaps the agent filled itself, each with the value it chose.
    #[serde(default)]
    pub assumptions: Vec<String>,
    /// Decisions left to the user, or that cannot be researched.
    #[serde(default)]
    pub open_questions: Vec<String>,
}

/// One stage of a plan. Everything after `status` is optional so plans saved before those
/// fields existed still load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanStep {
    pub title: String,
    pub status: StepStatus,
    /// Titles of the steps that must finish first.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Who does the step: the agent itself, or a subagent and its tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub owner: Option<String>,
    /// The check that shows the step is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub acceptance: Option<String>,
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
            suggested_rule: None,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"type":"permission_requested","request_id":3,"tool":"write_file","summary":"write a.txt"})
        );
        let suggested = SessionEvent::PermissionRequested {
            request_id: 4,
            tool: "run_command".into(),
            summary: "run `cargo test`".into(),
            suggested_rule: Some(crate::permissions::PermissionRule {
                tool: "run_command".into(),
                pattern: "cargo test".into(),
                action: crate::permissions::RuleAction::Allow,
            }),
        };
        assert_eq!(
            serde_json::to_value(&suggested).unwrap()["suggested_rule"],
            json!({"tool":"run_command","pattern":"cargo test","action":"allow"})
        );
        assert_eq!(
            serde_json::to_value(PermissionDecision::AllowForSession).unwrap(),
            json!("allow_for_session")
        );
    }

    #[test]
    fn plans_round_trip_with_their_stage_details() {
        let plan = Plan {
            goal: "ship".into(),
            steps: vec![
                PlanStep {
                    title: "research".into(),
                    status: StepStatus::Done,
                    depends_on: vec![],
                    owner: Some("subagent, standard".into()),
                    acceptance: Some("three sources cited".into()),
                },
                PlanStep {
                    title: "build".into(),
                    status: StepStatus::InProgress,
                    depends_on: vec!["research".into()],
                    owner: None,
                    acceptance: None,
                },
            ],
            assumptions: vec!["Rust 1.80".into()],
            open_questions: vec!["Which licence?".into()],
        };
        let value = serde_json::to_value(&plan).unwrap();
        assert_eq!(value["steps"][1]["depends_on"], json!(["research"]));
        assert!(value["steps"][1].get("owner").is_none());
        assert_eq!(serde_json::from_value::<Plan>(value).unwrap(), plan);
    }

    #[test]
    fn plans_saved_without_stage_details_still_load() {
        let plan: Plan = serde_json::from_value(
            json!({"goal": "g", "steps": [{"title": "t", "status": "pending"}]}),
        )
        .unwrap();
        assert!(plan.assumptions.is_empty() && plan.open_questions.is_empty());
        let step = &plan.steps[0];
        assert!(step.depends_on.is_empty() && step.owner.is_none() && step.acceptance.is_none());
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

/// Why a session is [`SessionEvent::Paused`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PauseCause {
    /// A provider's usage limit.
    #[default]
    UsageLimit,
    /// Background work paused at a step boundary because the machine got busy.
    Load,
    /// Background work that has not started, waiting for a quiet machine.
    Deferred,
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
    /// Waiting for a usage limit to reset, or (background work) for a quiet machine.
    Paused,
    /// A message is waiting, but the session has no model yet; it runs once one is set.
    NeedsModel,
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
