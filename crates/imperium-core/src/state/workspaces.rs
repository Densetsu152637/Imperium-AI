//! The daemon's side of environments' working folders (see [`crate::workspaces`] and
//! docs/14-projects-and-automation.md#environment-working-folders-implemented): where the folders are,
//! the scratch folder of each session, and moving the whole tree when `workspaces.root`
//! changes.

use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use imperium_protocol::methods::SESSION_UPDATED;
use imperium_protocol::session::SessionStatus;
use imperium_protocol::workspaces::{
    EnvironmentWorkspace, WORKSPACES_PROGRESS, WorkspacesMoveResult, WorkspacesProgress,
    WorkspacesWhereResult,
};

use super::DaemonState;
use crate::workspaces::{self, MoveMethod, MoveProgress};

/// The setting that names the folder.
pub const ROOT_SETTING: &str = "workspaces.root";

impl DaemonState {
    /// The folder that holds the environments' working folders now.
    pub fn workspaces_root(&self) -> PathBuf {
        self.settings().workspaces.root(&self.paths)
    }

    /// Makes the session's scratch folder; `None` (logged) when it cannot be made, and the
    /// session then runs without one.
    pub(super) fn session_scratch(&self, environment: &str, session: &str) -> Option<PathBuf> {
        match workspaces::ensure_session_dir(&self.workspaces_root(), environment, session) {
            Ok(dir) => Some(dir),
            Err(error) => {
                tracing::warn!(%environment, %session, %error, "could not create the session's scratch folder");
                None
            }
        }
    }

    /// Deletes the session's scratch folder.
    pub(super) fn remove_session_scratch(&self, environment: &str, session: &str) {
        if let Err(error) =
            workspaces::remove_session_dir(&self.workspaces_root(), environment, session)
        {
            tracing::warn!(%environment, %session, %error, "could not delete the session's scratch folder");
        }
    }

    /// `workspaces.where`.
    pub fn workspaces_where(&self) -> WorkspacesWhereResult {
        let root = self.workspaces_root();
        let environments = self
            .environments
            .list()
            .into_iter()
            .filter_map(|env| {
                let dir = workspaces::environment_dir(&root, &env.id)?;
                let memory = workspaces::memory_file(&root, &env.id)?;
                let session_folders = workspaces::sessions_dir(&root, &env.id)
                    .and_then(|dir| std::fs::read_dir(dir).ok())
                    .map_or(0, |entries| {
                        u32::try_from(
                            entries
                                .flatten()
                                .filter(|entry| entry.path().is_dir())
                                .count(),
                        )
                        .unwrap_or(u32::MAX)
                    });
                Some(EnvironmentWorkspace {
                    environment_id: env.id,
                    dir: display(&dir),
                    memory_exists: memory.is_file(),
                    memory_file: display(&memory),
                    session_folders,
                })
            })
            .collect();
        WorkspacesWhereResult {
            root: display(&root),
            default_root: display(&imperium_config::default_workspaces_root(&self.paths)),
            environments,
            warning: self
                .workspaces_warning
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        }
    }

    /// Running sessions that have a scratch folder under `root`, with their status.
    fn sessions_with_scratch(&self, root: &Path) -> Vec<(String, SessionStatus)> {
        self.sessions()
            .into_iter()
            .filter_map(|session| {
                let environment = self.placement(session.id()).environment_id?;
                workspaces::session_dir(root, &environment, session.id())
                    .filter(|dir| dir.is_dir())
                    .map(|_| (session.id().to_string(), session.status().0))
            })
            .collect()
    }

    /// Moves the working folders to `target` (empty: the default) and makes it
    /// `workspaces.root`: the same as changing that setting.
    ///
    /// Sessions that are working (or waiting on the user) hold their scratch folder open, so
    /// the move is refused while any is; idle ones are restarted afterwards so their scratch
    /// folder is the new one. A failure at any step puts everything back.
    pub fn move_workspaces(&self, target: &str) -> Result<WorkspacesMoveResult, String> {
        let default = imperium_config::default_workspaces_root(&self.paths);
        let target = target.trim();
        let new = if target.is_empty() {
            default.clone()
        } else {
            PathBuf::from(target)
        };
        // Held while the data and the setting change, so no session starts, ends or is archived
        // on the old place meanwhile; released before the idle sessions are restarted.
        let (staged_report, affected, old) = {
            let _moving = self
                .workspaces_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // Read under the lock: a move that ran first has already changed the root.
            let old = self.workspaces_root();
            if std::env::var_os("IMPERIUM_WORKSPACES__ROOT").is_some() {
                return Err(
                    "IMPERIUM_WORKSPACES__ROOT sets the working folders' place and wins over \
                     the config file; unset it to move them"
                        .into(),
                );
            }
            if !new.is_absolute() {
                return Err(format!(
                    "{} is not an absolute path; give the full path of the folder",
                    new.display()
                ));
            }
            let affected = self.sessions_with_scratch(&old);
            let busy: Vec<&str> = affected
                .iter()
                .filter(|(_, status)| {
                    matches!(
                        status,
                        SessionStatus::Working
                            | SessionStatus::NeedsAttention
                            | SessionStatus::Paused
                            | SessionStatus::NeedsModel
                            | SessionStatus::Watching
                    )
                })
                .map(|(id, _)| id.as_str())
                .collect();
            if !busy.is_empty() && new != old {
                return Err(format!(
                    "these sessions are working or watching with their scratch folders: {}. Wait \
                     for them to finish or stop them, then move the folders",
                    busy.join(", ")
                ));
            }
            // Stage: the data is at the new place; any originals are still there.
            let staged = workspaces::stage(&old, &new, &mut |step: MoveProgress| {
                self.announce(
                    WORKSPACES_PROGRESS,
                    WorkspacesProgress {
                        phase: step.phase.into(),
                        files_done: step.files_done,
                        files_total: step.files_total,
                    },
                );
            })
            .map_err(|error| error.to_string())?;
            if staged.method == MoveMethod::Nothing && new == old {
                return Ok(WorkspacesMoveResult {
                    from: display(&old),
                    to: display(&new),
                    method: "nothing".into(),
                    files: 0,
                    bytes: 0,
                    warnings: Vec::new(),
                    restarted_sessions: Vec::new(),
                });
            }
            // Commit: name the new place in the setting. If that fails, put everything back.
            let value = if new == default {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(new.display().to_string())
            };
            if let Err(error) = imperium_config::set_user_setting(
                &self.paths,
                ROOT_SETTING,
                &value,
                std::env::vars(),
            ) {
                return Err(match staged.undo() {
                    Ok(()) => format!(
                        "could not save {ROOT_SETTING}: {error}. The folders were left where \
                         they were"
                    ),
                    Err(back) => format!(
                        "could not save {ROOT_SETTING}: {error}; and the move could not be \
                         undone ({back}). The data is at {}",
                        new.display()
                    ),
                });
            }
            let mut next = (*self.settings()).clone();
            next.workspaces.root = (new != default).then(|| new.clone());
            self.replace_settings(next);
            if let Err(error) = workspaces::record_root(self.paths.data_dir(), &new) {
                tracing::warn!(%error, "could not record where the working folders are");
            }
            *self
                .workspaces_warning
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
            // Finalise: delete the originals of a copy. The data is safe at the new place.
            let (method, files, bytes) = (staged.method, staged.files, staged.bytes);
            let warnings = staged.finish(&mut |step: MoveProgress| {
                self.announce(
                    WORKSPACES_PROGRESS,
                    WorkspacesProgress {
                        phase: step.phase.into(),
                        files_done: step.files_done,
                        files_total: step.files_total,
                    },
                );
            });
            ((method, files, bytes, warnings), affected, old)
        };
        let (method, files, bytes, mut warnings) = staged_report;
        // Restart the idle sessions whose scratch folder moved so they point at the new one.
        let mut restarted = Vec::new();
        for (id, _) in &affected {
            match self.restart_session(id) {
                Ok(()) => restarted.push(id.clone()),
                Err(error) => warnings.push(format!(
                    "session {id} could not be restarted and still points at the old folder: {error}"
                )),
            }
        }
        self.announce_environments();
        tracing::info!(from = %old.display(), to = %new.display(), files, "moved the working folders");
        Ok(WorkspacesMoveResult {
            from: display(&old),
            to: display(&new),
            method: match method {
                MoveMethod::Nothing => "nothing",
                MoveMethod::Rename => "rename",
                MoveMethod::Copy => "copy",
            }
            .into(),
            files,
            bytes,
            warnings,
            restarted_sessions: restarted,
        })
    }

    /// Continues a running session from its journal as after a daemon restart. The new session
    /// takes the old one's place only once it started, so a failure leaves the old one running;
    /// clients subscribed to it follow the replacement ([`crate::next_event`]).
    fn restart_session(&self, id: &str) -> Result<(), String> {
        let meta_path = self.meta_path(id);
        let meta: serde_json::Value = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or("its record is missing")?;
        match self.resume_from_meta(id, &meta, &self.journal_path(id), false) {
            Ok((session, _)) => {
                self.announce(SESSION_UPDATED, self.summary(&session));
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// At start: if the configured root differs from where the working folders were last put
    /// (the setting was edited in the file, or set by an environment variable), moves them
    /// there. On failure the daemon keeps using the old place and says so in
    /// `workspaces.where`.
    pub fn reconcile_workspaces(&self) {
        let _moving = self
            .workspaces_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let configured = self.workspaces_root();
        let default = imperium_config::default_workspaces_root(&self.paths);
        let recorded = workspaces::recorded_root(self.paths.data_dir()).unwrap_or(default);
        // Nothing at the recorded place: a move that already finished (and only failed to
        // record itself), or a root chosen before anything was kept.
        if recorded != configured && recorded.is_dir() {
            match workspaces::stage(&recorded, &configured, &mut |_| {}) {
                Ok(staged) => {
                    let files = staged.files;
                    // The setting is already saved; recording the place is the commit.
                    if let Err(error) = workspaces::record_root(self.paths.data_dir(), &configured)
                    {
                        let message = format!(
                            "could not record the working folders' new place ({error}); they \
                             stay at {}",
                            recorded.display()
                        );
                        tracing::warn!("{message}");
                        let _ = staged.undo();
                        self.keep_old_root(&recorded, message);
                        return;
                    }
                    tracing::info!(from = %recorded.display(), to = %configured.display(), files, "moved the working folders to the configured place");
                    for warning in staged.finish(&mut |_| {}) {
                        tracing::warn!("{warning}");
                    }
                    return;
                }
                Err(error) => {
                    let message = format!(
                        "{ROOT_SETTING} is {}, but the working folders could not be moved there \
                         ({error}); they stay at {}",
                        configured.display(),
                        recorded.display()
                    );
                    tracing::warn!("{message}");
                    self.keep_old_root(&recorded, message);
                    return;
                }
            }
        }
        if let Err(error) = workspaces::record_root(self.paths.data_dir(), &configured) {
            tracing::warn!(%error, "could not record where the working folders are");
        }
    }

    /// Keeps using `place` for this run, and says why in `workspaces.where`.
    fn keep_old_root(&self, place: &Path, message: String) {
        let mut next = (*self.settings()).clone();
        next.workspaces.root = Some(place.to_path_buf());
        self.replace_settings(next);
        *self
            .workspaces_warning
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(message);
    }

    /// `workspaces.open`: shows an environment's folder or `Memory.md` with the computer's own
    /// programs.
    pub fn open_workspace(&self, environment: &str, target: &str) -> Result<(), String> {
        if self.environments.get(environment).is_none() {
            return Err(format!("no environment {environment:?}"));
        }
        let root = self.workspaces_root();
        let path = match target {
            "folder" => workspaces::environment_dir(&root, environment),
            "memory" => workspaces::memory_file(&root, environment),
            other => return Err(format!("cannot open {other:?}; use folder or memory")),
        }
        .ok_or("the environment's id is not usable as a folder name")?;
        if target == "folder" {
            std::fs::create_dir_all(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        } else if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(&path, "").map_err(|error| format!("{}: {error}", path.display()))?;
        }
        open_with_system(&path)
    }
}

/// The path as people write it.
fn display(path: &Path) -> String {
    imperium_tools::plain_path(path)
}

/// Hands a file or folder to the operating system's default program for it.
fn open_with_system(path: &Path) -> Result<(), String> {
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("explorer");
        command.arg(path);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(path);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open {}: {error}", path.display()))
}
