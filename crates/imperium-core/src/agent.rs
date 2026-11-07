//! One agent's conversation and its turn loop.

use std::collections::HashSet;
use std::time::Instant;

use imperium_models::{ChatMessage, ChatRequest, ToolCall, Usage, stream_chat};
use imperium_protocol::session::{PermissionDecision, SessionEvent, TokenUsage};
use imperium_tools::{Access, Invocation, Workspace, specs};

use crate::ResolvedModel;

/// Longest tool output shown in events (the model receives the full output).
const EVENT_OUTPUT_CHARS: usize = 2_000;

/// How the agent asks for approval; the session answers from the user's response.
pub trait Approver {
    async fn approve(&mut self, request_id: u64, tool: &str, summary: &str) -> PermissionDecision;
}

pub struct Agent {
    http: reqwest::Client,
    model: ResolvedModel,
    workspace: Workspace,
    max_steps: u32,
    messages: Vec<ChatMessage>,
    allowed_for_session: HashSet<&'static str>,
    next_request_id: u64,
}

impl Agent {
    pub fn new(model: ResolvedModel, workspace: Workspace, max_steps: u32) -> Self {
        let system = format!(
            "You are Imperium, a coding agent working in the project folder {}. Use the tools \
             to inspect and change files and to run commands; paths are relative to the project \
             folder. Work step by step, check your changes, and finish with a short summary of \
             what you did.",
            workspace.root_display()
        );
        Agent {
            http: reqwest::Client::new(),
            model,
            workspace,
            max_steps,
            messages: vec![ChatMessage::System { content: system }],
            allowed_for_session: HashSet::new(),
            next_request_id: 1,
        }
    }

    /// Runs one turn: model calls and tool calls until the model answers without tools.
    pub async fn run_turn(
        &mut self,
        text: String,
        approver: &mut impl Approver,
        emit: &mut impl FnMut(SessionEvent),
    ) {
        emit(SessionEvent::TurnStarted { text: text.clone() });
        self.messages.push(ChatMessage::User { content: text });
        let tools = specs();
        let mut total = Usage::default();
        for step in 1..=self.max_steps {
            let request = ChatRequest {
                model: &self.model.model,
                messages: &self.messages,
                tools: &tools,
            };
            let started = Instant::now();
            let mut first_text = None;
            let completion = match stream_chat(&self.http, &self.model.endpoint, request, |text| {
                first_text.get_or_insert_with(Instant::now);
                emit(SessionEvent::TextDelta {
                    text: text.to_string(),
                })
            })
            .await
            {
                Ok(completion) => completion,
                Err(error) => {
                    emit(SessionEvent::TurnFailed {
                        message: error.to_string(),
                    });
                    return;
                }
            };
            let usage = completion.usage.unwrap_or_default();
            total.prompt_tokens += usage.prompt_tokens;
            total.completion_tokens += usage.completion_tokens;
            total.cached_tokens += usage.cached_tokens;
            emit(self.call_telemetry(&usage, started, first_text));
            self.messages.push(ChatMessage::Assistant {
                content: Some(completion.text).filter(|text| !text.is_empty()),
                tool_calls: completion.tool_calls.clone(),
            });
            if completion.tool_calls.is_empty() {
                emit(SessionEvent::TurnCompleted {
                    usage: token_usage(&total),
                    cost_usd: self.model.pricing.map(|pricing| pricing.cost_usd(&total)),
                    steps: step,
                });
                return;
            }
            for call in completion.tool_calls {
                let output = self.run_tool(&call, approver, emit).await;
                self.messages.push(ChatMessage::Tool {
                    tool_call_id: call.id,
                    content: output,
                });
            }
        }
        emit(SessionEvent::TurnFailed {
            message: format!(
                "stopped after {} model calls (agents.max_steps)",
                self.max_steps
            ),
        });
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
            cost_usd: self.model.pricing.map(|pricing| pricing.cost_usd(usage)),
        }
    }

    /// Runs one tool call and returns the text the model sees.
    async fn run_tool(
        &mut self,
        call: &ToolCall,
        approver: &mut impl Approver,
        emit: &mut impl FnMut(SessionEvent),
    ) -> String {
        let finish = |emit: &mut dyn FnMut(SessionEvent), ok: bool, output: String| {
            emit(SessionEvent::ToolFinished {
                call_id: call.id.clone(),
                ok,
                output: truncate(&output),
            });
            output
        };
        let invocation = match Invocation::parse(&call.function.name, &call.function.arguments) {
            Ok(invocation) => invocation,
            Err(error) => return finish(emit, false, format!("error: {error}")),
        };
        let tool = invocation.tool_name();
        let summary = invocation.summary();
        if invocation.access() != Access::Read && !self.allowed_for_session.contains(tool) {
            let request_id = self.next_request_id;
            self.next_request_id += 1;
            emit(SessionEvent::PermissionRequested {
                request_id,
                tool: tool.to_string(),
                summary: summary.clone(),
            });
            match approver.approve(request_id, tool, &summary).await {
                PermissionDecision::Allow => {}
                PermissionDecision::AllowForSession => {
                    self.allowed_for_session.insert(tool);
                }
                PermissionDecision::Deny => {
                    return finish(emit, false, "The user denied this action.".into());
                }
            }
        }
        emit(SessionEvent::ToolStarted {
            call_id: call.id.clone(),
            tool: tool.to_string(),
            summary,
        });
        match invocation.execute(&self.workspace).await {
            Ok(output) => finish(emit, true, output),
            Err(error) => finish(emit, false, format!("error: {error}")),
        }
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

fn truncate(text: &str) -> String {
    if text.chars().count() <= EVENT_OUTPUT_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(EVENT_OUTPUT_CHARS).collect();
    format!("{kept}…")
}
