//! One agent's conversation and its turn loop, including planning and delegation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use imperium_config::Tier;
use imperium_models::{ChatMessage, ChatRequest, ToolCall, ToolSpec, Usage, stream_chat};
use imperium_protocol::session::{
    PermissionDecision, Plan, PlanStep, SessionEvent, StepStatus, TokenUsage,
};
use imperium_tools::{Access, Invocation, specs};
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;

use crate::session::{Shared, lock};
use crate::{ResolvedModel, route};

/// Longest tool output or report shown in events (the model receives the full text).
const EVENT_OUTPUT_CHARS: usize = 2_000;
/// Longest subagent report handed back to the parent.
const REPORT_CHARS: usize = 8_000;

/// What a turn produced, for a parent awaiting a subagent.
pub struct TurnOutcome {
    pub text: String,
    pub ok: bool,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
}

struct TaskReport {
    task_id: String,
    model: String,
    ok: bool,
    report: String,
}

pub struct Agent {
    id: String,
    shared: Arc<Shared>,
    model: ResolvedModel,
    messages: Vec<ChatMessage>,
    /// Only the root agent delegates; subagents do their brief themselves.
    can_delegate: bool,
    tasks: HashMap<String, JoinHandle<TaskReport>>,
}

impl Agent {
    pub fn new(id: String, shared: Arc<Shared>, model: ResolvedModel, can_delegate: bool) -> Self {
        let root = shared.workspace.root_display();
        let system = if can_delegate {
            format!(
                "You are Imperium, a coding agent working in the project folder {root}. Use the \
                 tools to inspect and change files and to run commands; paths are relative to the \
                 project folder. For multi-step work, keep a checklist with update_plan. For \
                 independent pieces of a larger task, delegate with spawn_agent (a self-contained \
                 brief; optionally a tier: basic, standard, strong, frontier), then call \
                 await_tasks to collect the reports; you use no tokens while waiting. Check the \
                 results, and finish with a short summary of what was done."
            )
        } else {
            format!(
                "You are an Imperium subagent working in the project folder {root}. Complete the \
                 brief you are given using the tools; paths are relative to the project folder. \
                 Your final reply is your report to the agent that delegated to you: state what \
                 you did, what you found, and anything left undone."
            )
        };
        Agent {
            id,
            shared,
            model,
            messages: vec![ChatMessage::System { content: system }],
            can_delegate,
            tasks: HashMap::new(),
        }
    }

    fn emit(&self, event: SessionEvent) {
        self.shared.emitter.emit(&self.id, event);
    }

    fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut tools = specs();
        tools.push(update_plan_spec());
        if self.can_delegate {
            tools.extend([spawn_agent_spec(), await_tasks_spec()]);
        }
        tools
    }

    /// Runs one turn: model calls and tool calls until the model answers without tools.
    pub async fn run_turn(&mut self, text: String) -> TurnOutcome {
        self.emit(SessionEvent::TurnStarted { text: text.clone() });
        self.messages.push(ChatMessage::User { content: text });
        let tools = self.tool_specs();
        let mut total = Usage::default();
        let mut reply = String::new();
        let max_steps = self.shared.max_steps;
        for step in 1..=max_steps {
            let request = ChatRequest {
                model: &self.model.model,
                messages: &self.messages,
                tools: &tools,
            };
            let started = Instant::now();
            let mut first_text = None;
            let emitter = &self.shared.emitter;
            let id = &self.id;
            let result = stream_chat(&self.shared.http, &self.model.endpoint, request, |text| {
                first_text.get_or_insert_with(Instant::now);
                emitter.emit(
                    id,
                    SessionEvent::TextDelta {
                        text: text.to_string(),
                    },
                );
            })
            .await;
            let completion = match result {
                Ok(completion) => completion,
                Err(error) => {
                    let message = error.to_string();
                    self.emit(SessionEvent::TurnFailed {
                        message: message.clone(),
                    });
                    return self.outcome(message, false, total);
                }
            };
            let usage = completion.usage.unwrap_or_default();
            total.prompt_tokens += usage.prompt_tokens;
            total.completion_tokens += usage.completion_tokens;
            total.cached_tokens += usage.cached_tokens;
            self.emit(self.call_telemetry(&usage, started, first_text));
            if !completion.text.is_empty() {
                reply = completion.text.clone();
            }
            self.messages.push(ChatMessage::Assistant {
                content: Some(completion.text).filter(|text| !text.is_empty()),
                tool_calls: completion.tool_calls.clone(),
            });
            if completion.tool_calls.is_empty() {
                self.emit(SessionEvent::TurnCompleted {
                    usage: token_usage(&total),
                    cost_usd: self.cost(&total),
                    steps: step,
                });
                return self.outcome(reply, true, total);
            }
            for call in completion.tool_calls {
                let output = self.run_tool(&call).await;
                self.messages.push(ChatMessage::Tool {
                    tool_call_id: call.id,
                    content: output,
                });
            }
        }
        let message = format!("stopped after {max_steps} model calls (agents.max_steps)");
        self.emit(SessionEvent::TurnFailed {
            message: message.clone(),
        });
        self.outcome(message, false, total)
    }

    fn outcome(&self, text: String, ok: bool, usage: Usage) -> TurnOutcome {
        TurnOutcome {
            text,
            ok,
            cost_usd: self.cost(&usage),
            usage,
        }
    }

    fn cost(&self, usage: &Usage) -> Option<f64> {
        self.model.pricing.map(|pricing| pricing.cost_usd(usage))
    }

    fn call_telemetry(
        &self,
        usage: &Usage,
        started: Instant,
        first_text: Option<Instant>,
    ) -> SessionEvent {
        let duration = started.elapsed();
        // Generation speed after the first token, unless the endpoint delivered everything at
        // once (a buffered response), where only the whole call's duration is meaningful.
        let after_first = first_text.map(|first| first.elapsed());
        let generating = after_first
            .filter(|window| window.as_millis() >= 100)
            .unwrap_or(duration);
        let tokens_per_sec = (usage.completion_tokens > 0 && !generating.is_zero())
            .then(|| usage.completion_tokens as f64 / generating.as_secs_f64());
        SessionEvent::ModelCallCompleted {
            model: self.model.spec.clone(),
            usage: token_usage(usage),
            duration_ms: millis(duration),
            time_to_first_token_ms: first_text.map(|first| millis(first - started)),
            tokens_per_sec,
            cost_usd: self.cost(usage),
        }
    }

    /// Runs one tool call and returns the text the model sees.
    async fn run_tool(&mut self, call: &ToolCall) -> String {
        let name = call.function.name.as_str();
        let arguments = call.function.arguments.as_str();
        let result = match name {
            "update_plan" => self.update_plan(arguments),
            "spawn_agent" if self.can_delegate => self.spawn_agent(arguments),
            "await_tasks" if self.can_delegate => self.await_tasks(arguments).await,
            _ => return self.run_builtin(call).await,
        };
        let (ok, output) = match result {
            Ok(output) => (true, output),
            Err(error) => (false, format!("error: {error}")),
        };
        self.emit(SessionEvent::ToolFinished {
            call_id: call.id.clone(),
            ok,
            output: truncate(&output, EVENT_OUTPUT_CHARS),
        });
        output
    }

    async fn run_builtin(&self, call: &ToolCall) -> String {
        let finish = |ok: bool, output: String| {
            self.emit(SessionEvent::ToolFinished {
                call_id: call.id.clone(),
                ok,
                output: truncate(&output, EVENT_OUTPUT_CHARS),
            });
            output
        };
        let invocation = match Invocation::parse(&call.function.name, &call.function.arguments) {
            Ok(invocation) => invocation,
            Err(error) => return finish(false, format!("error: {error}")),
        };
        let tool = invocation.tool_name();
        let summary = invocation.summary();
        let pre_approved = lock(&self.shared.allowed_tools).contains(tool);
        if invocation.access() != Access::Read && !pre_approved {
            let (request_id, answer) = self.shared.broker.request();
            self.emit(SessionEvent::PermissionRequested {
                request_id,
                tool: tool.to_string(),
                summary: summary.clone(),
            });
            match answer.await.unwrap_or(PermissionDecision::Deny) {
                PermissionDecision::Allow => {}
                PermissionDecision::AllowForSession => {
                    lock(&self.shared.allowed_tools).insert(tool);
                }
                PermissionDecision::Deny => {
                    return finish(false, "The user denied this action.".into());
                }
            }
        }
        self.emit(SessionEvent::ToolStarted {
            call_id: call.id.clone(),
            tool: tool.to_string(),
            summary,
        });
        match invocation.execute(&self.shared.workspace).await {
            Ok(output) => finish(true, output),
            Err(error) => finish(false, format!("error: {error}")),
        }
    }

    fn update_plan(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            #[serde(default)]
            goal: String,
            steps: Vec<StepArgs>,
        }
        #[derive(Deserialize)]
        struct StepArgs {
            title: String,
            status: StepStatus,
        }
        let args: Args = parse(arguments)?;
        let plan = Plan {
            goal: args.goal,
            steps: args
                .steps
                .into_iter()
                .map(|step| PlanStep {
                    title: step.title,
                    status: step.status,
                })
                .collect(),
        };
        let done = plan
            .steps
            .iter()
            .filter(|step| step.status == StepStatus::Done)
            .count();
        let summary = format!("plan updated: {done} of {} steps done", plan.steps.len());
        self.emit(SessionEvent::PlanUpdated { plan });
        Ok(summary)
    }

    fn spawn_agent(&mut self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            brief: String,
            #[serde(default)]
            tier: Option<String>,
            #[serde(default)]
            model: Option<String>,
        }
        let args: Args = parse(arguments)?;
        if args.brief.trim().is_empty() {
            return Err("brief must not be empty".into());
        }
        let tier = match args.tier.as_deref() {
            Some(tier) => tier.parse::<Tier>()?,
            None => Tier::default(),
        };
        let model = route(
            &self.shared.catalog,
            &self.shared.fallback_model,
            args.model.as_deref(),
            tier,
        );
        let task_id = self.shared.next_task_id();
        self.emit(SessionEvent::TaskSpawned {
            task_id: task_id.clone(),
            brief: args.brief.clone(),
            model: model.spec.clone(),
        });
        let shared = Arc::clone(&self.shared);
        let id = task_id.clone();
        let spec = model.spec.clone();
        // Each subagent is its own task, so subagents run in parallel across the runtime's threads.
        let handle = tokio::spawn(async move {
            let mut agent = Agent::new(id.clone(), Arc::clone(&shared), model, false);
            let outcome = agent.run_turn(args.brief).await;
            shared.emitter.emit(
                &id,
                SessionEvent::TaskFinished {
                    task_id: id.clone(),
                    ok: outcome.ok,
                    report: truncate(&outcome.text, EVENT_OUTPUT_CHARS),
                    usage: token_usage(&outcome.usage),
                    cost_usd: outcome.cost_usd,
                },
            );
            TaskReport {
                task_id: id,
                model: spec,
                ok: outcome.ok,
                report: truncate(&outcome.text, REPORT_CHARS),
            }
        });
        self.tasks.insert(task_id.clone(), handle);
        Ok(format!(
            "started {task_id}; call await_tasks to collect its report"
        ))
    }

    /// Waits for subagents without calling the model, then returns their reports.
    async fn await_tasks(&mut self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize, Default)]
        struct Args {
            #[serde(default)]
            task_ids: Option<Vec<String>>,
        }
        let args: Args = parse(arguments)?;
        let mut ids = args
            .task_ids
            .unwrap_or_else(|| self.tasks.keys().cloned().collect());
        ids.sort();
        if ids.is_empty() {
            return Ok("no tasks are running".into());
        }
        let mut reports = Vec::new();
        for id in ids {
            let Some(handle) = self.tasks.remove(&id) else {
                reports.push(format!("## {id}\nunknown or already collected"));
                continue;
            };
            reports.push(match handle.await {
                Ok(task) => format!(
                    "## {} ({}, {})\n{}",
                    task.task_id,
                    task.model,
                    if task.ok { "finished" } else { "failed" },
                    task.report
                ),
                Err(error) => format!("## {id}\nthe subagent crashed: {error}"),
            });
        }
        Ok(reports.join("\n\n"))
    }
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, String> {
    let arguments = if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    };
    serde_json::from_str(arguments).map_err(|error| format!("invalid arguments: {error}"))
}

fn update_plan_spec() -> ToolSpec {
    ToolSpec {
        name: "update_plan".into(),
        description: "Replace your plan: a goal and its steps with statuses. Keep exactly one step in_progress while working.".into(),
        parameters: json!({"type": "object", "properties": {
            "goal": {"type": "string"},
            "steps": {"type": "array", "items": {"type": "object", "properties": {
                "title": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "in_progress", "done", "blocked", "skipped"]}
            }, "required": ["title", "status"]}}
        }, "required": ["steps"]}),
    }
}

fn spawn_agent_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_agent".into(),
        description: "Delegate an independent task to a subagent that runs in parallel. The brief must be self-contained. Returns a task id; collect the report with await_tasks.".into(),
        parameters: json!({"type": "object", "properties": {
            "brief": {"type": "string"},
            "tier": {"type": "string", "enum": ["basic", "standard", "strong", "frontier"], "description": "Minimum model capability; the cheapest suitable model is chosen. Default standard."},
            "model": {"type": "string", "description": "A specific <endpoint>:<model> from the catalog"}
        }, "required": ["brief"]}),
    }
}

fn await_tasks_spec() -> ToolSpec {
    ToolSpec {
        name: "await_tasks".into(),
        description: "Wait for subagents to finish and return their reports. Omit task_ids to wait for all running tasks. Uses no tokens while waiting.".into(),
        parameters: json!({"type": "object", "properties": {
            "task_ids": {"type": "array", "items": {"type": "string"}}
        }}),
    }
}

fn token_usage(usage: &Usage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        cached_tokens: usage.cached_tokens,
    }
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}
