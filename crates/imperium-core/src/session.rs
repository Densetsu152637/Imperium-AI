use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use tokio::sync::{broadcast, mpsc};

use crate::ResolvedModel;
use crate::agent::{Agent, Approver};
use crate::journal::Journal;

/// Events kept for slow subscribers before they start missing some.
const EVENT_BUFFER: usize = 1024;

/// Everything a session needs to start.
#[derive(Debug)]
pub struct SessionConfig {
    pub id: String,
    pub workspace: Workspace,
    pub model: ResolvedModel,
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

/// Handle to a session running on its own thread. Dropping every handle stops the session
/// after its current turn.
#[derive(Debug)]
pub struct Session {
    id: String,
    model: String,
    project_dir: String,
    commands: mpsc::UnboundedSender<Command>,
    events: broadcast::Sender<SessionEvent>,
    /// Every event so far, so late subscribers can catch up.
    history: History,
    _thread: JoinHandle<()>,
}

/// A random 16-hex-digit session id.
pub fn new_session_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

impl Session {
    pub fn spawn(config: SessionConfig) -> std::io::Result<Self> {
        let journal = Journal::open(&config.journal_path)?;
        let (commands, receiver) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let id = config.id.clone();
        let model = config.model.spec.clone();
        let project_dir = config.workspace.root_display();
        let thread_events = events.clone();
        let history = History::default();
        let thread_history = history.clone();
        let thread = std::thread::Builder::new()
            .name(format!("session-{id}"))
            .spawn(move || {
                runtime.block_on(run(
                    config,
                    receiver,
                    thread_events,
                    thread_history,
                    journal,
                ))
            })?;
        Ok(Session {
            id,
            model,
            project_dir,
            commands,
            events,
            history,
            _thread: thread,
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
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    /// Every event so far plus a receiver for the rest, with no gap or overlap between them.
    pub fn subscribe_with_history(&self) -> (Vec<SessionEvent>, broadcast::Receiver<SessionEvent>) {
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

/// The session thread's main loop: one turn at a time, in message order.
async fn run(
    config: SessionConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: broadcast::Sender<SessionEvent>,
    history: History,
    mut journal: Journal,
) {
    let mut agent = Agent::new(config.model, config.workspace, config.max_steps);
    let mut queued = VecDeque::new();
    let mut emit = |event: SessionEvent| {
        if let Err(error) = journal.record(&event) {
            tracing::warn!(session = %config.id, %error, "could not write the session journal");
        }
        // Record and broadcast under the history lock so subscribe_with_history sees each
        // event exactly once. No subscribers is fine: the journal still has the event.
        let mut history = history.lock();
        history.push(event.clone());
        let _ = events.send(event);
    };
    loop {
        let text = match queued.pop_front() {
            Some(text) => text,
            None => match commands.recv().await {
                Some(Command::Message(text)) => text,
                Some(Command::Respond { .. }) => continue,
                None => return,
            },
        };
        let mut approver = ChannelApprover {
            commands: &mut commands,
            queued: &mut queued,
        };
        agent.run_turn(text, &mut approver, &mut emit).await;
    }
}

/// The session's events so far, shared between the session thread and its handles.
#[derive(Debug, Clone, Default)]
struct History(Arc<Mutex<Vec<SessionEvent>>>);

impl History {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<SessionEvent>> {
        // A panic while holding the lock leaves the vector valid; keep using it.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Waits for the user's answer on the command channel, queueing messages that arrive meanwhile.
struct ChannelApprover<'a> {
    commands: &'a mut mpsc::UnboundedReceiver<Command>,
    queued: &'a mut VecDeque<String>,
}

impl Approver for ChannelApprover<'_> {
    async fn approve(
        &mut self,
        request_id: u64,
        _tool: &str,
        _summary: &str,
    ) -> PermissionDecision {
        loop {
            match self.commands.recv().await {
                Some(Command::Respond {
                    request_id: answered,
                    decision,
                }) if answered == request_id => return decision,
                Some(Command::Respond { .. }) => {}
                Some(Command::Message(text)) => self.queued.push_back(text),
                // Every handle is gone; nobody can approve.
                None => return PermissionDecision::Deny,
            }
        }
    }
}
