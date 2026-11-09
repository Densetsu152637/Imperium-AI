//! Agent state: a record for every agent of a session (who started it, its brief, model,
//! status, plan, usage, and a subagent's conversation) so finished subagents can be resumed with
//! a follow-up instead of starting afresh.
//!
//! Records follow the session's events (`task_spawned`, `turn_started`, `model_call_completed`,
//! `task_finished`, ...), so a journal replayed after a restart rebuilds the agent tree, and
//! subagents save their conversation at every safe point. When kept, each record is written
//! atomically to `<session>.agents/<agent-id>.json` beside the session's journal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use imperium_models::ChatMessage;
use imperium_protocol::session::{
    AgentEvent, AgentStatus, AgentSummary, ROOT_AGENT, SessionEvent, TokenUsage,
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
}

#[derive(Debug, Default)]
struct State {
    records: HashMap<String, Record>,
    /// Models agents were last given, to resume them on the same one. Not kept on disk (an
    /// endpoint can carry an API key); after a restart the model is found by its spec.
    models: HashMap<String, ResolvedModel>,
    /// Replaying a journal: records change in memory only.
    replaying: bool,
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
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

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    imperium_api::truncate_chars(line.trim(), 80)
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
            title: first_line(task),
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
            updated_at_ms: now_ms(),
        },
        conversation: Vec::new(),
    }
}

impl AgentStore {
    /// A store writing to `dir` (when given), with the agents `history` describes and, for a
    /// resumed session (`load`), the records kept in `dir`. Kept records win over the journal:
    /// they hold the conversations, and are written at least as often.
    pub fn open(dir: Option<PathBuf>, history: &[AgentEvent], load: bool) -> Self {
        let store = AgentStore {
            dir,
            state: Mutex::default(),
        };
        lock(&store.state).replaying = true;
        for event in history {
            store.observe(event);
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
        drop(state);
        store
    }

    /// The highest task number in use, so new tasks continue after it.
    pub fn last_task_number(&self) -> u32 {
        lock(&self.state)
            .records
            .keys()
            .map(|id| task_number(id))
            .max()
            .unwrap_or(0)
    }

    /// Subagents recorded as working: after a restart, those the daemon stopped.
    pub fn running_subagents(&self) -> Vec<String> {
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
    pub fn mark_interrupted(&self, ids: &[String]) {
        let mut state = lock(&self.state);
        for id in ids {
            if let Some(record) = state.records.get_mut(id) {
                record.summary.status = AgentStatus::Interrupted;
                record.summary.updated_at_ms = now_ms();
                self.write(record);
            }
        }
    }

    /// Every agent, the root first and then subagents in the order they started.
    pub fn list(&self) -> Vec<AgentSummary> {
        let mut agents: Vec<AgentSummary> = lock(&self.state)
            .records
            .values()
            .map(|record| record.summary.clone())
            .collect();
        agents.sort_by_key(|agent| (agent.agent_id != ROOT_AGENT, task_number(&agent.agent_id)));
        agents
    }

    /// The model `id` last ran on, when it ran in this daemon.
    pub fn model(&self, id: &str) -> Option<ResolvedModel> {
        lock(&self.state).models.get(id).cloned()
    }

    /// Notes an agent's role and model when it is created or switched.
    pub fn register(&self, id: &str, role: &str, model: &ResolvedModel) {
        let mut state = lock(&self.state);
        state.models.insert(id.to_string(), model.clone());
        let record = state.records.entry(id.to_string()).or_insert_with(|| {
            let parent = (id != ROOT_AGENT).then(|| ROOT_AGENT.to_string());
            new_record(id, parent, role, "", &model.spec)
        });
        record.summary.role = role.to_string();
        record.summary.model = model.spec.clone();
        record.summary.local = model.local;
        record.summary.updated_at_ms = now_ms();
        self.write(record);
    }

    /// Saves a subagent's conversation (after its instructions) at a safe point: tool calls all
    /// have their results. For the root only the size is noted; its conversation is kept in the
    /// session's messages file.
    pub fn save_conversation(&self, id: &str, messages: &[ChatMessage]) {
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
        record.summary.updated_at_ms = now_ms();
        self.write(record);
    }

    /// A subagent's kept conversation.
    pub fn conversation(&self, id: &str) -> Vec<ChatMessage> {
        lock(&self.state)
            .records
            .get(id)
            .map(|record| record.conversation.clone())
            .unwrap_or_default()
    }

    /// Claims a finished subagent for a follow-up: it must have a kept conversation and not be
    /// working. Returns its state before the claim; it is marked running at once, so it cannot
    /// be resumed twice.
    pub fn claim(&self, id: &str) -> Result<AgentSummary, String> {
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
        record.summary.updated_at_ms = now_ms();
        self.write(record);
        Ok(before)
    }

    /// Gives back a claim that could not start, restoring the status it had.
    pub fn release(&self, id: &str, status: AgentStatus) {
        let mut state = lock(&self.state);
        if let Some(record) = state.records.get_mut(id) {
            record.summary.status = status;
            self.write(record);
        }
    }

    /// Follows an event into the agent records it changes.
    pub fn observe(&self, event: &AgentEvent) {
        let mut state = lock(&self.state);
        let replaying = state.replaying;
        let agent = event.agent_id.as_str();
        let records = &mut state.records;
        let root = || new_record(ROOT_AGENT, None, "root", "", "");
        let changed = match &event.event {
            SessionEvent::TaskSpawned {
                task_id,
                brief,
                model,
            } => {
                let mut record = new_record(task_id, Some(agent.to_string()), "leaf", brief, model);
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
        record.summary.updated_at_ms = now_ms();
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
            std::fs::write(&temporary, serde_json::to_vec(record)?)?;
            std::fs::rename(&temporary, &path)
        })();
        if let Err(error) = result {
            tracing::warn!(dir = %dir.display(), agent = %id, %error, "could not save an agent's state");
        }
    }
}
