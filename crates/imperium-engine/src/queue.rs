//! Messages waiting for an agent while it is busy. Queued messages become the agent's next
//! turns, in order; steering messages join its running turn at the next safe point (after a
//! model response and its tool calls, before the next model call). Every change is journalled
//! as `queue_changed`, so a resumed session restores the root agent's queue.
//!
//! A queued message can be turned into steering in place ([`Inboxes::steer`]): one lock covers
//! the change, so the message is never both waiting as a turn and delivered as steering, and it
//! is never lost. If the turn ends before its next safe point, the steering message runs as the
//! next turn, ahead of the other queued messages, whose order is kept.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use imperium_protocol::session::{AgentEvent, Attachment, QueuedMessage, ROOT_AGENT, SessionEvent};

use tokio::sync::watch;

use crate::session::{Emitter, lock};

/// Why a message could not be queued, edited, or removed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    #[error("the session has stopped")]
    Closed,
    #[error("no running agent {0:?} in this session")]
    UnknownAgent(String),
    #[error("no queued message {0:?}; it may have been delivered already")]
    UnknownMessage(String),
    #[error("the message is empty")]
    Empty,
    #[error("message {0:?} is steering the running turn and can no longer be edited")]
    Steering(String),
}

/// One agent's waiting messages.
#[derive(Debug)]
pub(crate) struct Inbox {
    agent_id: String,
    emitter: Arc<Emitter>,
    state: Mutex<InboxState>,
    /// Counts steering messages as they arrive, so a waiting agent wakes for one (the root
    /// in `await_tasks`) and running subagents pause for one sent to the root.
    steering: watch::Sender<u64>,
}

#[derive(Debug, Default)]
struct InboxState {
    messages: VecDeque<QueuedMessage>,
    /// The agent finished for good (a subagent that reported); nothing more is accepted.
    closed: bool,
}

impl Inbox {
    /// Adds a message to the end of the queue. Events are emitted under the queue's lock, so
    /// the journal sees every change in order.
    fn push(&self, message: QueuedMessage) -> Result<(), QueueError> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(QueueError::UnknownAgent(self.agent_id.clone()));
        }
        self.emit(SessionEvent::MessageQueued {
            id: message.id.clone(),
            text: message.text.clone(),
            steer: message.steer,
        });
        let steer = message.steer;
        state.messages.push_back(message);
        self.changed(&state);
        if steer {
            self.signal_steering();
        }
        Ok(())
    }

    /// Takes the next message to run as a turn: a steering message that missed the turn it
    /// was meant for comes first, then the oldest; the others keep their order.
    pub fn pop(&self) -> Option<QueuedMessage> {
        let mut state = lock(&self.state);
        let message = take_next(&mut state.messages)?;
        self.changed(&state);
        Some(message)
    }

    /// Like [`pop`](Self::pop), but when nothing waits the inbox closes, so a message cannot
    /// slip in after the agent's last look.
    pub fn pop_or_close(&self) -> Option<QueuedMessage> {
        let mut state = lock(&self.state);
        match take_next(&mut state.messages) {
            Some(message) => {
                self.changed(&state);
                Some(message)
            }
            None => {
                state.closed = true;
                None
            }
        }
    }

    /// Closes the inbox and takes whatever still waits in it (a subagent that paused).
    pub fn close_and_drain(&self) -> Vec<QueuedMessage> {
        let mut state = lock(&self.state);
        state.closed = true;
        if state.messages.is_empty() {
            return Vec::new();
        }
        let messages = state.messages.drain(..).collect();
        self.changed(&state);
        messages
    }

    /// Takes every steering message, in order, for the running turn. The agent announces
    /// each with `steered` once it has added them to its conversation.
    pub fn take_steering(&self) -> Vec<QueuedMessage> {
        let mut state = lock(&self.state);
        if !state.messages.iter().any(|message| message.steer) {
            return Vec::new();
        }
        let (steering, rest) = state
            .messages
            .drain(..)
            .partition::<Vec<_>, _>(|message| message.steer);
        state.messages = rest.into();
        self.changed(&state);
        steering
    }

    /// Turns a queued message into steering, in place. Steering one that already steers
    /// changes nothing.
    fn steer(&self, id: &str) -> Result<(), QueueError> {
        let mut state = lock(&self.state);
        let message = state
            .messages
            .iter_mut()
            .find(|message| message.id == id)
            .ok_or_else(|| QueueError::UnknownMessage(id.to_string()))?;
        if message.steer {
            return Ok(());
        }
        message.steer = true;
        self.changed(&state);
        self.signal_steering();
        Ok(())
    }

    /// Follows steering messages as they arrive: the value changes with each one.
    pub fn steering_signal(&self) -> watch::Receiver<u64> {
        self.steering.subscribe()
    }

    fn signal_steering(&self) {
        self.steering.send_modify(|count| *count += 1);
    }

    pub fn has_steering(&self) -> bool {
        lock(&self.state)
            .messages
            .iter()
            .any(|message| message.steer)
    }

    pub fn list(&self) -> Vec<QueuedMessage> {
        lock(&self.state).messages.iter().cloned().collect()
    }

    fn edit(&self, id: &str, text: String) -> Result<(), QueueError> {
        let mut state = lock(&self.state);
        let message = state
            .messages
            .iter_mut()
            .find(|message| message.id == id)
            .ok_or_else(|| QueueError::UnknownMessage(id.to_string()))?;
        if message.steer {
            return Err(QueueError::Steering(id.to_string()));
        }
        message.text = text;
        self.changed(&state);
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<(), QueueError> {
        let mut state = lock(&self.state);
        let index = state
            .messages
            .iter()
            .position(|message| message.id == id)
            .ok_or_else(|| QueueError::UnknownMessage(id.to_string()))?;
        state.messages.remove(index);
        self.changed(&state);
        Ok(())
    }

    fn emit(&self, event: SessionEvent) {
        self.emitter.emit_raw(&self.agent_id, event);
    }

    fn changed(&self, state: &InboxState) {
        self.emit(SessionEvent::QueueChanged {
            messages: state.messages.iter().cloned().collect(),
        });
    }
}

/// Every agent's inbox in a session, shared by the session's handle and its agents.
#[derive(Debug)]
pub(crate) struct Inboxes {
    emitter: Arc<Emitter>,
    next_id: AtomicU64,
    agents: Mutex<HashMap<String, Arc<Inbox>>>,
}

impl Inboxes {
    /// The inboxes for a session, with the root agent's queue as `history` left it.
    pub fn new(emitter: Arc<Emitter>, history: &[AgentEvent]) -> Self {
        let mut last_id = 0;
        let mut root_queue = None;
        for event in history {
            match &event.event {
                SessionEvent::MessageQueued { id, .. } => {
                    last_id = last_id.max(id_number(id));
                }
                SessionEvent::QueueChanged { messages } if event.agent_id == ROOT_AGENT => {
                    root_queue = Some(messages.clone());
                }
                _ => {}
            }
        }
        let inboxes = Inboxes {
            emitter,
            next_id: AtomicU64::new(last_id),
            agents: Mutex::new(HashMap::new()),
        };
        let root = inboxes.open(ROOT_AGENT);
        lock(&root.state).messages = root_queue.unwrap_or_default().into();
        inboxes
    }

    /// A fresh message id, `q1`, `q2`, ...
    pub fn next_id(&self) -> String {
        format!("q{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// The agent's inbox, created when it has none.
    pub fn open(&self, agent_id: &str) -> Arc<Inbox> {
        Arc::clone(
            lock(&self.agents)
                .entry(agent_id.to_string())
                .or_insert_with(|| {
                    Arc::new(Inbox {
                        agent_id: agent_id.to_string(),
                        emitter: Arc::clone(&self.emitter),
                        state: Mutex::default(),
                        steering: watch::Sender::new(0),
                    })
                }),
        )
    }

    /// Forgets an agent that finished for good.
    pub fn close(&self, agent_id: &str) {
        lock(&self.agents).remove(agent_id);
    }

    fn get(&self, agent_id: &str) -> Result<Arc<Inbox>, QueueError> {
        lock(&self.agents)
            .get(agent_id)
            .cloned()
            .ok_or_else(|| QueueError::UnknownAgent(agent_id.to_string()))
    }

    /// Queues `text` (and its attachments) for a running subagent, or as steering for its
    /// running turn.
    pub fn send(
        &self,
        agent_id: &str,
        id: String,
        text: String,
        steer: bool,
        attachments: Vec<Attachment>,
    ) -> Result<QueuedMessage, QueueError> {
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(QueueError::Empty);
        }
        let message = QueuedMessage {
            id,
            text,
            steer,
            attachments,
        };
        self.get(agent_id)?.push(message.clone())?;
        Ok(message)
    }

    /// Adds a message to the root agent's queue (it is busy).
    pub fn queue_for_root(&self, message: QueuedMessage) {
        // The root's inbox never closes.
        let _ = self.open(ROOT_AGENT).push(message);
    }

    pub fn list(&self, agent_id: &str) -> Result<Vec<QueuedMessage>, QueueError> {
        Ok(self.get(agent_id)?.list())
    }

    pub fn edit(&self, agent_id: &str, id: &str, text: String) -> Result<(), QueueError> {
        if text.trim().is_empty() {
            return Err(QueueError::Empty);
        }
        self.get(agent_id)?.edit(id, text)
    }

    pub fn remove(&self, agent_id: &str, id: &str) -> Result<(), QueueError> {
        self.get(agent_id)?.remove(id)
    }

    pub fn steer(&self, agent_id: &str, id: &str) -> Result<(), QueueError> {
        self.get(agent_id)?.steer(id)
    }
}

/// The first steering message, else the oldest; the rest keep their order.
fn take_next(messages: &mut VecDeque<QueuedMessage>) -> Option<QueuedMessage> {
    let index = messages
        .iter()
        .position(|message| message.steer)
        .unwrap_or(0);
    messages.remove(index)
}

/// The number in a message id such as `q12`; 0 for anything else.
fn id_number(id: &str) -> u64 {
    id.strip_prefix('q')
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

/// What the model reads for a steering message: marked, so it knows the message arrived while
/// it was working and the turn goes on.
pub(crate) fn steering_text(text: &str) -> String {
    format!(
        "<steering>\nA message sent while you were working on this turn. Take it into account \
         and carry on from where you are; nothing was cancelled.\n\n{text}\n</steering>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str, steer: bool) -> QueuedMessage {
        QueuedMessage {
            id: format!("q-{text}"),
            text: text.into(),
            steer,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn a_steering_message_that_missed_its_turn_runs_next_and_the_rest_keep_their_order() {
        let mut messages: VecDeque<QueuedMessage> = [
            message("a", false),
            message("b", false),
            message("c", true),
            message("d", false),
        ]
        .into();
        let order: Vec<String> = std::iter::from_fn(|| take_next(&mut messages))
            .map(|message| message.text)
            .collect();
        assert_eq!(order, ["c", "a", "b", "d"]);
    }
}
