//! Branching sessions and comparing models (roadmap item 7).
//!
//! A branch is a new session whose journal and root conversation are copies of the source's up
//! to a point. It is an ordinary session in every other way: it runs on its own, can be branched
//! again, and is archived or deleted like any other. Best-of-N (`session.compare`) is several
//! branches of one point, each on its own model and sent the same message, tagged with a shared
//! comparison id; picking one archives the rest.

use std::path::Path;
use std::sync::Arc;

use imperium_protocol::methods::{
    BranchOrigin, CompareBranch, SessionBranchParams, SessionBranchResult, SessionCompareParams,
    SessionComparePickParams, SessionComparePickResult, SessionCompareResult,
};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use serde::Deserialize;
use serde_json::Value;

use super::DaemonState;
use crate::checkpoints;
use crate::safe_git;

/// Fewest and most models a comparison takes.
const COMPARE_MODELS: std::ops::RangeInclusive<usize> = 2..=4;

/// A line of a journal, as the engine reads it.
#[derive(Deserialize)]
struct Stored {
    #[serde(flatten)]
    #[allow(dead_code)]
    event: AgentEvent,
}

/// The first `events` readable lines of a journal (what the engine counts as events), and the
/// byte offset each ends at.
fn journal_prefix(raw: &str, events: usize) -> (String, Vec<usize>) {
    let mut prefix = String::new();
    let mut ends = Vec::new();
    for line in raw.split_inclusive('\n') {
        if ends.len() == events {
            break;
        }
        if serde_json::from_str::<Stored>(line.trim_end()).is_ok() {
            prefix.push_str(line);
            if !line.ends_with('\n') {
                prefix.push('\n');
            }
            ends.push(prefix.len());
        }
    }
    (prefix, ends)
}

/// What to branch, from where.
pub(crate) struct BranchRequest {
    pub source: String,
    /// The event to branch at; `None` branches at the session's current point.
    pub at: Option<u32>,
    pub title: Option<String>,
    pub comparison_id: Option<String>,
    pub worktree: bool,
}

/// A branch that was made.
pub(crate) struct Branched {
    pub session_id: String,
    pub prompt: Option<String>,
    pub worktree: bool,
}

impl DaemonState {
    /// Starts a new session from `request.source` as it was at the branch point. The source is
    /// left as it is; the branch continues on its own.
    pub(crate) fn branch_session(&self, request: &BranchRequest) -> Result<Branched, String> {
        let source = request.source.as_str();
        let mut meta: Value = std::fs::read_to_string(self.meta_path(source))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or_else(|| format!("no session {source}, or it is not recorded"))?;
        let journal = self.journal_path(source);
        let plan = match request.at {
            Some(at) => imperium_engine::plan_branch(&journal, at as usize)?,
            None => imperium_engine::plan_branch_at_end(&journal)?,
        };
        let raw = std::fs::read_to_string(&journal)
            .map_err(|error| format!("could not read the session's journal: {error}"))?;
        let (prefix, ends) = journal_prefix(&raw, plan.events);
        if ends.len() != plan.events {
            return Err("the session's journal changed while branching; try again".into());
        }
        // Where the files were at this point: the end of the last turn kept.
        let kept = imperium_engine::journal_events(&journal);
        let turn_end = kept[..plan.events.min(kept.len())]
            .iter()
            .rposition(|event| {
                event.agent_id == ROOT_AGENT
                    && matches!(
                        event.event,
                        SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
                    )
            })
            .map_or(0, |index| ends[index] as u64);

        let new_id = imperium_engine::new_session_id().map_err(|error| error.to_string())?;
        let new_journal = self.journal_path(&new_id);
        std::fs::write(&new_journal, prefix).map_err(|error| error.to_string())?;
        let cleanup = |state: &Self| {
            let _ = std::fs::remove_file(&new_journal);
            let _ = std::fs::remove_file(imperium_engine::messages_path(&new_journal));
            let _ = std::fs::remove_file(imperium_engine::turns_path(&new_journal));
            let _ = std::fs::remove_file(state.meta_path(&new_id));
        };
        let written = (|| -> Result<(), String> {
            let messages = serde_json::to_vec(&plan.messages).map_err(|error| error.to_string())?;
            std::fs::write(imperium_engine::messages_path(&new_journal), messages)
                .map_err(|error| error.to_string())?;
            // The branch can be branched in turn: it inherits the index up to the point.
            let mut turns: String = std::fs::read_to_string(imperium_engine::turns_path(&journal))
                .unwrap_or_default()
                .lines()
                .filter(|line| {
                    serde_json::from_str::<Value>(line)
                        .ok()
                        .and_then(|mark| mark["events"].as_u64())
                        .is_some_and(|events| events <= plan.events as u64)
                })
                .map(|line| format!("{line}\n"))
                .collect();
            turns.push_str(&format!(
                "{{\"events\":{},\"messages\":{}}}\n",
                plan.events,
                plan.messages.len()
            ));
            std::fs::write(imperium_engine::turns_path(&new_journal), turns)
                .map_err(|error| error.to_string())
        })();
        if let Err(error) = written {
            cleanup(self);
            return Err(error);
        }

        let source_title = self
            .session(source)
            .map(|session| self.summary(&session).title)
            .or_else(|| meta["title"].as_str().map(str::to_string))
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Session".into());
        let source_worktrees: Vec<imperium_protocol::methods::SessionWorktree> =
            serde_json::from_value(meta["worktrees"].clone()).unwrap_or_default();
        let at = request
            .at
            .unwrap_or_else(|| u32::try_from(plan.events.saturating_sub(1)).unwrap_or(u32::MAX));
        if let Some(fields) = meta.as_object_mut() {
            let given = request.title.as_deref().filter(|t| !t.trim().is_empty());
            fields.insert(
                "title".into(),
                given
                    .map_or_else(|| format!("{source_title} (branch)"), str::to_string)
                    .into(),
            );
            fields.insert("title_by_user".into(), given.is_some().into());
            fields.insert("pinned".into(), false.into());
            fields.insert(
                "branched_from".into(),
                serde_json::to_value(BranchOrigin {
                    session_id: source.to_string(),
                    at,
                })
                .unwrap_or_default(),
            );
            match &request.comparison_id {
                Some(id) => fields.insert("comparison_id".into(), id.clone().into()),
                None => fields.remove("comparison_id"),
            };
            for key in [
                "deferred_brief",
                "forked_from",
                "archived",
                "archived_at",
                "status",
                "status_detail",
            ] {
                fields.remove(key);
            }
            if request.worktree {
                fields.insert("worktrees".into(), Value::Array(Vec::new()));
            }
        }
        if let Err(error) = std::fs::write(self.meta_path(&new_id), meta.to_string()) {
            cleanup(self);
            return Err(error.to_string());
        }
        if let Err(error) = self.resume_from_meta(&new_id, &meta, &new_journal, request.worktree) {
            cleanup(self);
            return Err(error.to_string());
        }
        let mut made_worktree = false;
        if request.worktree {
            let records = self.placement(&new_id).worktrees;
            if records.is_empty() {
                let _ = self.delete_session(&new_id, true);
                return Err(
                    "no git worktree could be made: the session's folders are not in a git \
                     repository on this machine"
                        .into(),
                );
            }
            if let Err(error) =
                self.files_into_worktrees(source, &source_worktrees, &records, turn_end)
            {
                let _ = self.delete_session(&new_id, true);
                return Err(error);
            }
            made_worktree = true;
        }
        Ok(Branched {
            session_id: new_id,
            prompt: plan.prompt,
            worktree: made_worktree,
        })
    }

    /// Writes the source's files as they were at journal position `journal_bytes` into the
    /// branch's new worktrees: from the first later checkpoint, else the files as they are now
    /// (no turn since changed them). The source's own working tree is only read.
    fn files_into_worktrees(
        &self,
        source: &str,
        source_worktrees: &[imperium_protocol::methods::SessionWorktree],
        records: &[imperium_protocol::methods::SessionWorktree],
        journal_bytes: u64,
    ) -> Result<(), String> {
        for record in records {
            let source_dir = source_worktrees
                .iter()
                .find(|tree| tree.folder == record.folder)
                .map_or(record.folder.as_str(), |tree| tree.path.as_str());
            let Some(source_top) = safe_git::toplevel(Path::new(source_dir)) else {
                continue;
            };
            let Some(new_top) = safe_git::toplevel(Path::new(&record.path)) else {
                continue;
            };
            let files = match checkpoints::commit_after(
                self.paths.data_dir(),
                source,
                journal_bytes,
                &source_top,
            ) {
                // Worktrees share their repository's objects.
                Some(commit) => checkpoints::commit_files(&new_top, &commit)?,
                None => {
                    checkpoints::working_files(&source_top, &mut checkpoints::HashCache::default())?
                }
            };
            checkpoints::restore(&new_top, &files, &mut checkpoints::HashCache::default())?;
        }
        Ok(())
    }

    /// `session.branch`.
    pub(crate) fn branch(
        &self,
        params: &SessionBranchParams,
    ) -> Result<SessionBranchResult, String> {
        let branched = self.branch_session(&BranchRequest {
            source: params.session_id.clone(),
            at: params.at,
            title: params.title.clone(),
            comparison_id: None,
            worktree: params.worktree,
        })?;
        Ok(SessionBranchResult {
            session_id: branched.session_id,
            prompt: branched.prompt,
            worktree: branched.worktree,
        })
    }

    /// `session.compare`: the same message, from the same point, on each model.
    pub(crate) async fn compare(
        self: &Arc<Self>,
        params: SessionCompareParams,
    ) -> Result<SessionCompareResult, String> {
        if !COMPARE_MODELS.contains(&params.models.len()) {
            return Err("a comparison takes two to four models".into());
        }
        let mut specs: Vec<&String> = params.models.iter().collect();
        specs.sort();
        specs.dedup();
        if specs.len() != params.models.len() {
            return Err("name each model once".into());
        }
        if params.message.trim().is_empty() {
            return Err("a comparison needs a message to send".into());
        }
        if self.session(&params.session_id).is_none() {
            return Err(format!("no running session {}", params.session_id));
        }
        // Checked up front, so no branch is made for a model that cannot be used.
        let mut resolved = Vec::new();
        for spec in &params.models {
            let model = self
                .resolve_model(Some(spec))
                .map_err(|error| format!("{spec}: {error}"))?;
            self.check_selectable(&model)
                .map_err(|error| format!("{spec}: {error}"))?;
            resolved.push(model);
        }
        let comparison_id = imperium_engine::new_session_id().map_err(|error| error.to_string())?;
        let source_title = self
            .session(&params.session_id)
            .map(|session| self.summary(&session).title)
            .unwrap_or_default();
        let mut branches = Vec::new();
        for (spec, model) in params.models.iter().zip(resolved) {
            let request = BranchRequest {
                source: params.session_id.clone(),
                at: None,
                title: Some(format!("{source_title} · {spec}")),
                comparison_id: Some(comparison_id.clone()),
                worktree: params.worktree != Some(false),
            };
            let state = Arc::clone(self);
            let made = tokio::task::spawn_blocking(move || {
                match state.branch_session(&request) {
                    // Folders outside git (or an explicit choice) share the source's folders.
                    Err(_) if params.worktree.is_none() => state.branch_session(&BranchRequest {
                        worktree: false,
                        ..request
                    }),
                    other => other,
                }
            })
            .await
            .map_err(|error| error.to_string())?;
            let branched = match made {
                Ok(branched) => branched,
                Err(error) => {
                    // No half a comparison: the branches made so far are dropped.
                    for branch in &branches {
                        let branch: &CompareBranch = branch;
                        let _ = self.delete_session(&branch.session_id, true);
                    }
                    return Err(error);
                }
            };
            let session = self
                .session(&branched.session_id)
                .ok_or("the branch stopped before it could start")?;
            session
                .set_model(model)
                .await
                .map_err(|error| error.to_string())?;
            branches.push(CompareBranch {
                session_id: branched.session_id,
                model: spec.clone(),
                worktree: branched.worktree,
            });
        }
        for branch in &branches {
            if let Some(session) = self.session(&branch.session_id) {
                session
                    .send(params.message.clone())
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(SessionCompareResult {
            comparison_id,
            branches,
        })
    }

    /// `session.compare_pick`: keeps one branch, archives the others.
    pub(crate) fn compare_pick(
        &self,
        params: &SessionComparePickParams,
    ) -> Result<SessionComparePickResult, String> {
        let mut members: Vec<String> = self
            .placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, placement)| {
                placement.comparison_id.as_deref() == Some(params.comparison_id.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();
        members.extend(
            self.archived_sessions()
                .into_iter()
                .filter(|summary| {
                    summary.comparison_id.as_deref() == Some(params.comparison_id.as_str())
                })
                .map(|summary| summary.session_id),
        );
        members.sort();
        members.dedup();
        if !members.contains(&params.session_id) {
            return Err(format!(
                "session {} is not part of comparison {}",
                params.session_id, params.comparison_id
            ));
        }
        // The winner may have been archived by an earlier pick; keeping it runs it again.
        if self.session(&params.session_id).is_none() {
            self.unarchive(&params.session_id)
                .map_err(|error| error.to_string())?;
        }
        let mut archived = Vec::new();
        for id in members.into_iter().filter(|id| *id != params.session_id) {
            if self.session(&id).is_some() {
                self.archive(&id).map_err(|error| error.to_string())?;
                archived.push(id);
            }
        }
        Ok(SessionComparePickResult {
            kept: params.session_id.clone(),
            archived,
        })
    }
}
