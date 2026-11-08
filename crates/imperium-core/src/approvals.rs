//! Approval requests from unattended work (an auto-fix push, for example). They wait here
//! until a client answers with `approvals.respond`; every client is told when one arrives.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use chrono::Local;
use imperium_protocol::methods::{APPROVAL_REQUESTED, ApprovalInfo};
use tokio::sync::oneshot;

use crate::DaemonState;

#[derive(Debug, Default)]
pub struct Approvals {
    next: AtomicU64,
    pending: Mutex<BTreeMap<u64, (ApprovalInfo, oneshot::Sender<bool>)>>,
}

impl Approvals {
    /// Queues a request and announces it; the receiver yields the answer (`false` if the
    /// daemon drops the request).
    pub fn request(
        &self,
        daemon: &DaemonState,
        kind: &str,
        summary: String,
    ) -> oneshot::Receiver<bool> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let info = ApprovalInfo {
            id,
            kind: kind.to_string(),
            summary,
            requested_at: Local::now().to_rfc3339(),
        };
        let (answer, receiver) = oneshot::channel();
        self.lock().insert(id, (info.clone(), answer));
        daemon.announce(APPROVAL_REQUESTED, info);
        receiver
    }

    pub fn list(&self) -> Vec<ApprovalInfo> {
        self.lock().values().map(|(info, _)| info.clone()).collect()
    }

    /// Answers a request; false when there is no such pending request.
    pub fn respond(&self, id: u64, approve: bool) -> bool {
        match self.lock().remove(&id) {
            Some((_, answer)) => {
                let _ = answer.send(approve);
                true
            }
            None => false,
        }
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<u64, (ApprovalInfo, oneshot::Sender<bool>)>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
