//! The agent loop's side of knowledge agents (docs/21-knowledge-agents.md): the tools that keep,
//! list and ask them, and the ephemeral fork that answers one question.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use imperium_api::parse_arguments as parse;
use imperium_api::{ChatMessage, ToolSpec, Usage};
use imperium_models::ChatRequest;
use imperium_protocol::session::{QuestionOutcome, SessionEvent};
use serde::Deserialize;
use tokio::sync::watch;

use super::{Agent, Streamed, maybe_signalled};
use crate::agents::FrozenTool;
use crate::breakdown::tool_texts;
use crate::knowledge::{
    Answer, Asker, ForkEnd, ForkRequest, ForkResult, MAX_QUESTION_CHARS, MAX_TIMEOUT_S, Question,
    Stop, stopped,
};
use crate::session::Shared;

/// The tools a fork may call: reads and the web. Everything else is refused where the call is
/// dispatched (docs/21-knowledge-agents.md, K4).
const FORK_TOOLS: [&str; 9] = [
    "read_file",
    "list_dir",
    "glob",
    "grep",
    "memory_search",
    "memory_read",
    "context_load",
    "web_search",
    "web_fetch",
];

/// The fork's tools that bring in material from the web.
const WEB_TOOLS: [&str; 2] = ["web_search", "web_fetch"];

/// What marks an agent as the fork that answers a question.
pub(super) struct ForkInfo {
    /// The knowledge agent it answers for.
    pub source: String,
    /// Raised when the question times out or is cancelled.
    pub stop: watch::Receiver<Option<Stop>>,
    used_web: AtomicBool,
    /// Its slot was saved under its last lease.
    saved: AtomicBool,
}

impl ForkInfo {
    /// Notes that a call to `tool` worked, for the answer's web marker.
    pub fn note_tool(&self, tool: &str) {
        if WEB_TOOLS.contains(&tool) {
            self.used_web.store(true, Ordering::SeqCst);
        }
    }

    fn used_web(&self) -> bool {
        self.used_web.load(Ordering::SeqCst)
    }
}

/// Why a fork may not call `name`; `None` when it may.
pub(super) fn refusal(name: &str) -> Option<String> {
    (!FORK_TOOLS.contains(&name)).then(|| {
        format!(
            "{name} is not available while you answer another agent's question: you may read \
             files, search, read memory and use the web, and nothing else. Answer from what you \
             have, or say that it is not in what you read."
        )
    })
}

/// What a fork is told when a call would have asked the user.
pub(super) fn ask_refusal(summary: &str) -> String {
    format!(
        "{summary} would need the user's approval, and a question's answer never asks for one, so \
         it was not run. Answer from what you have, or say that it is not in what you read."
    )
}

/// The events of a fork that reach the journal: its tool calls.
pub(super) fn forwarded(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ToolStarted { .. }
            | SessionEvent::ToolFinished { .. }
            | SessionEvent::PermissionRequested { .. }
            | SessionEvent::ContextLoaded { .. }
            | SessionEvent::MemoryUsed { .. }
    )
}

async fn cancelled(stop: &mut watch::Receiver<Option<Stop>>) {
    let _ = stopped(stop).await;
}

/// Answers one question in a fork of a knowledge agent: its frozen system message, tools and
/// conversation, unchanged, then the question. Nothing of it is kept.
pub(crate) async fn run_fork(shared: Arc<Shared>, request: ForkRequest) -> ForkResult {
    let effort = request.frozen.effort.clone();
    // What the runtime needs to save the slot once the fork is over, however it ends.
    let local_model = request.model.local.then(|| request.model.model.clone());
    let holder = imperium_api::holder(&shared.session_id, &request.source_id);
    let ForkRequest {
        fork_id,
        source_id,
        model,
        frozen,
        conversation,
        question,
        max_steps,
        stop,
    } = request;
    let tools: Vec<ToolSpec> = frozen.tools.iter().map(FrozenTool::spec).collect();
    let mut messages = vec![ChatMessage::System {
        content: frozen.system,
    }];
    messages.extend(conversation);
    messages.push(ChatMessage::user(question));
    let inbox = shared.inboxes.open(&fork_id);
    let workspace = shared.workspace.clone();
    let mut agent = Agent::bare(
        fork_id.clone(),
        inbox,
        Arc::clone(&shared),
        workspace,
        model,
        messages,
    );
    agent.can_ask = false;
    agent.role = "knowledge".into();
    // The reasoning effort the agent's own last request carried.
    agent.effort = effort.and_then(|effort| effort.parse::<imperium_api::Effort>().ok());
    agent.fork = Some(ForkInfo {
        source: source_id,
        stop,
        used_web: AtomicBool::new(false),
        saved: AtomicBool::new(false),
    });
    let result = agent.answer_loop(&tools, max_steps).await;
    shared.inboxes.close(&fork_id);
    drop(agent);
    // A fork that did not save its slot under its last lease (it was cancelled or failed, or
    // ended on an error) saves it now, with its cells in place, if nobody has taken it.
    // A session that has closed leaves no file behind.
    if !result.saved
        && !shared.knowledge.closed()
        && let (Some(model), Some(local)) = (local_model, shared.local.as_deref())
    {
        local.save_kept(&model, &holder).await;
    }
    result
}

impl Agent {
    fn cancel_signal(&self) -> watch::Receiver<Option<Stop>> {
        match &self.fork {
            Some(fork) => fork.stop.clone(),
            None => watch::channel(None).1,
        }
    }

    fn stop_reason(&self) -> Option<Stop> {
        self.fork.as_ref().and_then(|fork| *fork.stop.borrow())
    }

    fn ended(&self, end: ForkEnd, usage: Usage, calls: u32) -> ForkResult {
        ForkResult {
            saved: self
                .fork
                .as_ref()
                .is_some_and(|fork| fork.saved.load(Ordering::SeqCst)),
            end,
            usage,
            calls,
            used_web: self.fork.as_ref().is_some_and(ForkInfo::used_web),
        }
    }

    /// Waits while the session is stopped, at a safe point, unless the question is stopped first.
    async fn hold_unless_cancelled(
        &self,
        attached: &mut Option<imperium_api::LocalPresence>,
        holder: &str,
    ) -> Option<Stop> {
        let mut cancel = self.cancel_signal();
        tokio::select! {
            () = self.hold_while_stopped(attached, holder) => None,
            why = stopped(&mut cancel) => Some(why),
        }
    }

    /// The fork's turn: model calls and read-only tool calls until it answers.
    async fn answer_loop(&mut self, tools: &[ToolSpec], max_steps: u32) -> ForkResult {
        let mut usage = Usage::default();
        let mut calls = 0u32;
        let holder = self.holder();
        let mut attached = self
            .local_models()
            .map(|local| local.attach(&self.model.model, &holder));
        let texts = tool_texts(tools, &[]);
        let window = self.window().await;
        let estimate = u64::from(self.prompt_estimate(&texts));
        if estimate > window {
            let message = format!(
                "the frozen context and the question take about {estimate} tokens, which does \
                 not fit {}'s window of {window}; the question was not answered by another model",
                self.model.spec
            );
            return self.ended(ForkEnd::Refused(message), usage, calls);
        }
        let mut reply = String::new();
        for _ in 0..max_steps {
            if let Some(why) = self.stop_reason() {
                return self.ended(ForkEnd::Stopped(why), usage, calls);
            }
            if let Some(why) = self.hold_unless_cancelled(&mut attached, &holder).await {
                return self.ended(ForkEnd::Stopped(why), usage, calls);
            }
            let (result, started, queue_wait, lease) = loop {
                // Every call is checked against the budgets, charged to the knowledge agent's
                // model.
                if let Some(message) = self.budget_stop().await {
                    return self.ended(ForkEnd::Failed(message), usage, calls);
                }
                let prompt_tokens = self.prompt_estimate(&texts);
                let (client, lease) = match self.client(&holder, prompt_tokens, false).await {
                    Ok(taken) => taken,
                    Err(message) => return self.ended(ForkEnd::Failed(message), usage, calls),
                };
                let queue_wait = lease.as_ref().map(|lease| lease.waited);
                let request = ChatRequest {
                    model: &self.model.model,
                    messages: &self.messages,
                    tools,
                    slot: lease.as_ref().and_then(|lease| lease.slot),
                    max_tokens: lease.as_ref().and_then(|lease| lease.max_tokens),
                    effort: self.request_effort(),
                };
                let started = Instant::now();
                let mut ignore = |_: &str| {};
                let mut stop = self.shared.controls.stop_signal();
                let mut cancel = self.cancel_signal();
                let streamed = tokio::select! {
                    result = client.stream(request, &mut ignore) => Streamed::Done(result),
                    () = cancelled(&mut cancel) => Streamed::Paused,
                    Ok(_) = stop.wait_for(|stopped| *stopped) => Streamed::Stopped,
                };
                match streamed {
                    // The lease is kept until the reply is read: a final reply saves the slot
                    // under it.
                    Streamed::Done(result) => break (result, started, queue_wait, lease),
                    Streamed::Paused => {
                        drop(lease);
                        let why = self.stop_reason().unwrap_or(Stop::Cancelled);
                        return self.ended(ForkEnd::Stopped(why), usage, calls);
                    }
                    // The session was stopped: the request is sent again on resume.
                    Streamed::Stopped => {
                        drop(lease);
                        if let Some(why) = self.hold_unless_cancelled(&mut attached, &holder).await
                        {
                            return self.ended(ForkEnd::Stopped(why), usage, calls);
                        }
                    }
                }
            };
            let completion = match result {
                Ok(completion) => completion,
                Err(error) => {
                    return self.ended(ForkEnd::Failed(error.to_string()), usage, calls);
                }
            };
            let call_usage = completion.usage.unwrap_or_default();
            usage.prompt_tokens += call_usage.prompt_tokens;
            usage.completion_tokens += call_usage.completion_tokens;
            usage.cached_tokens += call_usage.cached_tokens;
            calls += 1;
            // The call is a model call of the session: budgets and usage count it for the
            // knowledge agent.
            if let Some(fork) = &self.fork {
                self.shared.emitter.emit(
                    &fork.source,
                    self.call_telemetry(&call_usage, started, completion.first_token, queue_wait),
                );
            }
            if !completion.text.trim().is_empty() {
                reply = completion.text.clone();
            }
            let text = completion.text;
            let tool_calls = completion.tool_calls;
            self.messages.push(ChatMessage::Assistant {
                content: Some(text.clone()).filter(|text| !text.is_empty()),
                tool_calls: tool_calls.clone(),
            });
            if self.model.local && completion.usage.is_some() {
                self.last_call = Some((
                    call_usage.prompt_tokens + call_usage.completion_tokens,
                    self.messages.len(),
                ));
            }
            // The slot of a fork's last call is saved while the fork still holds it: once it is
            // released a call queued for a slot takes it and writes over the prefix. A call that
            // goes on to tools, or a fork that is cancelled or fails, is saved after the fact,
            // best effort.
            if tool_calls.is_empty()
                && !self.shared.knowledge.closed()
                && let (Some(slot), Some(local), Some(fork)) = (
                    lease.as_ref().and_then(|lease| lease.slot),
                    self.local_models(),
                    &self.fork,
                )
                && local
                    .save_leased(&self.model.model, &holder, slot)
                    .await
                    .is_ok()
            {
                // Only a save that worked spares the after-the-fact one.
                fork.saved.store(true, Ordering::SeqCst);
            }
            drop(lease);
            if tool_calls.is_empty() {
                let end = if text.trim().is_empty() {
                    ForkEnd::Failed("the model replied with nothing".into())
                } else {
                    ForkEnd::Answered(text)
                };
                return self.ended(end, usage, calls);
            }
            for call in tool_calls {
                self.last_tool_ok.store(false, Ordering::SeqCst);
                let output = self.run_tool(&call).await;
                self.messages.push(ChatMessage::Tool {
                    tool_call_id: call.id,
                    content: output,
                });
            }
            // A fork that has filled its context window answers with what it has.
            if let Some(at) = self.shared.auto_compact_at
                && at > 0.0
                && f64::from(self.prompt_estimate(&texts)) > at * window as f64
            {
                let end = if reply.trim().is_empty() {
                    ForkEnd::Failed(
                        "it ran out of context room before it could answer (context.auto_compact_at)"
                            .into(),
                    )
                } else {
                    ForkEnd::Answered(format!(
                        "{reply}\n\n[it stopped here because its context was full]"
                    ))
                };
                return self.ended(end, usage, calls);
            }
        }
        let note =
            format!("[it stopped after {max_steps} model calls (knowledge.answer_max_steps)]");
        let end = if reply.trim().is_empty() {
            ForkEnd::Failed(format!("it did not finish: {note}"))
        } else {
            ForkEnd::Answered(format!("{reply}\n\n{note}"))
        };
        self.ended(end, usage, calls)
    }

    /// Waits for the question's answer; a timeout (which does not run while the session is
    /// stopped), a steering message to the root, or this agent being paused stops the question
    /// first.
    async fn wait_for_answer(&mut self, question: &mut Question, timeout: Duration) -> Answer {
        let mut stop = self.shared.controls.stop_signal();
        // A message the user sent before this wait ends it at once, as for `await_tasks`.
        if self.can_delegate && self.inbox.has_steering() {
            question.stop(Stop::Cancelled);
            return joined_answer((&mut question.handle).await);
        }
        let mut steering = self.can_delegate.then(|| self.inbox.steering_signal());
        let mut remaining = timeout;
        loop {
            // The time a stopped session spends stopped does not count.
            if *stop.borrow_and_update() {
                let _ = stop.wait_for(|stopped| !*stopped).await;
            }
            let began = Instant::now();
            let why = tokio::select! {
                joined = &mut question.handle => return joined_answer(joined),
                () = tokio::time::sleep(remaining) => Stop::Timeout,
                () = maybe_signalled(steering.as_mut()) => Stop::Cancelled,
                () = maybe_signalled(self.pause.as_mut()) => Stop::Cancelled,
                Ok(_) = stop.wait_for(|stopped| *stopped) => {
                    remaining = remaining.saturating_sub(began.elapsed());
                    continue;
                }
            };
            question.stop(why);
            return joined_answer((&mut question.handle).await);
        }
    }

    /// `ask_agent`: asks a knowledge agent a question and waits for the answer.
    pub(super) async fn ask_agent(&mut self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            agent: String,
            question: String,
            #[serde(default)]
            timeout_s: Option<u64>,
        }
        let args: Args = parse(arguments)?;
        let settings = self.shared.knowledge.settings().clone();
        if self.fork.is_some()
            || !self.can_ask
            || self
                .shared
                .agents
                .summary(&self.id)
                .is_some_and(|summary| summary.knowledge.is_some())
        {
            return Err(
                "knowledge agents and the agents that cannot delegate questions do not ask \
                 other agents"
                    .into(),
            );
        }
        let question = args.question.trim();
        if question.is_empty() {
            return Err("the question must not be empty".into());
        }
        if question.chars().count() > MAX_QUESTION_CHARS {
            return Err(format!(
                "the question is over {MAX_QUESTION_CHARS} characters; make it shorter and \
                 self-contained"
            ));
        }
        if self.asked_this_turn >= settings.max_questions_per_turn {
            return Err(format!(
                "you have asked {} questions in this turn (knowledge.max_questions_per_turn); \
                 answer from what you have, or read the material yourself",
                self.asked_this_turn
            ));
        }
        let target = crate::knowledge::resolve_target(&self.shared, &args.agent)?;
        if target.agent_id == self.id {
            return Err("you cannot ask yourself".into());
        }
        self.asked_this_turn += 1;
        let title = self
            .shared
            .agents
            .summary(&self.id)
            .map(|summary| summary.title)
            .unwrap_or_default();
        let timeout = Duration::from_secs(
            args.timeout_s
                .unwrap_or(u64::from(settings.default_timeout_s))
                .clamp(1, MAX_TIMEOUT_S),
        );
        let asker = Asker {
            id: self.id.clone(),
            title,
        };
        let fork_model = self.shared.agents.model(&target.agent_id);
        let mut pending =
            crate::knowledge::start_question(&self.shared, asker, target, question.to_string());
        // Asleep until the answer arrives, so the local runtime may swap this agent's model out
        // for the fork's and restore it afterwards; but not when the fork runs where it needs
        // nothing of this agent's slot: hosted, or loaded with a free slot beside it.
        let asleep = self.local_models().filter(|local| match &fork_model {
            Some(model) if !model.local => false,
            Some(model) => !local.fork_fits(&self.model.model, &model.model),
            None => true,
        });
        let _waiting = asleep.map(|local| local.waiting(&self.model.model, &self.holder()));
        let answer = self.wait_for_answer(&mut pending, timeout).await;
        match answer.outcome {
            QuestionOutcome::Answered => Ok(answer.text),
            _ => Err(answer.text),
        }
    }

    /// `list_knowledge_agents`.
    pub(super) fn list_knowledge_agents(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize, Default)]
        struct Args {
            #[serde(default)]
            query: Option<String>,
        }
        let args: Args = parse(arguments)?;
        Ok(crate::knowledge::list(&self.shared, args.query.as_deref()))
    }

    /// `keep_as_knowledge` (the root's): keeps a finished subagent as a knowledge agent, or
    /// retires one.
    pub(super) fn keep_as_knowledge(&self, arguments: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Args {
            agent_id: String,
            #[serde(default)]
            topic: Option<String>,
            #[serde(default)]
            retire: bool,
        }
        let args: Args = parse(arguments)?;
        if args.retire {
            crate::knowledge::retire(&self.shared, &args.agent_id)?;
            return Ok(format!(
                "{} is an ordinary idle subagent again; it can still be resumed with resume_agent",
                args.agent_id
            ));
        }
        let topic = args
            .topic
            .ok_or("a topic is required, or retire: true to stop keeping the agent")?;
        let card = crate::knowledge::promote(&self.shared, &args.agent_id, &topic)?;
        Ok(format!(
            "{} is now a knowledge agent; ask it with ask_agent instead of reading what it read \
             again.\n{}",
            card.agent_id,
            crate::knowledge::describe(&card)
        ))
    }
}

/// A finished question task as an answer; a crashed task is a failed question.
fn joined_answer(joined: Result<Answer, tokio::task::JoinError>) -> Answer {
    joined.unwrap_or_else(|error| Answer {
        outcome: QuestionOutcome::Failed,
        text: format!("the question's task crashed: {error}"),
        plain: format!("the question's task crashed: {error}"),
        degraded: false,
        untrusted_web: false,
        untrusted_tools: false,
    })
}
