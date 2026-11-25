//! One agent's conversation and its turn loop, including planning and delegation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use imperium_api::{ChatMessage, ToolCall, ToolSpec, Usage};
use imperium_api::{
    Effort, Load, LocalRequest, Priority, Sampler, SamplingFuture, SamplingReply, SamplingRequest,
    ToolContext, ToolEvent, ToolGroup, ToolProvider, parse_arguments as parse,
    truncate_chars as truncate,
};
use imperium_api::{PermissionMode, PermissionRule, RuleAction};
use imperium_config::{Location, Tier};
use imperium_models::{ChatClient, ChatRequest, Endpoint};
use imperium_policy::permissions::{CallKind, Decision, Judgement, Refusal, Target, suggest};
use imperium_policy::{SHELL_TOOLS, ToolSource};
use imperium_protocol::patterns;
use imperium_protocol::session::{
    AgentEvent, AgentStatus, Attachment, ContextItem, ContextPointer, ExtensionKind, PauseCause,
    PermissionDecision, Plan, PlanStep, Question, ROOT_AGENT, SessionEvent, StepStatus, TokenUsage,
    ToolLoadVia,
};
use imperium_protocol::tools as contract;
use imperium_tools::{Access, Invocation, ToolError, Workspace, specs};
use imperium_workflow::LoadManifest;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

use imperium_config::{Enforce, HarnessMode, HarnessSettings, WritePolicy};
use imperium_extensions::{Activation, HookEvent, Verdict};
use imperium_memory::{NewEntry, Scope};

use imperium_protocol::methods::{Compaction, ContextFit};

mod attachments;
mod auto;
mod background;
mod catalog;
mod compaction;
mod delegation;
mod fork;
mod memory;
mod prompts;
mod sampling;
mod specs;
mod tools;
mod turn;

pub(crate) use auto::AutoGuard;
use auto::{AutoGate, builtin_input, provider_input};
pub(crate) use catalog::effective_call;
use catalog::*;
#[cfg(test)]
pub(crate) use catalog::{test_call, test_proxied};
use delegation::*;
pub(crate) use delegation::{cancel_subagent, resume_subagent};
pub(crate) use fork::run_fork;
use memory::*;
use prompts::*;
use sampling::*;
use specs::*;
pub(crate) use specs::{
    memory_edit_spec, memory_forget_spec, memory_read_spec, memory_remember_spec,
    memory_search_spec,
};
pub(crate) use tools::shown;
use tools::*;
pub(crate) use turn::effective_window;
use turn::*;

use crate::agents::{AgentKind, Frozen, FrozenTool, NamedAgent, task_number};
use crate::artefacts::{self, handle_message};
use crate::breakdown::{
    Call, Category, LOAD_SKILL, Measured, Request, Spans, TokenCounter, estimate_tokens,
    join_parts, tool_texts,
};
use crate::compact::{self, SwitchError};
use crate::harness::{self, SessionState, StateUpdate};
use crate::instructions;
use crate::queue::{Inbox, QueueError, steering_text};
use crate::session::{SessionMemory, Shared, lock};
use crate::spill;
use crate::{ResolvedModel, TaskRequest, escalate, route, route_noted};
use imperium_api::{LocalLease, LocalModels, LocalPresence};

/// Longest tool output or report shown in events (the model receives the full text).
const EVENT_OUTPUT_CHARS: usize = 2_000;
/// Longest tool-call arguments or output shown in events; the transcript collapses each call
/// and folds longer text behind "show more".
const EVENT_TOOL_CHARS: usize = 8_000;
/// Longest subagent report handed back to the parent.
const REPORT_CHARS: usize = 8_000;
/// Share of the root's context window, in percent, above which an input is stored as an
/// artefact instead of entering the conversation.
const ARTEFACT_THRESHOLD_PERCENT: u64 = 75;
/// Most chunks one `delegate_chunks` call may make; a larger input needs larger chunks.
const MAX_CHUNKS: usize = 64;
/// Most chunk agents one session may start across all its `delegate_chunks` calls.
const MAX_CHUNK_AGENTS: usize = 256;
/// Share of a chunk model's window, in percent, that one chunk may take.
const CHUNK_WINDOW_PERCENT: u64 = 60;
/// The window assumed when none is known, in tokens.
const DEFAULT_WINDOW_TOKENS: u64 = 32_768;
/// How long `ask_user` waits for an answer before taking the recommendations.
const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
/// Most options a `request_clarification` shows.
const MAX_CLARIFICATION_OPTIONS: usize = 5;
/// Most ambiguity patterns whose guidance goes at the top of one turn.
const MAX_AMBIGUITY_HINTS: usize = 2;
/// Upper limits on the rewrite call, whatever `[harness]` says: tokens it may write, seconds it
/// may take, and characters of history it reads.
const MAX_REWRITE_TOKENS: u32 = 2_000;
const MAX_REWRITE_SECONDS: u64 = 120;
const MAX_REWRITE_HISTORY_CHARS: usize = 20_000;
/// How often a call waiting for a local model looks at why it waits.
const WAIT_REASON_POLL: std::time::Duration = std::time::Duration::from_millis(500);
/// What a turn produced, for a parent awaiting a subagent.
pub(crate) struct TurnOutcome {
    pub text: String,
    pub ok: bool,
    /// The turn stopped early because the user steered the root agent; `text` says how far
    /// it got.
    pub paused: bool,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
}
pub(crate) struct TaskReport {
    task_id: String,
    model: String,
    ok: bool,
    /// Paused for a steering message to the root: `report` is its progress so far.
    paused: bool,
    report: String,
}

impl TaskReport {
    /// The report as its parent reads it.
    fn render(&self, cap: usize) -> String {
        let state = if self.paused {
            "paused"
        } else if self.ok {
            "finished"
        } else {
            "failed"
        };
        format!(
            "## {} ({}, {state})\n{}",
            self.task_id,
            self.model,
            clip(&self.report, cap)
        )
    }
}

pub(crate) struct Agent {
    id: String,
    shared: Arc<Shared>,
    /// The folders, grants and sandbox this agent's tools work in: the session's, or for a
    /// writer subagent the same with its own worktrees as folders.
    workspace: Workspace,
    /// A writer subagent working in worktrees of its own: its changes are committed on their
    /// branch when it stops working.
    writer: bool,
    model: ResolvedModel,
    messages: Vec<ChatMessage>,
    /// The labelled parts of messages this agent built (its system prompt, user messages with
    /// a preamble), by message index, for context breakdowns.
    spans: HashMap<usize, Spans>,
    /// Only the root agent delegates; subagents do their brief themselves.
    can_delegate: bool,
    /// Messages waiting for this agent: steering ones join its running turn.
    inbox: Arc<Inbox>,
    tasks: HashMap<String, JoinHandle<TaskReport>>,
    /// Tasks in `tasks` that pause when the user steers this (root) agent: subagents it
    /// spawned or resumed, not chunk and reduce agents.
    pausable_tasks: HashSet<String>,
    /// For a subagent that pauses when the user steers the root agent: the root's steering
    /// signal, as it was when this subagent started.
    pause: Option<watch::Receiver<u64>>,
    /// The workflow role whose files this agent loads: `root`, or what the delegator asked.
    role: String,
    /// Required references from the latest manifest that are not loaded yet.
    unloaded: Mutex<Vec<String>>,
    /// The latest local call's exact prompt plus completion tokens, and the number of messages
    /// (its reply included) they cover; the next call's prompt estimate starts from it.
    last_call: Option<(u64, usize)>,
    /// The latest model call that reported its prompt size: the messages it read, and that
    /// size, so measurements without a tokenizer can be calibrated.
    last_reported: Option<(usize, u64)>,
    /// A single-call task: no tools, no recalled context, and exactly one model call.
    single_call: bool,
    /// Set when the model was switched (or first given) and the conversation has not been
    /// measured against it yet: that happens when the next turn starts, so selecting a model
    /// never loads it.
    fit_pending: bool,
    /// While a fit is pending after a switch: the model that read the conversation last, which
    /// writes a compaction's summary when it can.
    previous: Option<ResolvedModel>,
    /// What the next `turn_started` says instead of the text, for chunk and reduce agents
    /// whose real brief holds chunk text that must stay out of the journal and history.
    shown: Option<String>,
    /// For a chunk or reduce agent: the brief its parent wrote (without chunk text) and which
    /// agent of the call it is, for an evaluation harness that answers subagents from recordings.
    replay_key: Option<(String, crate::ReplayRole)>,
    /// The reasoning effort this subagent was spawned with, over the session's.
    effort: Option<Effort>,
    /// How many messages the conversation had when mid-turn compaction last ran or was last
    /// tried: another needs at least two more (a reply and a result), so it cannot loop.
    compact_mark: Option<usize>,
    /// Mid-turn compaction made no progress this turn; it is not tried again until the next.
    compact_stalled: bool,
    /// The notice that mid-turn compaction was skipped as not worth it has been given.
    compact_skip_noticed: bool,
    /// The instruction files the conversation holds; read from it on first use.
    instructions: Option<instructions::Tracker>,
    /// Whether the latest tool call succeeded (only successful file calls load instruction
    /// files).
    last_tool_ok: std::sync::atomic::AtomicBool,
    /// Set while a call from the MCP bridge runs: this agent's permission requests then say so
    /// in their summary, so clients that must not answer them (the harness) can tell.
    bridged: std::sync::atomic::AtomicBool,
    /// Set by a `tool.before` hook that rewrote the call in flight: the new arguments, for
    /// `run_tool` to check as a new call.
    pending_rewrite: Mutex<Option<String>>,
    /// While a rewritten call runs: the arguments the model gave, as the user is shown them.
    rewritten_from: Mutex<Option<String>>,
    /// The arguments the latest call ran with when a hook rewrote them.
    ran_rewritten: Mutex<Option<String>>,
    /// The root agent's structured state (see [`harness`]); read back from the conversation
    /// when it is restored.
    state: SessionState,
    /// The harness paragraph in the system prompt, for the model the agent has now: it is
    /// rewritten when the model is switched, so the prompt and the tools offered always agree.
    harness_text: String,
    /// The loader sentence in the system prompt while reactive tool loading is on (its own
    /// swappable region, beside `harness_text`).
    loader_text: String,
    /// May ask knowledge agents questions: the root and ordinary subagents, never a
    /// `single_call` or chunk agent, a knowledge agent, or a fork (docs/21-knowledge-agents.md).
    can_ask: bool,
    /// What kind of agent this is, for promotion to a knowledge agent.
    kind: AgentKind,
    /// Questions this agent asked in its current turn.
    asked_this_turn: u32,
    /// The tools offered on this agent's latest turn, kept for promotion.
    last_tools: Vec<ToolSpec>,
    /// Set for the ephemeral fork that answers a question: it only reads, never asks, and
    /// leaves no record.
    fork: Option<fork::ForkInfo>,
    /// The agent file this subagent runs as: its tools are offered and run only from the file's
    /// list (never more than the delegator had); the profile, the rules and the safety floor
    /// still apply to every call.
    named: Option<NamedAgent>,
    /// Reactive tool loading (docs/plans/reactive-tools.md): whether the agent's deferred
    /// tools are numerous enough to be worth a loader; decided once.
    loader_worth: std::sync::OnceLock<bool>,
    /// Whether `activate_plugin` stays offered for the whole session; decided once.
    plugin_loader: std::sync::OnceLock<bool>,
    /// The fixed description of `tool_search`; written once.
    search_text: std::sync::OnceLock<String>,
    /// The tools whose parameters this agent has seen in a `tool_search` result or that it
    /// ran through `call_tool`.
    loaded_tools: Mutex<Vec<String>>,
    /// The tools whose parameters the model has seen first-hand since the last compaction
    /// (searched, run through `call_tool`, or already advised): schema advice is given once
    /// per tool, and again after a compaction removed the earlier result.
    schema_seen: Mutex<HashSet<String>>,
    /// The tools already reported in a `tools_loaded` event (once per tool and agent).
    reported_tools: Mutex<HashSet<String>>,
    /// Whether the call in flight got as far as running (it started), and whether its
    /// arguments were judged and rejected before that; both are false for a call a refusal
    /// stopped. Set by [`Agent::tool_started`] and the argument checks, read by
    /// `with_schema_advice`.
    call_started: std::sync::atomic::AtomicBool,
    call_args_failed: std::sync::atomic::AtomicBool,
}

impl Agent {
    pub(crate) fn new(
        id: String,
        shared: Arc<Shared>,
        model: ResolvedModel,
        can_delegate: bool,
        role: String,
    ) -> Self {
        let workspace = shared.workspace.clone();
        Self::new_in(id, shared, model, can_delegate, role, workspace, None)
    }

    /// An agent whose tools work in `workspace` (a writer subagent's own worktrees) instead of
    /// the session's.
    pub(crate) fn new_in(
        id: String,
        shared: Arc<Shared>,
        model: ResolvedModel,
        can_delegate: bool,
        role: String,
        workspace: Workspace,
        writer_note: Option<&str>,
    ) -> Self {
        // Its background commands are reported to it.
        let workspace = workspace.with_owner(&id);
        let root = workspace.root_display();
        let mut system = if can_delegate {
            root_prompt(&root, shared.knowledge.enabled())
        } else {
            subagent_prompt(&root)
        };
        // An environment's further folders are also open to the tools, by absolute path.
        let others = workspace.folders_display().split_off(1);
        if !others.is_empty() {
            system.push_str(&format!(
                " The environment also includes these folders, which the tools accept by \
                 absolute path: {}.",
                others.join(", ")
            ));
        }
        if let Some(note) = writer_note {
            system.push(' ');
            system.push_str(note);
        }
        // The project's other git worktrees, open like its folders.
        let worktrees: Vec<String> = workspace
            .project_worktrees()
            .iter()
            .map(|tree| imperium_tools::plain_path(tree))
            .collect();
        if !worktrees.is_empty() {
            system.push_str(&format!(
                " The project's git worktrees are open to the tools too, by absolute path: {}. \
                 Worktrees added later with `git worktree add` are open as well.",
                worktrees.join(", ")
            ));
        }
        // The session's own scratch folder, kept apart from the environment's folders.
        if let Some(scratch) = workspace.scratch() {
            system.push_str(&format!(
                " Your scratch folder is {}: it is yours alone, writable, and deleted when this \
                 session is archived. Put temporary files, notes to yourself and generated \
                 intermediates there rather than in the environment's folders; anything meant to \
                 last belongs in the folders or in memory.",
                imperium_tools::plain_path(scratch)
            ));
        }
        // Paths outside the environment that grants open (package caches, toolchains). The
        // folder of saved tool results is named where a result is saved, not here: its path
        // is the session's own, and would make every session's prompt differ.
        let results = shared
            .spill_dir
            .as_ref()
            .and_then(|dir| std::fs::canonicalize(dir).ok());
        let grants: Vec<String> = workspace
            .grants()
            .iter()
            .filter(|grant| workspace.scratch() != Some(grant.path.as_path()))
            .filter(|grant| results.as_deref() != Some(grant.path.as_path()))
            .map(|grant| grant.describe())
            .collect();
        if !grants.is_empty() {
            system.push_str(&format!(
                " These paths outside the environment are also open to the tools, by absolute \
                 path: {}.",
                grants.join(", ")
            ));
        }
        // How the root agent answers the user; it closes the base instruction. The personality
        // follows the reply rules, so it sets the tone without replacing them.
        let mut harness_text = String::new();
        if can_delegate {
            system.push_str("\n\n");
            system.push_str(RESPONSE_STYLE);
            harness_text = harness_prompt(&shared.harness, model.local, is_attended(&shared));
            if !harness_text.is_empty() {
                system.push_str("\n\n");
                system.push_str(&harness_text);
            }
            if let Some(personality) = &shared.personality {
                system.push_str("\n\n");
                system.push_str(personality);
            }
        }
        // Each part is kept with its label, so context breakdowns count them separately.
        let mut parts = vec![(Category::SystemBase, system)];
        // Skills and plugins load on use: only their index is here, plus eager skills.
        if let Some(index) = shared.extensions.index() {
            parts.push((Category::SystemSkills, index));
        }
        // Only the agent that delegates needs the named agents' list.
        if can_delegate && let Some(agents) = shared.extensions.agents_index() {
            parts.push((Category::SystemSkills, agents));
        }
        if let Some(eager) =
            imperium_extensions::skills::eager_instructions(&shared.extensions.skills)
        {
            parts.push((Category::SkillsLoaded, eager));
        }
        if let Some(memory) = &shared.memory {
            parts.push((
                Category::SystemMemory,
                memory_instructions(memory, can_delegate),
            ));
        }
        // Tool providers' rules for their tools (the web tools' citation rules). An agent
        // limited to local files is offered none of the integrations, so none of their rules.
        for provider in &shared.tools {
            if !can_delegate
                && (!provider.for_subagents() || (writer_note.is_some() && !provider.for_writers()))
            {
                continue;
            }
            if provider.group() == ToolGroup::Builtin
                && !shared.workspace.profile().allows_provider_tools()
            {
                continue;
            }
            if let Some(instructions) = provider.instructions() {
                parts.push((Category::SystemCitations, instructions.to_string()));
            }
        }
        if can_delegate && let Some(instructions) = &shared.instructions {
            parts.push((Category::SystemProject, instructions.clone()));
        }
        let (system, spans) = join_parts(parts);
        let inbox = shared.inboxes.open(&id);
        shared.agents.register(&id, &role, &model);
        Agent {
            spans: HashMap::from([(0, spans)]),
            can_delegate,
            role,
            harness_text,
            ..Agent::bare(
                id,
                inbox,
                shared,
                workspace,
                model,
                vec![ChatMessage::System { content: system }],
            )
        }
    }

    /// An agent with nothing of its own yet: the given conversation and no tasks, plan, state
    /// or record (the caller registers one when the agent is a subagent of the session).
    fn bare(
        id: String,
        inbox: Arc<Inbox>,
        shared: Arc<Shared>,
        workspace: Workspace,
        model: ResolvedModel,
        messages: Vec<ChatMessage>,
    ) -> Self {
        Agent {
            id,
            inbox,
            shared,
            workspace,
            writer: false,
            model,
            messages,
            spans: HashMap::new(),
            can_delegate: false,
            tasks: HashMap::new(),
            pausable_tasks: HashSet::new(),
            pause: None,
            role: "leaf".into(),
            unloaded: Mutex::new(Vec::new()),
            last_call: None,
            last_reported: None,
            single_call: false,
            fit_pending: false,
            previous: None,
            shown: None,
            replay_key: None,
            effort: None,
            compact_mark: None,
            compact_stalled: false,
            compact_skip_noticed: false,
            instructions: None,
            last_tool_ok: std::sync::atomic::AtomicBool::new(false),
            bridged: std::sync::atomic::AtomicBool::new(false),
            pending_rewrite: Mutex::new(None),
            rewritten_from: Mutex::new(None),
            ran_rewritten: Mutex::new(None),
            state: SessionState::default(),
            harness_text: String::new(),
            loader_text: String::new(),
            can_ask: true,
            kind: AgentKind::Ordinary,
            asked_this_turn: 0,
            last_tools: Vec::new(),
            fork: None,
            named: None,
            loader_worth: std::sync::OnceLock::new(),
            plugin_loader: std::sync::OnceLock::new(),
            search_text: std::sync::OnceLock::new(),
            loaded_tools: Mutex::new(Vec::new()),
            schema_seen: Mutex::new(HashSet::new()),
            reported_tools: Mutex::new(HashSet::new()),
            call_started: std::sync::atomic::AtomicBool::new(false),
            call_args_failed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Runs this agent as the named agent `named`: its instructions follow the base ones, and
    /// when its file listed tools only those are offered and run. Nothing here grants a
    /// permission: every call still goes through the profile, the rules and the safety floor.
    pub(crate) fn with_named(mut self, named: NamedAgent) -> Self {
        if let Some(ChatMessage::System { content }) = self.messages.first_mut() {
            content.push_str(crate::breakdown::PART_SEPARATOR);
            let start = content.len();
            // A root agent takes its task from the user, a subagent from the agent that started it.
            let source = if self.can_delegate {
                "the user"
            } else {
                "the agent that started you"
            };
            content.push_str(&format!(
                "You are running as the agent {}. Its definition follows; your task comes from \
                 {source}.\n\n{}",
                named.name, named.prompt
            ));
            let end = content.len();
            self.spans
                .entry(0)
                .or_default()
                .push((Category::SystemBase, start..end));
        }
        self.named = Some(named);
        self
    }

    /// Runs this root agent as the named agent `def` (an evaluation harness running an agent
    /// file as the root): its instructions follow the base ones and only the tools its file
    /// lists are offered, none when the file lists `tools` with nothing in it. Returns the
    /// agent and notes for the user.
    pub(crate) fn into_named_root(
        self,
        def: &imperium_extensions::AgentDef,
    ) -> (Self, Vec<String>) {
        // The agent's file narrows what the agent has (its universe), not what it is offered
        // first: with reactive loading on, that is only the core.
        let offered: Vec<String> = self
            .tool_universe()
            .0
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        let (mut named, notes) = delegation::named_agent(def, &offered);
        if def.no_tools {
            named.tools = Some(Vec::new());
            return (self.with_named(named), Vec::new());
        }
        (self.with_named(named), notes)
    }

    /// Marks this agent as a writer in worktrees of its own (see [`Agent::writer`]).
    fn writing(mut self) -> Self {
        self.writer = true;
        self
    }

    /// Thinks at `effort` instead of the session's effort, when its model can reason.
    pub(crate) fn with_effort(mut self, effort: Option<Effort>) -> Self {
        self.effort = effort;
        self
    }

    /// The effort this agent's calls carry: its own or the session's, when the model can
    /// reason; `None` (nothing sent) when it cannot.
    fn request_effort(&self) -> Option<Effort> {
        self.model
            .reasons(self.shared.local.as_deref())
            .then(|| self.effort.unwrap_or_else(|| self.shared.effort.get()))
    }

    /// The model this agent calls.
    pub(crate) fn model(&self) -> &ResolvedModel {
        &self.model
    }

    /// Messages waiting for this agent.
    pub(crate) fn inbox(&self) -> &Arc<Inbox> {
        &self.inbox
    }
    /// What the root reads, after a steering message, about the subagents that stopped for it.
    fn stopped_status(&self, stopped: &[TaskReport]) -> String {
        if stopped.is_empty() {
            return String::new();
        }
        let agents = self.shared.agents.list();
        let title = |id: &str| {
            agents
                .iter()
                .find(|agent| agent.agent_id == id)
                .map(|agent| agent.title.clone())
                .unwrap_or_default()
        };
        let lines: Vec<String> = stopped
            .iter()
            .map(|report| {
                let state = if report.paused {
                    "paused"
                } else if report.ok {
                    "finished"
                } else {
                    "failed"
                };
                format!(
                    "- {} ({state}; {}): {}\n  {}",
                    report.task_id,
                    report.model,
                    title(&report.task_id),
                    truncate(&report.report, REPORT_CHARS).replace('\n', "\n  ")
                )
            })
            .collect();
        format!(
            "\n\nThe subagents you were running stopped for this message:\n{}\n\nA paused \
             subagent keeps its conversation. resume_agent continues it (its text can re-brief \
             it in light of this message), and resume_agent with cancel: true ends it. If you \
             decide nothing, paused subagents stay paused for the user.",
            lines.join("\n")
        )
    }
    /// Makes this subagent pause when the user steers the root agent from now on.
    fn pausable(mut self) -> Self {
        self.pause = Some(self.shared.inboxes.open(ROOT_AGENT).steering_signal());
        self
    }

    /// Whether the user steered the root agent since this (pausable) subagent started.
    fn pause_requested(&self) -> bool {
        self.pause
            .as_ref()
            .is_some_and(|pause| pause.has_changed().unwrap_or(false))
    }
    /// Makes this a single-call agent: it is given the brief and no tools, makes exactly one
    /// model call, and that reply is its report.
    pub(crate) fn single_call(mut self) -> Self {
        let root = self.workspace.root_display();
        let system = single_call_prompt(&root);
        let (system, spans) = join_parts(vec![(Category::SystemBase, system)]);
        self.messages = vec![ChatMessage::System { content: system }];
        self.spans = HashMap::from([(0, spans)]);
        self.single_call = true;
        self.can_ask = false;
        self.kind = AgentKind::SingleCall;
        self
    }

    /// The conversation so far.
    pub(crate) fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Continues an earlier conversation: its messages after the instructions, which stay
    /// this agent's current ones.
    pub(crate) fn restore(&mut self, messages: Vec<ChatMessage>) {
        let earlier = messages
            .into_iter()
            .skip_while(|message| matches!(message, ChatMessage::System { .. }));
        self.messages.truncate(1);
        self.messages.extend(earlier);
        self.spans.retain(|index, _| *index == 0);
        self.last_reported = None;
        self.last_call = None;
        if self.can_delegate {
            self.state = harness::latest_state(&self.messages);
        }
    }

    fn emit(&self, event: SessionEvent) {
        // A question's fork shows its tool calls and nothing else of its turn.
        if self.fork.is_some() && !fork::forwarded(&event) {
            return;
        }
        self.shared.emitter.emit(&self.id, event);
    }

    /// Saves a subagent's conversation, so it can be resumed later (after a restart too). The
    /// root's conversation is saved by its session after every turn. What its request began
    /// with (the system message and the tools offered) is noted too, for promotion to a
    /// knowledge agent.
    fn checkpoint(&self) {
        if !self.can_delegate && self.fork.is_none() {
            if let Some(ChatMessage::System { content }) = self.messages.first() {
                self.shared.agents.note_run(
                    &self.id,
                    self.kind,
                    Frozen {
                        system: content.clone(),
                        tools: self.last_tools.iter().map(FrozenTool::of).collect(),
                        effort: self
                            .request_effort()
                            .map(|effort| effort.as_str().to_string()),
                    },
                );
            }
            self.shared
                .agents
                .save_conversation(&self.id, &self.messages);
        }
    }
    /// This agent's name to a local model.
    fn holder(&self) -> String {
        // A question's fork works under its knowledge agent's name, so a local model gives it
        // the slot whose cached prompt is the agent's conversation.
        let name = self.fork.as_ref().map_or(&self.id, |fork| &fork.source);
        imperium_api::holder(&self.shared.session_id, name)
    }
    /// The local runtime, when this agent's model runs on it.
    fn local_models(&self) -> Option<&dyn LocalModels> {
        self.shared.local.as_deref().filter(|_| self.model.local)
    }
    /// Runs a tool call an outside caller sent through the MCP bridge (`imperium mcp-serve
    /// --tools`), between turns. It is dispatched exactly like a call the model makes: every
    /// check, the user's approval and the journal apply. Returns whether it succeeded and the
    /// full output.
    pub(crate) async fn run_bridged(
        &mut self,
        id: &str,
        name: &str,
        arguments: &str,
    ) -> (bool, String) {
        let call: ToolCall = match serde_json::from_value(json!({
            "id": id,
            "type": "function",
            "function": {"name": name, "arguments": arguments},
        })) {
            Ok(call) => call,
            Err(error) => return (false, format!("error: {error}")),
        };
        self.last_tool_ok.store(false, Ordering::SeqCst);
        self.bridged.store(true, Ordering::SeqCst);
        let output = self.run_tool(&call).await;
        self.bridged.store(false, Ordering::SeqCst);
        (self.last_tool_ok.load(Ordering::SeqCst), output)
    }

    /// Ends the marking of permission requests as bridged (a bridged call that was cut short
    /// leaves it set).
    pub(crate) fn end_bridged(&self) {
        self.bridged.store(false, Ordering::SeqCst);
    }
    /// Whether a person reads this session's turns; see [`is_attended`].
    fn attended(&self) -> bool {
        is_attended(&self.shared)
    }

    /// Whether the harness aid set to `mode` applies to this agent now: the root agent of an
    /// attended session, on a model `mode` turns it on for.
    fn harness_on(&self, mode: HarnessMode) -> bool {
        self.can_delegate && !self.single_call && self.attended() && mode_on(mode, self.model.local)
    }

    /// Whether the budget lets a call to `spec` go without asking the user. The rewrite never
    /// raises a budget prompt of its own: it is skipped instead.
    fn budget_allows(&self, spec: &str) -> bool {
        self.shared
            .budget_gate
            .as_ref()
            .is_none_or(|gate| matches!(gate.check(spec), crate::BudgetVerdict::Go))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_streamed_window_is_shown_and_recorded() {
        let (shown, recorded) =
            generation_speed(40, Duration::from_secs(5), Some(Duration::from_secs(2)));
        assert_eq!(shown, Some(20.0));
        assert_eq!(recorded, Some(Duration::from_secs(2)));
    }

    #[test]
    fn a_short_reply_is_shown_over_the_whole_call_but_not_recorded() {
        let (shown, recorded) =
            generation_speed(2, Duration::from_secs(10), Some(Duration::from_millis(50)));
        assert_eq!(shown, Some(0.2));
        assert_eq!(recorded, None);
        let (shown, recorded) = generation_speed(30, Duration::from_secs(3), None);
        assert_eq!(shown, Some(10.0));
        assert_eq!(recorded, None);
    }

    #[test]
    fn the_window_falls_back_to_the_reported_one_then_to_the_default() {
        assert_eq!(effective_window(Some(8_000), false, Some(4_096)), 8_000);
        assert_eq!(effective_window(Some(8_000), true, Some(4_096)), 4_096);
        assert_eq!(effective_window(None, false, Some(16_384)), 16_384);
        assert_eq!(effective_window(None, false, None), DEFAULT_WINDOW_TOKENS);
        assert_eq!(effective_window(Some(8_000), true, None), 8_000);
    }

    #[test]
    fn clipped_reports_say_so() {
        assert_eq!(clip("short", 10), "short");
        assert!(clip(&"x".repeat(50), 10).ends_with("truncated to fit the merge window]"));
    }

    #[test]
    fn failed_or_unsure_reports_call_for_a_stronger_route() {
        let report = |ok, paused, text: &str| TaskReport {
            task_id: "t1".into(),
            model: "m".into(),
            ok,
            paused,
            report: text.into(),
        };
        assert!(needs_escalation(&report(false, false, "it broke")));
        assert!(needs_escalation(&report(
            true,
            false,
            "CONFIDENCE: LOW\nnot sure"
        )));
        assert!(!needs_escalation(&report(
            true,
            false,
            "confidence: low\nnot sure"
        )));
        assert!(!needs_escalation(&report(true, false, "done")));
        assert!(!needs_escalation(&report(
            true,
            false,
            "CONFIDENCE: LOW is a phrase"
        )));
        assert!(!needs_escalation(&report(
            true,
            false,
            "all fine\nCONFIDENCE: LOW"
        )));
        // A pause is the user steering, not a failure.
        assert!(!needs_escalation(&report(false, true, "paused")));
    }

    /// The delegation and planning tools sent to models are the protocol's exported definitions.
    #[test]
    fn specs_are_the_protocol_definitions() {
        for (spec, definition) in [
            (update_plan_spec(), contract::update_plan()),
            (spawn_agent_spec(), contract::spawn_agent()),
            (await_tasks_spec(), contract::await_tasks()),
            (steer_agent_spec(), contract::steer_agent()),
            (queue_agent_spec(), contract::queue_agent()),
            (list_agents_spec(), contract::list_agents()),
            (resume_agent_spec(), contract::resume_agent()),
            (keep_as_knowledge_spec(), contract::keep_as_knowledge()),
            (ask_agent_spec(), contract::ask_agent()),
            (
                list_knowledge_agents_spec(),
                contract::list_knowledge_agents(),
            ),
            (delegate_chunks_spec(), contract::delegate_chunks()),
            (ask_user_spec(), contract::ask_user()),
            (
                request_clarification_spec(),
                contract::request_clarification(),
            ),
            (update_state_spec(), contract::update_state()),
        ] {
            assert_eq!(spec.name, definition.name);
            assert_eq!(spec.description, definition.description);
            assert_eq!(spec.parameters, definition.parameters);
        }
    }
}

#[cfg(test)]
mod single_call_tests {
    use super::single_call_prompt;

    #[test]
    fn the_single_call_prompt_has_no_runs_of_spaces() {
        let prompt = single_call_prompt("/work");
        assert!(!prompt.contains("  "), "{prompt}");
        assert!(prompt.contains("/work") && prompt.contains("subagent"));
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::{KNOWLEDGE_SOURCE, PROVERB, RESPONSE_STYLE, root_prompt, subagent_prompt};

    /// The rules of roadmap item 19 stay in the root prompt: change them on purpose, here too.
    #[test]
    fn the_root_prompt_keeps_the_response_style() {
        for rule in [
            "lead with the result",
            "the big picture first, more detail only when the user asks",
            "headings, lists or tables only when the content has that shape",
            "Avoid emojis",
            "Show code only when the user asks to see it",
            "say plainly what you did not verify",
        ] {
            assert!(RESPONSE_STYLE.contains(rule), "{rule} missing");
        }
        let root = root_prompt("/work", true);
        for rule in [
            "Act through the tools",
            "rather than pasting code for the user to apply",
            "instead of relying on what you learned in training",
            "web_search",
            "search_sessions",
            "memory_search",
            "list_models",
        ] {
            assert!(root.contains(rule), "{rule} missing");
        }
    }

    /// The tool contracts the root prompt names survive rewording.
    #[test]
    fn the_root_prompt_names_its_tools() {
        let root = root_prompt("/work", true);
        for tool in [
            "update_plan",
            "spawn_agent",
            "await_tasks",
            "steer_agent",
            "queue_agent",
            "delegate_chunks",
            "ask_user",
            "<steering>",
        ] {
            assert!(root.contains(tool), "{tool} missing");
        }
    }

    #[test]
    fn the_root_prompt_mentions_knowledge_agents_only_while_they_are_on() {
        let on = root_prompt("/work", true);
        let off = root_prompt("/work", false);
        assert!(on.contains("keep_as_knowledge") && on.contains("ask_agent"));
        assert!(!off.contains("keep_as_knowledge") && !off.contains("ask_agent"));
        // The proverb says "knowledge" too; the check is about knowledge agents.
        assert!(!off.replace(PROVERB, "").contains("knowledge"));
        // The rest of the prompt is the same either way.
        assert!(on.starts_with(&off));
        assert!(!off.contains("  ") && !on.contains("  "));
    }

    #[test]
    fn the_prompts_carry_the_knowledge_source_rule() {
        assert!(!KNOWLEDGE_SOURCE.contains("knowledge"));
        assert!(KNOWLEDGE_SOURCE.starts_with("Settled real-world facts, arithmetic"));
        for prompt in [
            root_prompt("/work", false),
            root_prompt("/work", true),
            subagent_prompt("/work"),
        ] {
            assert_eq!(prompt.matches(KNOWLEDGE_SOURCE).count(), 1);
            assert!(!prompt.contains("  "));
        }
        // It follows each prompt's look-up sentence.
        let root = root_prompt("/work", false);
        let look_up = root.find("you learned in training.").unwrap();
        assert!(root.find(KNOWLEDGE_SOURCE).unwrap() > look_up);
        let subagent = subagent_prompt("/work");
        let look_up = subagent.find("you learned in training.").unwrap();
        assert!(subagent.find(KNOWLEDGE_SOURCE).unwrap() > look_up);
    }

    #[test]
    fn the_prompts_teach_knowledge_agents() {
        let root = root_prompt("/work", true);
        for text in [
            "keep_as_knowledge",
            "ask_agent",
            "knowledge topic",
            "self-contained",
        ] {
            assert!(root.contains(text), "{text} missing from the root prompt");
        }
        let subagent = subagent_prompt("/work");
        for text in [
            "ask_agent",
            "list_knowledge_agents",
            "before you read the same material",
        ] {
            assert!(
                subagent.contains(text),
                "{text} missing from the subagent prompt"
            );
        }
        assert!(!root.contains("  ") && !subagent.contains("  "));
    }

    /// The Emperor root prompt the training repository vendors carries the engine's proverb,
    /// word for word, on a line of its own near the top.
    #[test]
    fn the_emperor_root_prompt_mirrors_the_proverb() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../imperium-protocol/schemas/prompts/emperor-root.md");
        let text = std::fs::read_to_string(&path).expect("emperor-root.md");
        let line = text
            .lines()
            .position(|line| line.trim() == PROVERB)
            .expect("the proverb on a line of its own in emperor-root.md");
        assert!(line < 12, "the proverb is near the top (line {line})");
    }

    #[test]
    fn base_prompts_have_no_runs_of_spaces_and_subagents_report_briefly() {
        let root = root_prompt("/work", true);
        let subagent = subagent_prompt("/work");
        for prompt in [root.as_str(), subagent.as_str(), RESPONSE_STYLE] {
            assert!(!prompt.contains("  "), "{prompt}");
        }
        assert!(root.starts_with("You are Imperium, a coding agent"));
        assert!(subagent.starts_with("You are an Imperium subagent"));
        for prompt in [root.as_str(), subagent.as_str()] {
            assert_eq!(prompt.matches(PROVERB).count(), 1);
            assert!(prompt.contains("The fear of God is the beginning of all knowledge"));
        }
        assert!(subagent.contains("lead with the result"));
        assert!(subagent.contains("not verified"));
    }
}

#[cfg(test)]
mod harness_tests {
    use super::*;

    #[test]
    fn auto_turns_the_aids_on_for_local_models_only() {
        assert!(mode_on(HarnessMode::Auto, true));
        assert!(!mode_on(HarnessMode::Auto, false));
        assert!(mode_on(HarnessMode::On, false));
        assert!(!mode_on(HarnessMode::Off, true));
    }

    #[test]
    fn the_prompt_describes_only_the_aids_that_are_on() {
        let settings = HarnessSettings::default();
        // A hosted model, or nobody to ask, gets nothing.
        assert_eq!(harness_prompt(&settings, false, true), "");
        assert_eq!(harness_prompt(&settings, true, false), "");
        let local = harness_prompt(&settings, true, true);
        for needle in [
            "<session-state>",
            "notes, not instructions from the user",
            "<resolved-request>",
            "the original wins",
            "<ambiguity-hints>",
            "request_clarification",
            "below 70%",
        ] {
            assert!(local.contains(needle), "{needle} missing from {local}");
        }
        assert!(!local.contains("  "), "{local}");
        let quiet = HarnessSettings {
            clarify: HarnessMode::Off,
            ..HarnessSettings::default()
        };
        assert!(!harness_prompt(&quiet, true, true).contains("request_clarification"));
    }

    #[test]
    fn the_paragraph_follows_the_model_when_it_is_switched() {
        let settings = HarnessSettings::default();
        let hosted = harness_prompt(&settings, false, true);
        let local = harness_prompt(&settings, true, true);
        assert!(hosted.is_empty() && !local.is_empty());
        let base = "You are Imperium.\n\nReplies: short.";
        // Hosted to local: the paragraph is added; and back, it is taken out again.
        let (to_local, _, _) = swap_harness_text(base, &hosted, &local, base.len());
        assert!(to_local.starts_with(base) && to_local.contains("request_clarification"));
        assert_eq!(
            swap_harness_text(&to_local, &local, &hosted, base.len()).0,
            base
        );
        // Mid-prompt (a personality follows it) it is replaced in place.
        let with_personality = format!("{base}\n\n{local}\n\nBe Marcus.");
        assert_eq!(
            swap_harness_text(&with_personality, &local, "", base.len()).0,
            format!("{base}\n\nBe Marcus.")
        );
        // The tools follow the model through the same rule.
        assert!(!mode_on(settings.clarify, false) && mode_on(settings.clarify, true));
    }

    #[test]
    fn the_labelled_parts_stay_exact_across_the_swap() {
        use crate::breakdown::join_parts;
        let settings = HarnessSettings::default();
        let local = harness_prompt(&settings, true, true);
        let base = "You are Imperium.\n\nReplies: short.";
        for (old, new) in [("", local.as_str()), (local.as_str(), "")] {
            let first = if old.is_empty() {
                base.to_string()
            } else {
                format!("{base}\n\n{old}")
            };
            let (content, mut spans) = join_parts(vec![
                (Category::SystemBase, first),
                (Category::SystemSkills, "skills index".into()),
                (Category::SystemProject, "project rules".into()),
            ]);
            let end = spans[0].1.end;
            let (updated, at, delta) = swap_harness_text(&content, old, new, end);
            shift_spans(&mut spans, at, delta);
            // Every part still holds the text it was made from.
            let base_part = &updated[spans[0].1.clone()];
            assert!(base_part.starts_with(base), "{base_part}");
            assert_eq!(base_part.contains("request_clarification"), !new.is_empty());
            assert_eq!(&updated[spans[1].1.clone()], "skills index");
            assert_eq!(&updated[spans[2].1.clone()], "project rules");
        }
    }

    fn state_with_goal() -> SessionState {
        SessionState {
            goals: vec!["ship".into()],
            ..SessionState::default()
        }
    }

    #[test]
    fn a_compaction_that_took_the_state_away_gets_it_back_in_a_user_message() {
        let state = state_with_goal();
        let mut messages = vec![
            ChatMessage::System {
                content: "system".into(),
            },
            // What a compaction leaves: the summary in the first kept message.
            ChatMessage::user(
                "<conversation-summary>\nearlier\n</conversation-summary>\n\nlatest question",
            ),
        ];
        let mut spans = HashMap::new();
        reattach_state(&mut messages, &mut spans, &state);
        assert_eq!(harness::latest_state(&messages), state);
        let ChatMessage::User { content, .. } = &messages[1] else {
            panic!("a user message");
        };
        assert!(content.starts_with("<session-state>"));
        assert!(content.contains("<conversation-summary>"));
        // A state that is still there is left alone.
        let before = messages.clone();
        reattach_state(&mut messages, &mut spans, &state);
        assert_eq!(messages, before);
    }

    #[test]
    fn the_state_is_not_put_into_an_instruction_file_message() {
        let state = state_with_goal();
        let instruction =
            instructions::wrap(&[instructions::render_block("AGENTS.md", "x", false)]);
        let mut messages = vec![
            ChatMessage::System {
                content: "system".into(),
            },
            ChatMessage::user(instruction.clone()),
            ChatMessage::user("a question"),
        ];
        reattach_state(&mut messages, &mut HashMap::new(), &state);
        assert_eq!(messages[1], ChatMessage::user(instruction));
        assert_eq!(harness::latest_state(&messages), state);
    }
}
