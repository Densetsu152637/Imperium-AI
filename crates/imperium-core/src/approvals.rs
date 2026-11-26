//! Approval requests from unattended work (an auto-fix push, for example). They wait here
//! until a client answers with `approvals.respond`; every client is told when one arrives.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use chrono::Local;
use imperium_protocol::methods::{APPROVAL_REQUESTED, ApprovalInfo};
use tokio::sync::oneshot;

use crate::DaemonState;

/// The answer to a request whose text may be edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub approve: bool,
    /// The text the user sent instead of the proposed one.
    pub text: Option<String>,
}

#[derive(Debug)]
enum Reply {
    Plain(oneshot::Sender<bool>),
    Text(oneshot::Sender<Answer>),
}

#[derive(Debug)]
struct Pending {
    info: ApprovalInfo,
    /// The session whose call the request stands for, when it is one (a bridged tool call).
    session_id: Option<String>,
    reply: Reply,
}

/// Why an answer was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespondError {
    /// Text was sent for a request whose text cannot be edited.
    NoText,
}

#[derive(Debug, Default)]
pub struct Approvals {
    next: AtomicU64,
    pending: Mutex<BTreeMap<u64, Pending>>,
}

impl Approvals {
    fn queue(
        &self,
        daemon: &DaemonState,
        kind: &str,
        summary: String,
        (text, session_id): (Option<String>, Option<String>),
        reply: Reply,
    ) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let info = ApprovalInfo {
            id,
            kind: kind.to_string(),
            summary,
            requested_at: Local::now().to_rfc3339(),
            text,
        };
        self.lock().insert(
            id,
            Pending {
                info: info.clone(),
                session_id,
                reply,
            },
        );
        daemon.announce(APPROVAL_REQUESTED, info);
    }

    /// Queues a request and announces it; the receiver yields the answer (`false` if the
    /// daemon drops the request).
    pub fn request(
        &self,
        daemon: &DaemonState,
        kind: &str,
        summary: String,
    ) -> oneshot::Receiver<bool> {
        let (answer, receiver) = oneshot::channel();
        self.queue(daemon, kind, summary, (None, None), Reply::Plain(answer));
        receiver
    }

    /// Like [`Self::request`], for a request a session's call waits on: the card is tied to
    /// the session by its id, so [`Self::deny_session`] finds it whatever the summary says.
    pub fn request_for_session(
        &self,
        daemon: &DaemonState,
        kind: &str,
        session_id: &str,
        summary: String,
    ) -> oneshot::Receiver<bool> {
        let (answer, receiver) = oneshot::channel();
        self.queue(
            daemon,
            kind,
            summary,
            (None, Some(session_id.to_string())),
            Reply::Plain(answer),
        );
        receiver
    }

    /// Queues a request for `text`, which the user may replace before approving it.
    pub fn request_text(
        &self,
        daemon: &DaemonState,
        kind: &str,
        summary: String,
        text: String,
    ) -> oneshot::Receiver<Answer> {
        let (answer, receiver) = oneshot::channel();
        self.queue(
            daemon,
            kind,
            summary,
            (Some(text), None),
            Reply::Text(answer),
        );
        receiver
    }

    pub fn list(&self) -> Vec<ApprovalInfo> {
        self.lock()
            .values()
            .map(|pending| pending.info.clone())
            .collect()
    }

    /// Answers a request; false when there is no such pending request.
    pub fn respond(&self, id: u64, approve: bool) -> bool {
        self.respond_with(id, approve, None).unwrap_or(false)
    }

    /// Answers a request, with the user's own text for one whose text may be edited. Text for
    /// any other request is refused, and that request stays pending.
    pub fn respond_with(
        &self,
        id: u64,
        approve: bool,
        text: Option<String>,
    ) -> Result<bool, RespondError> {
        let mut pending = self.lock();
        if text.is_some()
            && pending
                .get(&id)
                .is_some_and(|p| matches!(p.reply, Reply::Plain(_)))
        {
            return Err(RespondError::NoText);
        }
        let Some(Pending { reply, .. }) = pending.remove(&id) else {
            return Ok(false);
        };
        match reply {
            Reply::Plain(answer) => {
                let _ = answer.send(approve);
            }
            Reply::Text(answer) => {
                let _ = answer.send(Answer {
                    approve,
                    text: text.filter(|_| approve),
                });
            }
        }
        Ok(true)
    }

    /// Declines every request of `kind` that stands for a call of `session_id`. The session is
    /// matched by the id stored with the request, never looked for in a summary (which quotes
    /// text the model wrote).
    pub fn deny_session(&self, session_id: &str, kind: &str) {
        let ids: Vec<u64> = self
            .lock()
            .iter()
            .filter(|(_, p)| p.info.kind == kind && p.session_id.as_deref() == Some(session_id))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.respond(id, false);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<u64, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon() -> (tempfile::TempDir, std::sync::Arc<DaemonState>) {
        let home = tempfile::tempdir().unwrap();
        let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
            .settings()
            .unwrap();
        let state = DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
        (home, state)
    }

    #[test]
    fn a_session_card_is_matched_by_its_id_not_by_its_summary() {
        let (_home, state) = daemon();
        let approvals = Approvals::default();
        let mut a = approvals.request_for_session(&state, "tool", "A", "run_command".into());
        let mut b = approvals.request_for_session(
            &state,
            "tool",
            "B",
            "run_command in session B: echo in session A: x".into(),
        );
        approvals.deny_session("A", "tool");
        assert_eq!(a.try_recv(), Ok(false));
        assert!(b.try_recv().is_err(), "B's card was answered");
        assert_eq!(approvals.list().len(), 1);
    }

    #[test]
    fn text_is_taken_only_for_a_request_that_has_text() {
        let (_home, state) = daemon();
        let approvals = Approvals::default();
        let _plain = approvals.request(&state, "push", "push it".into());
        assert_eq!(
            approvals.respond_with(1, true, Some("x".into())),
            Err(RespondError::NoText)
        );
        assert_eq!(approvals.list().len(), 1);
        let mut reply = approvals.request_text(&state, "reply", "reply".into(), "draft".into());
        assert_eq!(approvals.list()[1].text.as_deref(), Some("draft"));
        assert_eq!(
            approvals.respond_with(2, true, Some("mine".into())),
            Ok(true)
        );
        assert_eq!(
            reply.try_recv(),
            Ok(Answer {
                approve: true,
                text: Some("mine".into())
            })
        );
    }
}
