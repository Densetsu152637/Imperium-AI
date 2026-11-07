use std::collections::VecDeque;
use std::path::PathBuf;
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
    commands: mpsc::UnboundedSender<Command>,
    events: broadcast::Sender<SessionEvent>,
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
        let thread_events = events.clone();
        let thread = std::thread::Builder::new()
            .name(format!("session-{id}"))
            .spawn(move || runtime.block_on(run(config, receiver, thread_events, journal)))?;
        Ok(Session {
            id,
            model,
            commands,
            events,
            _thread: thread,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Receives events from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
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
    mut journal: Journal,
) {
    let mut agent = Agent::new(config.model, config.workspace, config.max_steps);
    let mut queued = VecDeque::new();
    let mut emit = |event: SessionEvent| {
        if let Err(error) = journal.record(&event) {
            tracing::warn!(session = %config.id, %error, "could not write the session journal");
        }
        // No subscribers is fine: the journal still has the event.
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
