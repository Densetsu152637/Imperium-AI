//! A thin adapter over Imperium's approval flow.
//!
//! It mirrors `imperium_core::approvals::Approvals` (pending requests in a map, answered by id
//! over a oneshot channel), which the spike cannot depend on directly: that type needs the
//! daemon's `DaemonState` to announce requests. The one addition Ratio needs is
//! [`ApprovalDesk::withdraw`]: a withdrawal and an answer both take the pending entry under the
//! same lock, so exactly one of them wins, and a withdrawn request's later answer is discarded
//! without changing anything.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use tokio::sync::oneshot;

/// A pending request, as a client lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalInfo {
    pub id: u64,
    /// `tool`, `declassify`, `request` or `question`.
    pub kind: String,
    /// What the user is asked, with every labelled part already redacted.
    pub summary: String,
}

/// The user's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub approve: bool,
    /// Allow the tool for the rest of the session.
    pub for_session: bool,
    /// The user's text, for a question.
    pub text: Option<String>,
}

#[derive(Debug)]
struct Entry {
    info: ApprovalInfo,
    reply: oneshot::Sender<Answer>,
}

/// A request a call waits on.
#[derive(Debug)]
pub(crate) struct Pending {
    pub id: u64,
    pub receiver: oneshot::Receiver<Answer>,
}

#[derive(Debug, Default)]
pub struct ApprovalDesk {
    next: AtomicU64,
    pending: Mutex<BTreeMap<u64, Entry>>,
}

impl ApprovalDesk {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, Entry>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a request. Done synchronously when the call is made, so a client sees it (and
    /// can answer it) before the call's future is first polled.
    pub(crate) fn request(&self, kind: &str, summary: String) -> Pending {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (reply, receiver) = oneshot::channel();
        self.lock().insert(
            id,
            Entry {
                info: ApprovalInfo {
                    id,
                    kind: kind.to_owned(),
                    summary,
                },
                reply,
            },
        );
        Pending { id, receiver }
    }

    pub fn list(&self) -> Vec<ApprovalInfo> {
        self.lock().values().map(|e| e.info.clone()).collect()
    }

    /// Answers a request; `false` when it is no longer pending (answered or withdrawn), and
    /// the answer is then discarded.
    pub fn respond(&self, id: u64, answer: Answer) -> bool {
        let entry = self.lock().remove(&id);
        entry.is_some_and(|e| e.reply.send(answer).is_ok())
    }

    /// Answers the newest pending request.
    pub fn respond_last(&self, answer: Answer) -> bool {
        let entry = {
            let mut pending = self.lock();
            let id = pending.keys().next_back().copied();
            id.and_then(|id| pending.remove(&id))
        };
        entry.is_some_and(|e| e.reply.send(answer).is_ok())
    }

    /// Withdraws a request; `false` when the user's answer won the race.
    pub(crate) fn withdraw(&self, id: u64) -> bool {
        self.lock().remove(&id).is_some()
    }
}
