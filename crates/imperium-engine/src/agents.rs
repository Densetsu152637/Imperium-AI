//! Agent state: a record for every agent of a session (who started it, its brief, model,
//! status, plan, usage, and a subagent's conversation) so finished subagents can be resumed with
//! a follow-up instead of starting afresh.
//!
//! Records follow the session's events (`task_spawned`, `turn_started`, `model_call_completed`,
//! `task_finished`, ...), so a journal replayed after a restart rebuilds the agent tree, and
//! subagents save their conversation at every safe point. When kept, each record is written
//! atomically to `<session>.agents/<agent-id>.json` beside the session's journal.

use imperium_api::time::unix_ms;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use imperium_api::ChatMessage;
use imperium_protocol::session::{
    AgentEvent, AgentStatus, AgentSummary, KnowledgeCard, ROOT_AGENT, SessionEvent, TokenUsage,
};
use serde::{Deserialize, Serialize};

use crate::ResolvedModel;
use crate::session::lock;

/// Longest report kept in a record.
const REPORT_CHARS: usize = 2_000;

/// One agent's state as it is kept on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    summary: AgentSummary,
    /// A subagent's conversation after its instructions (which are rebuilt when it resumes).
    /// The root's is kept in `<session>.messages.json` instead.
    #[serde(default)]
    conversation: Vec<ChatMessage>,
    /// What kind of agent it is, which decides whether it can be kept as a knowledge agent.
    #[serde(default)]
    kind: AgentKind,
    /// A knowledge agent's system message and tool specs as its last request sent them,
    /// recorded at promotion (see docs/21-knowledge-agents.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frozen: Option<Frozen>,
    /// When the agent's latest turn started, in milliseconds since the Unix epoch.
    #[serde(default)]
    turn_started_ms: u64,
}

/// What kind of agent a record describes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentKind {
    /// A subagent with the tools a subagent has (or the root).
    #[default]
    Ordinary,
    /// A `single_call` subagent: no tools, one reply.
    SingleCall,
    /// A `delegate_chunks` chunk or reduce agent.
    Chunk,
    /// A subagent spawned with `access: write`.
    Writer,
}

/// One tool as the agent was offered it; `ToolSpec` itself is not serialisable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FrozenTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl FrozenTool {
    pub(crate) fn of(spec: &imperium_api::ToolSpec) -> Self {
        FrozenTool {
            name: spec.name.clone(),
            description: spec.description.clone(),
            parameters: spec.parameters.clone(),
        }
    }

    pub(crate) fn spec(&self) -> imperium_api::ToolSpec {
        imperium_api::ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }
}

/// The part of a request that does not change between a knowledge agent's questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Frozen {
    pub system: String,
    pub tools: Vec<FrozenTool>,
    /// The reasoning effort its last request carried (`None`: none was sent).
    #[serde(default)]
    pub effort: Option<String>,
}

/// What a fork needs from a knowledge agent's record, taken under one lock.
pub(crate) struct ForkSource {
    pub frozen: Frozen,
    pub conversation: Vec<ChatMessage>,
    pub card: KnowledgeCard,
}

/// The id of an ephemeral question fork, such as `t3#q7`: no record is ever made for one.
pub(crate) fn is_fork_id(id: &str) -> bool {
    id.contains('#')
}

#[derive(Debug, Default)]
struct State {
    records: HashMap<String, Record>,
    /// Models agents were last given, to resume them on the same one. Not kept on disk (an
    /// endpoint can carry an API key); after a restart the model is found by its spec.
    models: HashMap<String, ResolvedModel>,
    /// Replaying a journal: records change in memory only.
    replaying: bool,
    /// What each agent's latest request began with (its system message and tools), kept in
    /// memory for promotion to a knowledge agent.
    prefixes: HashMap<String, Frozen>,
}

/// Every agent's state in a session.
#[derive(Debug)]
pub(crate) struct AgentStore {
    /// `<session>.agents/`, or `None` when agent state is not kept on disk.
    dir: Option<PathBuf>,
    state: Mutex<State>,
}

/// The folder a session's agent state is kept in, beside its journal.
pub fn agents_dir(journal: &Path) -> PathBuf {
    journal.with_extension("agents")
}

/// The number in a task id such as `t12`; 0 for anything else.
pub(crate) fn task_number(id: &str) -> u32 {
    id.strip_prefix('t')
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

/// Agent ids are `root` or `t<n>`; anything else is not used as a file name.
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn add(total: &mut TokenUsage, usage: &TokenUsage) {
    total.prompt_tokens += usage.prompt_tokens;
    total.completion_tokens += usage.completion_tokens;
    total.cached_tokens += usage.cached_tokens;
}

fn new_record(id: &str, parent: Option<String>, role: &str, task: &str, model: &str) -> Record {
    Record {
        summary: AgentSummary {
            agent_id: id.to_string(),
            parent,
            role: role.to_string(),
            title: if task.trim().is_empty() {
                String::new()
            } else {
                crate::title::task_title(None, task)
            },
            task: task.to_string(),
            model: model.to_string(),
            local: false,
            status: AgentStatus::Idle,
            resumable: false,
            turns: 0,
            messages: 0,
            usage: TokenUsage::default(),
            cost_usd: None,
            plan: None,
            last_report: None,
            updated_at_ms: unix_ms(),
            knowledge: None,
        },
        conversation: Vec::new(),
        kind: AgentKind::Ordinary,
        frozen: None,
        turn_started_ms: 0,
    }
}

impl AgentStore {
    /// A store writing to `dir` (when given), with the agents `history` describes and, for a
    /// resumed session (`load`), the records kept in `dir`. Kept records win over the journal:
    /// they hold the conversations, and are written at least as often.
    #[cfg(test)]
    pub fn open(dir: Option<PathBuf>, history: &[AgentEvent], load: bool) -> Self {
        let timed: Vec<(u64, AgentEvent)> =
            history.iter().map(|event| (0, event.clone())).collect();
        Self::open_timed(dir, &timed, load)
    }

    /// [`AgentStore::open`] with the time each event was journalled (0: unknown), so a turn
    /// that is replayed is dated by when it happened rather than when it was replayed.
    pub(crate) fn open_timed(
        dir: Option<PathBuf>,
        history: &[(u64, AgentEvent)],
        load: bool,
    ) -> Self {
        let store = AgentStore {
            dir,
            state: Mutex::default(),
        };
        lock(&store.state).replaying = true;
        for (at_ms, event) in history {
            store.observe_at(event, *at_ms);
        }
        let mut state = lock(&store.state);
        state.replaying = false;
        if let Some(dir) = store.dir.as_ref().filter(|_| load)
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let record = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Record>(&text).ok());
                match record {
                    Some(record) if safe_id(&record.summary.agent_id) => {
                        state
                            .records
                            .insert(record.summary.agent_id.clone(), record);
                    }
                    _ => tracing::warn!(path = %path.display(), "could not read an agent's state"),
                }
            }
        }
        // A card needs the frozen prefix and the kept conversation, which only a record kept on
        // disk has: a session branched from another, or restarted with agent state off, has
        // ordinary idle subagents only.
        for record in state.records.values_mut() {
            if record.frozen.is_none() {
                record.summary.knowledge = None;
            }
        }
        drop(state);
        store
    }

    /// The highest task number in use, so new tasks continue after it.
    pub(crate) fn last_task_number(&self) -> u32 {
        lock(&self.state)
            .records
            .keys()
            .map(|id| task_number(id))
            .max()
            .unwrap_or(0)
    }

    /// Subagents recorded as working: after a restart, those the daemon stopped.
    pub(crate) fn running_subagents(&self) -> Vec<String> {
        let mut ids: Vec<String> = lock(&self.state)
            .records
            .values()
            .filter(|record| {
                record.summary.agent_id != ROOT_AGENT
                    && record.summary.status == AgentStatus::Running
            })
            .map(|record| record.summary.agent_id.clone())
            .collect();
        ids.sort_by_key(|id| task_number(id));
        ids
    }

    /// Marks subagents the daemon stopped mid-turn; they resume from their last saved point.
    pub(crate) fn mark_interrupted(&self, ids: &[String]) {
        let mut state = lock(&self.state);
        for id in ids {
            if let Some(record) = state.records.get_mut(id) {
                record.summary.status = AgentStatus::Interrupted;
                record.summary.updated_at_ms = unix_ms();
                self.write(record);
            }
        }
    }

    /// Every agent, the root first and then subagents in the order they started.
    pub(crate) fn list(&self) -> Vec<AgentSummary> {
        let mut agents: Vec<AgentSummary> = lock(&self.state)
            .records
            .values()
            .map(|record| record.summary.clone())
            .collect();
        agents.sort_by_key(|agent| (agent.agent_id != ROOT_AGENT, task_number(&agent.agent_id)));
        agents
    }

    /// The model `id` last ran on, when it ran in this daemon.
    pub(crate) fn model(&self, id: &str) -> Option<ResolvedModel> {
        lock(&self.state).models.get(id).cloned()
    }

    /// Notes an agent's role and model when it is created or switched.
    pub(crate) fn register(&self, id: &str, role: &str, model: &ResolvedModel) {
        let mut state = lock(&self.state);
        state.models.insert(id.to_string(), model.clone());
        let record = state.records.entry(id.to_string()).or_insert_with(|| {
            let parent = (id != ROOT_AGENT).then(|| ROOT_AGENT.to_string());
            new_record(id, parent, role, "", &model.spec)
        });
        record.summary.role = role.to_string();
        record.summary.model = model.spec.clone();
        record.summary.local = model.local;
        record.summary.updated_at_ms = unix_ms();
        self.write(record);
    }

    /// Saves a subagent's conversation (after its instructions) at a safe point: tool calls all
    /// have their results. For the root only the size is noted; its conversation is kept in the
    /// session's messages file.
    pub(crate) fn save_conversation(&self, id: &str, messages: &[ChatMessage]) {
        let mut state = lock(&self.state);
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        let earlier: Vec<ChatMessage> = messages
            .iter()
            .skip_while(|message| matches!(message, ChatMessage::System { .. }))
            .cloned()
            .collect();
        record.summary.messages = u32::try_from(earlier.len()).unwrap_or(u32::MAX);
        if id != ROOT_AGENT {
            record.conversation = earlier;
            record.summary.resumable = true;
        }
        record.summary.updated_at_ms = unix_ms();
        self.write(record);
    }

    /// A subagent's kept conversation.
    pub(crate) fn conversation(&self, id: &str) -> Vec<ChatMessage> {
        lock(&self.state)
            .records
            .get(id)
            .map(|record| record.conversation.clone())
            .unwrap_or_default()
    }

    /// Claims a finished subagent for a follow-up: it must have a kept conversation and not be
    /// working. Returns its state before the claim; it is marked running at once, so it cannot
    /// be resumed twice.
    pub(crate) fn claim(&self, id: &str) -> Result<AgentSummary, String> {
        let mut state = lock(&self.state);
        if id == ROOT_AGENT {
            return Err(
                "the root agent is continued by sending the session a message, not resumed".into(),
            );
        }
        let record = state
            .records
            .get_mut(id)
            .ok_or_else(|| format!("no agent {id:?} in this session"))?;
        if record.summary.status == AgentStatus::Running {
            return Err(format!(
                "{id} is still working; steer_agent or queue_agent reach it"
            ));
        }
        if !record.summary.resumable {
            return Err(format!(
                "{id}'s conversation was not kept (sessions.agent_state_keep is off), so it \
                 cannot be resumed"
            ));
        }
        let before = record.summary.clone();
        record.summary.status = AgentStatus::Running;
        record.summary.updated_at_ms = unix_ms();
        self.write(record);
        Ok(before)
    }

    /// Marks a subagent that is not working as cancelled (failed, with that report), so it is
    /// no longer listed as paused. Fails for the root, an unknown agent, or one that works.
    pub(crate) fn cancel(&self, id: &str) -> Result<(), String> {
        let mut state = lock(&self.state);
        if id == ROOT_AGENT {
            return Err("the root agent cannot be cancelled".into());
        }
        let record = state
            .records
            .get_mut(id)
            .ok_or_else(|| format!("no agent {id:?} in this session"))?;
        if record.summary.status == AgentStatus::Running {
            return Err(format!(
                "{id} is still working; it pauses when the user steers, or finishes on its own"
            ));
        }
        record.summary.status = AgentStatus::Failed;
        record.summary.updated_at_ms = unix_ms();
        self.write(record);
        Ok(())
    }

    /// Gives back a claim that could not start, restoring the status it had.
    pub(crate) fn release(&self, id: &str, status: AgentStatus) {
        let mut state = lock(&self.state);
        if let Some(record) = state.records.get_mut(id) {
            record.summary.status = status;
            self.write(record);
        }
    }

    /// Notes what kind of agent `id` is and what its latest request began with.
    pub(crate) fn note_run(&self, id: &str, kind: AgentKind, prefix: Frozen) {
        let mut state = lock(&self.state);
        if let Some(record) = state.records.get_mut(id) {
            record.kind = kind;
        }
        state.prefixes.insert(id.to_string(), prefix);
    }

    /// When `id`'s latest turn started (0 when unknown).
    pub(crate) fn turn_started_ms(&self, id: &str) -> u64 {
        lock(&self.state)
            .records
            .get(id)
            .map_or(0, |record| record.turn_started_ms)
    }

    pub(crate) fn kind(&self, id: &str) -> AgentKind {
        lock(&self.state)
            .records
            .get(id)
            .map_or(AgentKind::Ordinary, |record| record.kind)
    }

    /// What `id`'s latest request began with, when it ran in this daemon.
    pub(crate) fn latest_prefix(&self, id: &str) -> Option<Frozen> {
        lock(&self.state).prefixes.get(id).cloned()
    }

    pub(crate) fn summary(&self, id: &str) -> Option<AgentSummary> {
        lock(&self.state)
            .records
            .get(id)
            .map(|record| record.summary.clone())
    }

    /// Keeps `id` as a knowledge agent with `card` and its frozen prefix.
    pub(crate) fn set_knowledge(&self, id: &str, card: KnowledgeCard, frozen: Frozen) {
        let mut state = lock(&self.state);
        if let Some(record) = state.records.get_mut(id) {
            record.summary.knowledge = Some(card);
            record.frozen = Some(frozen);
            record.summary.updated_at_ms = unix_ms();
            self.write(record);
        }
    }

    /// Makes `id` an ordinary idle subagent again; whether it was a knowledge agent.
    pub(crate) fn clear_knowledge(&self, id: &str) -> bool {
        let mut state = lock(&self.state);
        let Some(record) = state.records.get_mut(id) else {
            return false;
        };
        let was = record.summary.knowledge.take().is_some();
        record.frozen = None;
        record.summary.updated_at_ms = unix_ms();
        self.write(record);
        was
    }

    /// Changes `id`'s card; returns it as it is afterwards (`None` when `id` has none).
    pub(crate) fn update_knowledge(
        &self,
        id: &str,
        change: impl FnOnce(&mut KnowledgeCard),
    ) -> Option<KnowledgeCard> {
        let mut state = lock(&self.state);
        let record = state.records.get_mut(id)?;
        let card = record.summary.knowledge.as_mut()?;
        change(card);
        let card = card.clone();
        record.summary.updated_at_ms = unix_ms();
        self.write(record);
        Some(card)
    }

    /// Replaces a knowledge agent's frozen prefix (after it was resumed and finished).
    pub(crate) fn refreeze(&self, id: &str, frozen: Frozen) {
        let mut state = lock(&self.state);
        if let Some(record) = state.records.get_mut(id)
            && record.summary.knowledge.is_some()
        {
            record.frozen = Some(frozen);
            self.write(record);
        }
    }

    /// The session's knowledge agents' cards, in the order the agents started.
    pub(crate) fn knowledge_cards(&self) -> Vec<KnowledgeCard> {
        let mut cards: Vec<KnowledgeCard> = lock(&self.state)
            .records
            .values()
            .filter_map(|record| record.summary.knowledge.clone())
            .collect();
        cards.sort_by_key(|card| task_number(&card.agent_id));
        cards
    }

    /// What a fork of knowledge agent `id` starts from.
    pub(crate) fn fork_source(&self, id: &str) -> Option<ForkSource> {
        let state = lock(&self.state);
        let record = state.records.get(id)?;
        Some(ForkSource {
            frozen: record.frozen.clone()?,
            conversation: record.conversation.clone(),
            card: record.summary.knowledge.clone()?,
        })
    }

    /// Follows an event into the agent records it changes.
    pub(crate) fn observe(&self, event: &AgentEvent) {
        self.observe_at(event, unix_ms());
    }

    /// [`AgentStore::observe`] for an event that happened at `at_ms` (0: unknown).
    fn observe_at(&self, event: &AgentEvent, at_ms: u64) {
        let agent = event.agent_id.as_str();
        // A question's fork is not an agent: its tool events carry a derived id and make no
        // record.
        if is_fork_id(agent) {
            return;
        }
        let mut state = lock(&self.state);
        let replaying = state.replaying;
        let records = &mut state.records;
        let root = || new_record(ROOT_AGENT, None, "root", "", "");
        let changed = match &event.event {
            SessionEvent::TaskSpawned {
                task_id,
                brief,
                model,
                title,
                ..
            } => {
                let mut record = new_record(task_id, Some(agent.to_string()), "leaf", brief, model);
                // Journals written before titles derive one from the brief.
                if !title.trim().is_empty() {
                    record.summary.title = title.clone();
                }
                record.summary.status = AgentStatus::Running;
                // A subagent registered before its spawn event keeps its role.
                if let Some(earlier) = records.get(task_id) {
                    record.summary.role = earlier.summary.role.clone();
                    record.summary.local = earlier.summary.local;
                }
                records.insert(task_id.clone(), record);
                Some(task_id.as_str())
            }
            SessionEvent::TaskResumed { task_id, model, .. } => {
                records.get_mut(task_id).map(|record| {
                    record.summary.status = AgentStatus::Running;
                    record.summary.model = model.clone();
                    record.summary.last_report = None;
                    task_id.as_str()
                })
            }
            SessionEvent::TaskFinished {
                task_id,
                ok,
                report,
                ..
            } => records.get_mut(task_id).map(|record| {
                record.summary.status = if *ok {
                    AgentStatus::Idle
                } else {
                    AgentStatus::Failed
                };
                record.summary.last_report =
                    Some(imperium_api::truncate_chars(report, REPORT_CHARS));
                task_id.as_str()
            }),
            SessionEvent::TaskPaused {
                task_id, progress, ..
            } => records.get_mut(task_id).map(|record| {
                record.summary.status = AgentStatus::Paused;
                record.summary.last_report =
                    Some(imperium_api::truncate_chars(progress, REPORT_CHARS));
                task_id.as_str()
            }),
            SessionEvent::TurnStarted { .. } => {
                let record = if agent == ROOT_AGENT {
                    records.entry(agent.to_string()).or_insert_with(root)
                } else {
                    match records.get_mut(agent) {
                        Some(record) => record,
                        None => return,
                    }
                };
                record.summary.turns += 1;
                record.turn_started_ms = at_ms;
                record.summary.status = AgentStatus::Running;
                Some(agent)
            }
            // A subagent's status follows its task (it may run queued follow-ups after a turn).
            SessionEvent::TurnCompleted { .. } if agent == ROOT_AGENT => {
                records.get_mut(agent).map(|record| {
                    record.summary.status = AgentStatus::Idle;
                    agent
                })
            }
            SessionEvent::TurnFailed { .. } if agent == ROOT_AGENT => {
                records.get_mut(agent).map(|record| {
                    record.summary.status = AgentStatus::Failed;
                    agent
                })
            }
            SessionEvent::ModelCallCompleted {
                model,
                usage,
                cost_usd,
                ..
            } => {
                if let Some(record) = records.get_mut(agent) {
                    add(&mut record.summary.usage, usage);
                    if let Some(cost) = cost_usd {
                        record.summary.cost_usd =
                            Some(record.summary.cost_usd.unwrap_or(0.0) + cost);
                    }
                    record.summary.model = model.clone();
                }
                // Written with the turn's end, not on every call.
                None
            }
            SessionEvent::PlanUpdated { plan } => records.get_mut(agent).map(|record| {
                record.summary.plan = Some(plan.clone());
                agent
            }),
            // Cards follow the journal when it is replayed; otherwise the store is the source.
            SessionEvent::KnowledgeChanged { agent_id, card, .. } if replaying => {
                records.get_mut(agent_id).map(|record| {
                    record.summary.knowledge = card.as_deref().cloned();
                    agent_id.as_str()
                })
            }
            SessionEvent::ModelChanged { to, .. } => {
                let record = records.entry(agent.to_string()).or_insert_with(root);
                record.summary.model = to.clone();
                Some(agent)
            }
            _ => None,
        };
        let Some(id) = changed else { return };
        let Some(record) = state.records.get_mut(id) else {
            return;
        };
        record.summary.updated_at_ms = unix_ms();
        if !replaying {
            self.write(record);
        }
    }

    /// Writes a record atomically (a temporary file, then a rename), when state is kept.
    fn write(&self, record: &Record) {
        let Some(dir) = &self.dir else { return };
        let id = &record.summary.agent_id;
        if !safe_id(id) {
            return;
        }
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let path = dir.join(format!("{id}.json"));
            let temporary = dir.join(format!("{id}.json.tmp"));
            imperium_api::fs::replace_file(&path, &temporary, serde_json::to_vec(record)?)
        })();
        if let Err(error) = result {
            tracing::warn!(dir = %dir.display(), agent = %id, %error, "could not save an agent's state");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_protocol::session::{KnowledgeStatus, KnowledgeUntrusted};

    fn card(agent: &str) -> KnowledgeCard {
        KnowledgeCard {
            agent_id: agent.into(),
            title: "Reader".into(),
            model: "fake:m".into(),
            local: false,
            topic: "the alpha module".into(),
            sources: Vec::new(),
            more_sources: 0,
            untrusted: KnowledgeUntrusted::default(),
            context_tokens: 100,
            context_window: Some(1000),
            est_question_cost: None,
            last_cached_ratio: None,
            questions: 0,
            usage: TokenUsage::default(),
            cost_usd: None,
            status: KnowledgeStatus::Ready,
            queued: 0,
            stale_fraction: 0.0,
        }
    }

    fn event(agent: &str, event: SessionEvent) -> AgentEvent {
        AgentEvent {
            agent_id: agent.into(),
            event,
        }
    }

    /// The journal of a session whose t1 was kept, asked once (the fork `t1#q1` read a file),
    /// and whose t2 asked.
    fn journal() -> Vec<AgentEvent> {
        vec![
            event(
                ROOT_AGENT,
                SessionEvent::TaskSpawned {
                    task_id: "t1".into(),
                    brief: "read the module".into(),
                    model: "fake:m".into(),
                    title: "Reader".into(),
                    at_ms: None,
                },
            ),
            event(
                "t1",
                SessionEvent::TaskFinished {
                    task_id: "t1".into(),
                    ok: true,
                    report: "read".into(),
                    usage: TokenUsage::default(),
                    cost_usd: None,
                    at_ms: None,
                },
            ),
            event(
                "t1",
                SessionEvent::KnowledgeChanged {
                    agent_id: "t1".into(),
                    card: Some(Box::new(card("t1"))),
                    retired: false,
                },
            ),
            event(
                "t1#q1",
                SessionEvent::ToolStarted {
                    call_id: "c".into(),
                    tool: "read_file".into(),
                    summary: "read a.txt".into(),
                    arguments: None,
                },
            ),
            event(
                "t1#q1",
                SessionEvent::TurnStarted {
                    text: "never emitted by a fork, but never an agent either".into(),
                    attachments: Vec::new(),
                },
            ),
        ]
    }

    #[test]
    fn a_replayed_turn_is_dated_by_the_journal_not_by_the_replay() {
        let mut timed: Vec<(u64, AgentEvent)> =
            journal().into_iter().map(|event| (1_000, event)).collect();
        timed.push((
            2_345,
            event(
                "t1",
                SessionEvent::TurnStarted {
                    text: "a later turn".into(),
                    attachments: Vec::new(),
                },
            ),
        ));
        let store = AgentStore::open_timed(None, &timed, true);
        assert_eq!(store.turn_started_ms("t1"), 2_345);
        // A journal line without a time marks nothing.
        let untimed = AgentStore::open(None, &journal(), true);
        assert_eq!(untimed.turn_started_ms("t1"), 0);
        // A turn that starts now is dated now.
        let live = AgentStore::open(None, &journal(), true);
        live.observe(&event(
            "t1",
            SessionEvent::TurnStarted {
                text: "now".into(),
                attachments: Vec::new(),
            },
        ));
        assert!(live.turn_started_ms("t1") > 1_000_000_000_000);
    }

    #[test]
    fn a_fork_of_a_journal_makes_no_agent_record() {
        let store = AgentStore::open(None, &journal(), true);
        let ids: Vec<String> = store
            .list()
            .into_iter()
            .map(|agent| agent.agent_id)
            .collect();
        assert_eq!(ids, ["t1"]);
        assert!(is_fork_id("t1#q1") && !is_fork_id("t1"));
    }

    #[test]
    fn cards_come_back_from_kept_records_and_not_from_a_journal_alone() {
        // A session branched from another, or restarted with agent state off, has the journal
        // and no record on disk: its subagents are ordinary, idle ones.
        let replayed = AgentStore::open(None, &journal(), true);
        assert!(replayed.summary("t1").unwrap().knowledge.is_none());
        assert!(replayed.knowledge_cards().is_empty());

        // With the records kept, the card (and the frozen prefix it needs) is restored.
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("s.agents");
        let first = AgentStore::open(Some(kept.clone()), &[], false);
        let model = crate::ResolvedModel {
            spec: "fake:m".into(),
            endpoint: imperium_models::Endpoint {
                base_url: String::new(),
                api_key: None,
            },
            model: "m".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "openai".into(),
            reasoning: None,
        };
        first.register("t1", "leaf", &model);
        first.save_conversation("t1", &[ChatMessage::user("read it")]);
        let frozen = Frozen {
            system: "system".into(),
            tools: Vec::new(),
            effort: Some("high".into()),
        };
        first.set_knowledge("t1", card("t1"), frozen);
        let reopened = AgentStore::open(Some(kept), &journal(), true);
        let cards = reopened.knowledge_cards();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].topic, "the alpha module");
        assert_eq!(
            reopened.fork_source("t1").unwrap().frozen.effort.as_deref(),
            Some("high")
        );
        assert!(
            reopened
                .list()
                .iter()
                .all(|agent| !agent.agent_id.contains('#'))
        );
        // Retiring clears the card and the frozen prefix, and it stays cleared after a reopen.
        assert!(reopened.clear_knowledge("t1"));
        assert!(reopened.fork_source("t1").is_none());
        assert!(!reopened.clear_knowledge("t1"));
    }
}
