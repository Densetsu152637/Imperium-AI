use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use imperium_config::RoutingSettings;
use imperium_protocol::session::{AgentEvent, PermissionDecision, ROOT_AGENT, SessionEvent};
use imperium_tools::Workspace;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::agent::Agent;
use crate::journal::Journal;
use crate::{CatalogModel, ResolvedModel};

/// Events kept for slow subscribers before they start missing some.
const EVENT_BUFFER: usize = 1024;

/// Everything a session needs to start.
#[derive(Debug)]
pub struct SessionConfig {
    pub id: String,
    pub workspace: Workspace,
    /// The root agent's model, and the fallback for subagents.
    pub model: ResolvedModel,
    /// Models subagents may be routed to.
    pub catalog: Vec<CatalogModel>,
    pub routing: RoutingSettings,
    pub max_steps: u32,
    pub journal_path: PathBuf,
}

#[derive(Debug)]
enum Command {
    Message(String),
    Respond {
        request_id: u64,
        decision: PermissionDecision,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the session has stopped")]
pub struct SessionClosed;

/// Handle to a running session. Its root agent and every subagent are separate tasks on the
/// daemon's multi-threaded runtime, so agents run in parallel across cores. Dropping every
/// handle stops the session after its current turn.
#[derive(Debug)]
pub struct Session {
    id: String,
    model: String,
    project_dir: String,
    commands: mpsc::UnboundedSender<Command>,
    events: broadcast::Sender<AgentEvent>,
    /// Every event so far, so late subscribers can catch up.
    history: History,
    _task: JoinHandle<()>,
}

/// A random 16-hex-digit session id.
pub fn new_session_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

impl Session {
    /// Starts the session's root agent task. Must be called from within a Tokio runtime.
    pub fn spawn(config: SessionConfig) -> std::io::Result<Self> {
        let journal = Journal::open(&config.journal_path)?;
        let (commands, receiver) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let id = config.id.clone();
        let model = config.model.spec.clone();
        let project_dir = config.workspace.root_display();
        let history = History::default();
        let emitter = Emitter {
            session_id: id.clone(),
            journal: Mutex::new(journal),
            history: history.clone(),
            events: events.clone(),
        };
        let task = tokio::spawn(run(config, receiver, emitter));
        Ok(Session {
            id,
            model,
            project_dir,
            commands,
            events,
            history,
            _task: task,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The project folder the session works in.
    pub fn project_dir(&self) -> &str {
        &self.project_dir
    }

    /// Receives events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    /// Every event so far plus a receiver for the rest, with no gap or overlap between them.
    pub fn subscribe_with_history(&self) -> (Vec<AgentEvent>, broadcast::Receiver<AgentEvent>) {
        let history = self.history.lock();
        (history.clone(), self.events.subscribe())
    }

    /// Queues a user message; it runs when any current turn finishes.
    pub fn send(&self, text: String) -> Result<(), SessionClosed> {
        self.commands
            .send(Command::Message(text))
            .map_err(|_| SessionClosed)
    }

    /// Answers a permission request. Answers to unknown or already-answered requests are
    /// ignored.
    pub fn respond(
        &self,
        request_id: u64,
        decision: PermissionDecision,
    ) -> Result<(), SessionClosed> {
        self.commands
            .send(Command::Respond {
                request_id,
                decision,
            })
            .map_err(|_| SessionClosed)
    }
}

/// State shared by every agent in a session.
pub(crate) struct Shared {
    pub emitter: Emitter,
    pub broker: PermissionBroker,
    pub http: reqwest::Client,
    pub workspace: Workspace,
    pub catalog: Vec<CatalogModel>,
    pub routing: RoutingSettings,
    pub fallback_model: ResolvedModel,
    pub max_steps: u32,
    /// Tools the user allowed for the rest of the session, for every agent.
    pub allowed_tools: Mutex<HashSet<&'static str>>,
    next_task: AtomicU32,
}

impl Shared {
    pub fn next_task_id(&self) -> String {
        format!("t{}", self.next_task.fetch_add(1, Ordering::Relaxed) + 1)
    }
}

/// The root agent's task: one turn at a time, in message order, while
/// permission answers are routed to whichever agent asked.
async fn run(
    config: SessionConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    emitter: Emitter,
) {
    let shared = Arc::new(Shared {
        emitter,
        broker: PermissionBroker::default(),
        http: reqwest::Client::new(),
        workspace: config.workspace,
        catalog: config.catalog,
        routing: config.routing,
        fallback_model: config.model.clone(),
        max_steps: config.max_steps,
        allowed_tools: Mutex::new(HashSet::new()),
        next_task: AtomicU32::new(0),
    });
    let mut root = Agent::new(ROOT_AGENT.into(), Arc::clone(&shared), config.model, true);
    let mut queued = VecDeque::new();
    loop {
        let text = match queued.pop_front() {
            Some(text) => text,
            None => match commands.recv().await {
                Some(Command::Message(text)) => text,
                // Subagents still running after their parent's turn may be waiting for this.
                Some(Command::Respond {
                    request_id,
                    decision,
                }) => {
                    shared.broker.resolve(request_id, decision);
                    continue;
                }
                None => return,
            },
        };
        let turn = root.run_turn(text);
        tokio::pin!(turn);
        let mut handles_open = true;
        loop {
            tokio::select! {
                _ = &mut turn => break,
                command = commands.recv(), if handles_open => match command {
                    Some(Command::Respond { request_id, decision }) => {
                        shared.broker.resolve(request_id, decision);
                    }
                    Some(Command::Message(text)) => queued.push_back(text),
                    // Every handle is gone: nobody can answer, so deny what is pending.
                    None => {
                        handles_open = false;
                        shared.broker.deny_all();
                    }
                },
            }
        }
        if !handles_open {
            return;
        }
    }
}

/// Journals, records, and broadcasts events, attributing each to an agent.
pub(crate) struct Emitter {
    session_id: String,
    journal: Mutex<Journal>,
    history: History,
    events: broadcast::Sender<AgentEvent>,
}

impl Emitter {
    pub fn emit(&self, agent_id: &str, event: SessionEvent) {
        let event = AgentEvent {
            agent_id: agent_id.to_string(),
            event,
        };
        if let Err(error) = lock(&self.journal).record(&event) {
            tracing::warn!(session = %self.session_id, %error, "could not write the session journal");
        }
        // Record and broadcast under the history lock so subscribe_with_history sees each
        // event exactly once. No subscribers is fine: the journal still has the event.
        let mut history = self.history.lock();
        history.push(event.clone());
        let _ = self.events.send(event);
    }
}

/// Pending permission requests from any agent, answered by the user through the session.
#[derive(Default)]
pub(crate) struct PermissionBroker {
    next_id: AtomicU64,
    denying: AtomicBool,
    pending: Mutex<HashMap<u64, oneshot::Sender<PermissionDecision>>>,
}

impl PermissionBroker {
    /// Registers a request; the receiver resolves with the user's answer.
    pub fn request(&self) -> (u64, oneshot::Receiver<PermissionDecision>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (answer, receiver) = oneshot::channel();
        let mut pending = lock(&self.pending);
        if self.denying.load(Ordering::Relaxed) {
            let _ = answer.send(PermissionDecision::Deny);
        } else {
            pending.insert(id, answer);
        }
        (id, receiver)
    }

    fn resolve(&self, id: u64, decision: PermissionDecision) {
        if let Some(answer) = lock(&self.pending).remove(&id) {
            let _ = answer.send(decision);
        }
    }

    /// Denies every pending request and any made from now on.
    fn deny_all(&self) {
        let mut pending = lock(&self.pending);
        self.denying.store(true, Ordering::Relaxed);
        for (_, answer) in pending.drain() {
            let _ = answer.send(PermissionDecision::Deny);
        }
    }
}

/// The session's events so far, shared between its agents and its handles.
#[derive(Debug, Clone, Default)]
struct History(Arc<Mutex<Vec<AgentEvent>>>);

impl History {
    fn lock(&self) -> MutexGuard<'_, Vec<AgentEvent>> {
        lock(&self.0)
    }
}

/// Locks a mutex, recovering from poisoning: every value guarded here stays valid if a panic
/// interrupts its holder.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
