//! Archiving sessions. An archived session stops running and is not resumed after a restart,
//! but its journal and record are kept: it stays listed (with `archived`), its history can still
//! be read, and unarchiving resumes it where it left off.
//!
//! Sessions are also archived on their own, the way Claude's desktop app tidies its list: once
//! the pull request a session watches is merged or closed (`sessions.auto_archive_on_pr_close`,
//! on by default), and after a number of days without activity
//! (`sessions.auto_archive_after_days`, off by default). A session that is working, waiting on
//! the user (an approval, a usage limit, a model), has queued messages, is pinned, or is a
//! project agent is never archived on its own.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use imperium_engine::Session;
use imperium_protocol::methods::{SESSION_UPDATED, SessionSummary};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent, SessionStatus};
use serde_json::Value;

use super::{CreateError, DaemonState};

/// How often the daemon looks for sessions idle long enough to archive.
const IDLE_CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
/// The first look, a little after the daemon starts.
const FIRST_IDLE_CHECK_AFTER: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("no session {0:?}")]
    NotFound(String),
    #[error("session {0} is a persistent or project agent, which runs while the daemon does")]
    Persistent(String),
    #[error("could not record the archive: {0}")]
    Record(#[from] std::io::Error),
    #[error(transparent)]
    Resume(#[from] CreateError),
}

fn now_ms(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

fn read_meta(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

impl DaemonState {
    fn journal_path(&self, id: &str) -> std::path::PathBuf {
        self.paths
            .data_dir()
            .join("sessions")
            .join(format!("{id}.jsonl"))
    }

    fn archived_map(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, SessionSummary>> {
        self.archived
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Lists a session recorded as archived, from its meta file and journal.
    pub(super) fn remember_archived(&self, id: &str, meta: &Value, journal: &Path) {
        let project = meta["project_dir"].as_str().unwrap_or_default().to_string();
        let folders = meta["folders"]
            .as_array()
            .map(|folders| {
                folders
                    .iter()
                    .filter_map(|folder| folder.as_str().map(str::to_string))
                    .collect()
            })
            .filter(|folders: &Vec<String>| !folders.is_empty())
            .unwrap_or_else(|| vec![project.clone()]);
        let title = meta["title"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| title_from(&imperium_engine::journal_events(journal)));
        let summary = SessionSummary {
            session_id: id.to_string(),
            project_dir: project,
            environment_id: meta["environment_id"].as_str().map(str::to_string),
            folders,
            model: imperium_engine::journalled_model(journal)
                .or_else(|| meta["model"].as_str().map(str::to_string)),
            isolation: meta["isolation"].as_str().unwrap_or_default().to_string(),
            title,
            status: serde_json::from_value(meta["status"].clone()).unwrap_or(SessionStatus::Idle),
            status_detail: meta["status_detail"].as_str().map(str::to_string),
            resume_policy: self.resume_policy(),
            background: meta["background"] == true,
            hold: None,
            pinned: meta["pinned"] == true,
            persistent: false,
            worktrees: serde_json::from_value(meta["worktrees"].clone()).unwrap_or_default(),
            archived: true,
            archived_at: meta["archived_at"].as_u64(),
        };
        self.archived_map().insert(id.to_string(), summary);
    }

    /// Archived sessions, as lists show them.
    pub fn archived_sessions(&self) -> Vec<SessionSummary> {
        let mut sessions: Vec<_> = self.archived_map().values().cloned().collect();
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        sessions
    }

    /// Stops listing an archived session (it is being deleted); its summary, if it was one.
    pub(super) fn take_archived(&self, id: &str) -> Option<SessionSummary> {
        self.archived_map().remove(id)
    }

    /// An archived session's events, read from its journal.
    pub fn archived_history(&self, id: &str) -> Option<Vec<AgentEvent>> {
        self.archived_map()
            .contains_key(id)
            .then(|| imperium_engine::journal_events(&self.journal_path(id)))
    }

    /// Archives a running session: records it, stops it, and tells every client. Archiving an
    /// archived session changes nothing.
    pub fn archive(&self, id: &str) -> Result<SessionSummary, ArchiveError> {
        self.archive_at(id, SystemTime::now())
    }

    fn archive_at(&self, id: &str, now: SystemTime) -> Result<SessionSummary, ArchiveError> {
        if let Some(summary) = self.archived_map().get(id) {
            return Ok(summary.clone());
        }
        let session = self
            .session(id)
            .ok_or_else(|| ArchiveError::NotFound(id.to_string()))?;
        let placement = self.placement(id);
        if placement.persistent || placement.project_agent {
            return Err(ArchiveError::Persistent(id.to_string()));
        }
        let mut summary = self.summary(&session);
        summary.archived = true;
        summary.archived_at = Some(now_ms(now));
        if summary.status != SessionStatus::Merged {
            summary.status = SessionStatus::Idle;
            summary.status_detail = None;
        }
        let path = self.meta_path(id);
        let mut meta = read_meta(&path).unwrap_or_else(|| {
            serde_json::json!({
                "project_dir": summary.project_dir,
                "model": summary.model,
                "environment_id": summary.environment_id,
                "folders": summary.folders,
                "pinned": summary.pinned,
            })
        });
        meta["archived"] = Value::Bool(true);
        meta["archived_at"] = summary.archived_at.into();
        meta["title"] = summary.title.clone().into();
        meta["isolation"] = summary.isolation.clone().into();
        meta["status"] = serde_json::to_value(summary.status).unwrap_or(Value::Null);
        meta["status_detail"] = summary.status_detail.clone().into();
        std::fs::write(&path, meta.to_string())?;
        // Dropping the daemon's handle stops the session once its current step ends.
        self.sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .remove(id);
        super::lock_watches(&self.watches).remove(id);
        self.placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        self.archived_map().insert(id.to_string(), summary.clone());
        tracing::info!(session = %id, "archived the session");
        self.announce(SESSION_UPDATED, &summary);
        Ok(summary)
    }

    /// Resumes an archived session and tells every client. Unarchiving a running session
    /// changes nothing.
    pub fn unarchive(&self, id: &str) -> Result<SessionSummary, ArchiveError> {
        if let Some(session) = self.session(id) {
            return Ok(self.summary(&session));
        }
        let Some(archived) = self.archived_map().remove(id) else {
            return Err(ArchiveError::NotFound(id.to_string()));
        };
        let path = self.meta_path(id);
        let restore = |state: &Self| {
            state
                .archived_map()
                .insert(id.to_string(), archived.clone());
        };
        let Some(mut meta) = read_meta(&path) else {
            restore(self);
            return Err(ArchiveError::NotFound(id.to_string()));
        };
        if let Some(fields) = meta.as_object_mut() {
            for key in ["archived", "archived_at", "status", "status_detail"] {
                fields.remove(key);
            }
        }
        if let Err(error) = std::fs::write(&path, meta.to_string()) {
            restore(self);
            return Err(error.into());
        }
        match self.resume_from_meta(id, &meta, &self.journal_path(id)) {
            Ok((session, _)) => {
                let summary = self.summary(&session);
                self.announce(SESSION_UPDATED, &summary);
                Ok(summary)
            }
            Err(error) => {
                meta["archived"] = Value::Bool(true);
                meta["archived_at"] = archived.archived_at.into();
                let _ = std::fs::write(&path, meta.to_string());
                restore(self);
                Err(error.into())
            }
        }
    }

    /// Whether a session may be archived on its own: it is idle (or its pull request merged),
    /// nothing waits for it, and it is neither pinned nor a project agent.
    pub fn may_auto_archive(&self, session: &Session) -> bool {
        let placement = self.placement(session.id());
        let quiet = matches!(
            session.status().0,
            SessionStatus::Idle | SessionStatus::Merged
        );
        let nothing_queued = session.queue(None).map_or(true, |queue| queue.is_empty());
        quiet
            && nothing_queued
            && !placement.pinned
            && !placement.persistent
            && !placement.project_agent
    }

    /// After the pull request a session watches was merged or closed: archives the session when
    /// `sessions.auto_archive_on_pr_close` is on and it may be archived now. Returns whether it
    /// was.
    pub fn archive_after_pull_request(&self, session: &Session) -> bool {
        self.settings().sessions.auto_archive_on_pr_close
            && self.may_auto_archive(session)
            && self.archive(session.id()).is_ok()
    }

    /// Archives sessions idle for `sessions.auto_archive_after_days` as of `now` (activity is
    /// the session's last journalled event). Returns the ids archived.
    pub fn archive_idle_sessions(&self, now: SystemTime) -> Vec<String> {
        let days = self.settings().sessions.auto_archive_after_days;
        if days == 0 {
            return Vec::new();
        }
        let idle_for = DAY * days;
        let mut archived = Vec::new();
        for session in self.sessions() {
            let last = std::fs::metadata(self.journal_path(session.id()))
                .and_then(|meta| meta.modified())
                .unwrap_or(now);
            let idle = now.duration_since(last).unwrap_or_default() >= idle_for;
            if idle && self.may_auto_archive(&session) && self.archive_at(session.id(), now).is_ok()
            {
                archived.push(session.id().to_string());
            }
        }
        archived
    }

    /// Looks for idle sessions to archive a minute after start and then every hour.
    pub fn start_auto_archive(self: &Arc<Self>) {
        let daemon = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_IDLE_CHECK_AFTER).await;
            loop {
                let Some(state) = daemon.upgrade() else {
                    return;
                };
                state.archive_idle_sessions(SystemTime::now());
                drop(state);
                tokio::time::sleep(IDLE_CHECK_EVERY).await;
            }
        });
    }

    fn placement(&self, id: &str) -> super::Placement {
        self.placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            .unwrap_or_default()
    }
}

/// A session's first message, as [`Session::title`] gives it.
fn title_from(events: &[AgentEvent]) -> String {
    events
        .iter()
        .find_map(|event| match &event.event {
            SessionEvent::TurnStarted { text } if event.agent_id == ROOT_AGENT => {
                Some(text.chars().take(80).collect())
            }
            _ => None,
        })
        .unwrap_or_default()
}
