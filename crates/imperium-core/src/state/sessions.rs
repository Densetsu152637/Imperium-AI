use super::*;

impl DaemonState {
    /// Sessions with a turn running: the user's, and background ones.
    pub(crate) fn working_sessions(&self) -> (u32, u32) {
        let sessions = self
            .sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held");
        let mut counts = (0, 0);
        for session in sessions.values() {
            if session.status().0 == SessionStatus::Working {
                if session.is_background() {
                    counts.1 += 1;
                } else {
                    counts.0 += 1;
                }
            }
        }
        counts
    }

    /// When a session's journal was last written, in milliseconds since the Unix epoch.
    pub(crate) fn session_activity_ms(&self, id: &str) -> Option<u64> {
        let modified = std::fs::metadata(self.journal_path(id))
            .ok()?
            .modified()
            .ok()?;
        let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
        u64::try_from(since.as_millis()).ok()
    }

    /// Re-opens sessions active within `agents.resume_days`, from their journals and saved
    /// conversations. Returns how many continued.
    pub fn resume_sessions(&self) -> usize {
        let dir = self.paths.data_dir().join("sessions");
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(
                self.settings().agents.resume_days * 86_400,
            ))
            .unwrap_or(std::time::UNIX_EPOCH);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return 0;
        };
        let mut resumed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".meta.json"))
                .map(str::to_string)
            else {
                continue;
            };
            let journal = dir.join(format!("{id}.jsonl"));
            let recent = std::fs::metadata(&journal)
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| modified >= cutoff);
            let meta: Option<serde_json::Value> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());
            // Archived sessions stay listed, however old, but do not run.
            if let Some(meta) = meta.as_ref().filter(|meta| meta["archived"] == true) {
                self.remember_archived(&id, meta, &journal);
                continue;
            }
            let (Some(meta), true) = (meta, recent) else {
                continue;
            };
            match self.resume_from_meta(&id, &meta, &journal, false) {
                Ok(_) => resumed += 1,
                Err(error) => tracing::warn!(session = %id, %error, "could not resume the session"),
            }
        }
        resumed
    }

    /// Continues one recorded session from its meta file and journal.
    pub(super) fn resume_from_meta(
        &self,
        id: &str,
        meta: &serde_json::Value,
        journal: &Path,
        worktree: bool,
    ) -> Result<(Arc<Session>, broadcast::Receiver<AgentEvent>), CreateError> {
        let id = id.to_string();
        {
            let journal = journal.to_path_buf();
            let project = meta["project_dir"].as_str().unwrap_or_default();
            // A model the session switched to is in its journal; else the one it started with.
            let switched = imperium_engine::journalled_model(&journal);
            let model = switched.as_deref().or(meta["model"].as_str());
            let options = SessionOptions {
                resume_id: Some(id.clone()),
                background: meta["background"].as_bool().unwrap_or(false),
                // Stays unattended after a restart even when its pre-approved tools are gone.
                unattended: recorded_unattended(meta),
                run_now: meta["run_now"].as_bool().unwrap_or(false),
                // A branch asking for a worktree of its own gets a new one.
                worktree,
                id: worktree.then(|| id.clone()),
                // A session that never had a model keeps waiting for one.
                may_wait_for_model: model.is_none(),
                worktrees: serde_json::from_value(meta["worktrees"].clone()).unwrap_or_default(),
                // The effort the session was set to, else (older sessions) the default.
                effort: meta["effort"]
                    .as_str()
                    .and_then(|effort| effort.parse().ok()),
                permission_mode: meta["permission_mode"]
                    .as_str()
                    .and_then(|mode| mode.parse().ok()),
                // A session an agent started stays under that agent's profile.
                agent_id: meta["agent_id"].as_str().map(str::to_string),
                agent_recorded: serde_json::from_value(meta["agent_permissions"].clone()).ok(),
                subfolder: meta["subfolder"].as_str().map(str::to_string),
                read_only: meta["read_only"].as_bool().unwrap_or(false),
                ..SessionOptions::default()
            };
            let created = match meta["environment_id"].as_str() {
                // Its environment as it is now, if it still exists.
                Some(env) if self.environments.get(env).is_some() => {
                    self.create_in_environment(env, model, options)
                }
                Some(_) => {
                    let recorded: Vec<String> = meta["folders"]
                        .as_array()
                        .map(|folders| {
                            folders
                                .iter()
                                .filter_map(|folder| folder.as_str().map(str::to_string))
                                .skip(1)
                                .collect()
                        })
                        .unwrap_or_default();
                    let options = SessionOptions {
                        folders: recorded,
                        ..options
                    };
                    self.create_session_with(project, model, options)
                }
                // Sessions from before environments: their folder becomes one.
                None => match self.environments.for_folder(project) {
                    Ok(env) => self.create_in_environment(&env.id, model, options),
                    Err(_) => self.create_session_with(project, model, options),
                },
            };
            let (session, events) = created?;
            // The pull request it was bound to, with its switches and last status. An older
            // record has none.
            if let Some(record) = meta.get("pull_request") {
                self.pr_monitor.restore(self, &session, record);
            }
            // A brief still waiting for a quiet machine is sent once the project agents start
            // ([`crate::project_agent::start`]).
            if let Some(brief) = meta["deferred_brief"]["text"].as_str() {
                lock_pending(&self.pending_briefs).push(PendingBrief {
                    project: project.to_string(),
                    session: Arc::clone(&session),
                    events: session.subscribe(),
                    brief: brief.to_string(),
                    until_unix_ms: meta["deferred_brief"]["until_unix_ms"]
                        .as_u64()
                        .unwrap_or(0),
                });
            }
            Ok((session, events))
        }
    }

    /// Briefs restored by [`DaemonState::resume_sessions`] that have not been sent yet.
    pub(crate) fn take_pending_briefs(&self) -> Vec<PendingBrief> {
        std::mem::take(&mut *lock_pending(&self.pending_briefs))
    }

    /// Stops a background session giving way to the load for the rest of its life, after a
    /// restart too.
    pub fn release_run_now(&self, session: &Session) {
        session.run_now().release();
        self.update_meta(session.id(), |meta| {
            meta.insert("run_now".into(), true.into());
        });
    }

    /// Records the brief a session gets once the machine is quiet (and the time it gets it
    /// anyway), so a restart does not lose it; `None` once it has been sent.
    pub(crate) fn set_deferred_brief(&self, id: &str, brief: Option<(&str, u64)>) {
        self.update_meta(id, |meta| match brief {
            Some((text, until_unix_ms)) => {
                meta.insert(
                    "deferred_brief".into(),
                    serde_json::json!({ "text": text, "until_unix_ms": until_unix_ms }),
                );
            }
            None => {
                meta.remove("deferred_brief");
            }
        });
    }

    /// Changes a remembered session's `.meta.json`; sessions that are not remembered have none.
    pub(super) fn update_meta(
        &self,
        id: &str,
        change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) {
        let path = self
            .paths
            .data_dir()
            .join("sessions")
            .join(format!("{id}.meta.json"));
        let Some(serde_json::Value::Object(mut meta)) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
        else {
            return;
        };
        change(&mut meta);
        if let Err(error) = std::fs::write(&path, serde_json::Value::Object(meta).to_string()) {
            tracing::warn!(path = %path.display(), %error, "could not record the session");
        }
    }

    /// Records a session's pull-request binding (its URL, switches and last status) with the
    /// session so it survives a restart; `None` removes it. A session that is gone, or not
    /// remembered, has nothing to record.
    pub(crate) fn record_pull_request(&self, id: &str, record: Option<serde_json::Value>) {
        if record.is_some() && self.session(id).is_none() {
            return;
        }
        self.update_meta(id, |meta| match record {
            Some(record) => {
                meta.insert("pull_request".into(), record);
            }
            None => {
                meta.remove("pull_request");
            }
        });
    }

    /// The configured resume policy for usage limits.
    pub(super) fn resume_policy(&self) -> ResumePolicy {
        let resume = &self.settings().resume;
        ResumePolicy {
            mode: resume.mode.clone(),
            at_time: resume.at_time.clone(),
            retry_every_minutes: resume.retry_every_minutes,
            max_wait_hours: resume.max_wait_hours,
        }
    }

    /// Pull requests sessions follow that are not yet merged or closed, by session id.
    pub fn watched(&self) -> Vec<(String, String, u64)> {
        self.pr_monitor.watched()
    }

    /// Binds a session to a pull request (the old `watch`, now the same as
    /// `pull_requests.bind` without its repository check, for the daemon's own callers); the
    /// monitor marks the session merged when the pull request merges. `None` unbinds.
    pub fn watch(&self, session: &Session, target: Option<(String, u64)>) {
        match target {
            Some((repo, number)) => self.pr_monitor.bind(self, session, &repo, number),
            None => {
                self.pr_monitor.unbind(self, session);
            }
        }
    }

    /// Stops and forgets a session so it does not continue after a restart, removing its
    /// worktrees when asked (their branches stay). A persistent agent's session is refused:
    /// the agent is deleted in its environment instead.
    pub fn delete_session(
        &self,
        id: &str,
        remove_worktrees: bool,
    ) -> Result<SessionDeleteResult, String> {
        let _workspaces = self
            .workspaces_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let placement = self
            .placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            // An archived session is deleted the same way, from its record.
            .or_else(|| {
                self.take_archived(id).map(|summary| Placement {
                    environment_id: summary.environment_id,
                    worktrees: summary.worktrees,
                    ..Placement::default()
                })
            });
        let Some(placement) = placement else {
            return Err(format!("no session {id}"));
        };
        if placement.persistent {
            return Err(
                "this is a persistent agent of its environment; delete the agent in the environment".into(),
            );
        }
        self.forget_session(id);
        // A pull request it had turned auto-merge on for is not watched any more: off.
        self.pr_monitor().forget(self, id);
        self.forget_eval(id);
        // Files copied into its attachments folder go with it.
        let _ = std::fs::remove_dir_all(crate::attachments::store_dir(self.paths.data_dir(), id));
        let sessions = self.paths.data_dir().join("sessions");
        let _ = std::fs::remove_file(sessions.join(format!("{id}.meta.json")));
        // So do its checkpoints: their refs in each repository and their records.
        crate::checkpoints::forget(self.paths.data_dir(), id);
        // So does its scratch folder.
        if let Some(environment) = &placement.environment_id {
            self.remove_session_scratch(environment, id);
        }
        // Its agents' kept state goes with it.
        let agents = imperium_engine::agents_dir(&sessions.join(format!("{id}.jsonl")));
        if agents.exists()
            && let Err(error) = std::fs::remove_dir_all(&agents)
        {
            tracing::warn!(path = %agents.display(), %error, "could not delete the session's agent state");
        }
        // So do the large tool results it saved to files.
        let results = imperium_engine::results_dir(&sessions.join(format!("{id}.jsonl")));
        if results.exists()
            && let Err(error) = std::fs::remove_dir_all(&results)
        {
            tracing::warn!(path = %results.display(), %error, "could not delete the session's saved tool results");
        }
        let mut result = SessionDeleteResult::default();
        // The worktrees of its writer subagents go first (they were made from the session's
        // folders, which may be its own worktrees), on the same terms; branches stay.
        let journal = sessions.join(format!("{id}.jsonl"));
        let mut writer_failures = Vec::new();
        if remove_worktrees {
            let (removed, failed) = crate::writers::GitWriterTrees::remove_recorded(&journal);
            result.removed_worktrees.extend(removed);
            writer_failures.clone_from(&failed);
            result.failed.extend(failed);
        }
        // The record goes, except for the writers whose worktrees could not be removed, which it
        // keeps naming for cleaning up by hand.
        crate::writers::GitWriterTrees::settle(&journal, remove_worktrees, &writer_failures);
        if remove_worktrees && !placement.worktrees.is_empty() {
            // A worktree another session also works in (a branch shares its source's) is
            // theirs too, and may hold uncommitted work: it stays.
            let others = self.worktree_paths_in_use();
            let own: Vec<_> = placement
                .worktrees
                .iter()
                .filter(|tree| !others.iter().any(|path| same_path(path, &tree.path)))
                .cloned()
                .collect();
            if !own.is_empty() {
                let (removed, failed) = crate::worktrees::remove(&own);
                result.removed_worktrees.extend(removed);
                result.failed.extend(failed);
            }
        }
        self.announce(
            imperium_protocol::methods::SESSION_DELETED,
            SessionDeleteParams {
                session_id: id.to_string(),
                remove_worktrees,
            },
        );
        Ok(result)
    }

    /// Where sessions put their worktrees (`sessions.worktree_dir`), read afresh so a change in
    /// Settings applies to the next one; `None` puts them inside their repositories.
    pub(super) fn worktree_dir(&self) -> Option<PathBuf> {
        let fresh = crate::connectors::saved_settings(&self.paths);
        let current = self.settings();
        fresh
            .as_ref()
            .unwrap_or(&*current)
            .sessions
            .worktree_dir
            .clone()
            .filter(|dir| !dir.as_os_str().is_empty())
    }

    /// Where the sessions of `environment_id` put their worktrees: the environment's own folder
    /// when it sets one, else [`Self::worktree_dir`].
    pub fn worktree_dir_for(&self, environment_id: Option<&str>) -> Option<PathBuf> {
        environment_id
            .and_then(|id| self.environments.get(id))
            .and_then(|env| env.worktree_dir)
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| self.worktree_dir())
    }

    /// Worktree folders that running and archived sessions work in.
    pub(super) fn worktree_paths_in_use(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .flat_map(|placement| placement.worktrees.iter().map(|tree| tree.path.clone()))
            .collect();
        paths.extend(
            self.archived_sessions()
                .into_iter()
                .flat_map(|summary| summary.worktrees.into_iter().map(|tree| tree.path)),
        );
        // Sessions with a meta file on disk that are neither running nor archived (not resumed
        // yet, or past `agents.resume_days`) still own their worktrees.
        if let Ok(entries) = std::fs::read_dir(self.paths.data_dir().join("sessions")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".meta.json"))
                {
                    continue;
                }
                let Some(meta) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                else {
                    continue;
                };
                if let Some(trees) = meta["worktrees"].as_array() {
                    paths.extend(
                        trees
                            .iter()
                            .filter_map(|tree| tree["path"].as_str().map(str::to_string)),
                    );
                }
            }
        }
        paths
    }

    /// Test hook: `session.compare` fails after creating its `branches`th branch. Compiled only
    /// for this crate's tests or with the `testing` feature.
    #[cfg(any(test, feature = "testing"))]
    pub fn inject_compare_failure(&self, branches: usize) {
        self.compare_fault
            .store(branches, std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn meta_path(&self, id: &str) -> PathBuf {
        self.paths
            .data_dir()
            .join("sessions")
            .join(format!("{id}.meta.json"))
    }

    /// Sets a session's reasoning effort from its agents' next model calls on, records it with
    /// the session so it survives a restart, and tells every client.
    pub fn set_effort(&self, session: &Session, effort: imperium_api::Effort) {
        session.set_effort(effort);
        // Sessions that are not remembered (project agents) have no meta file; their effort
        // lasts until the daemon stops.
        self.update_meta(session.id(), |meta| {
            meta.insert("effort".into(), effort.as_str().into());
        });
        self.announce(
            imperium_protocol::methods::SESSION_UPDATED,
            self.summary(session),
        );
    }

    /// Sets a session's permission mode from its agents' next calls on, records it with the
    /// session so it survives a restart, and tells every client.
    pub fn set_permission_mode(&self, session: &Session, mode: imperium_api::PermissionMode) {
        session.set_permission_mode(mode);
        self.update_meta(session.id(), |meta| {
            meta.insert("permission_mode".into(), mode.as_str().into());
        });
        tracing::info!(session = %session.id(), mode = %mode, "the session's permission mode changed");
        self.announce(
            imperium_protocol::methods::SESSION_UPDATED,
            self.summary(session),
        );
    }

    /// Whether the model `spec` names (a model table id or `<endpoint>:<model>`) can reason:
    /// the table row's `reasoning` setting or tags, else a local model's chat template (or
    /// its `reasoning` setting), else whether its name is a known reasoning model.
    pub fn model_reasons(&self, spec: &str) -> bool {
        let settings = self.settings();
        let row = settings
            .models
            .catalog
            .iter()
            .find(|entry| entry.spec == spec || entry.id == spec);
        if let Some(set) = row.and_then(imperium_config::CatalogEntry::reasoning_setting) {
            return set;
        }
        let spec = row.map_or(spec, |entry| entry.spec.as_str());
        let (endpoint, model) = spec.split_once(':').unwrap_or(("", spec));
        let local = endpoint == imperium_config::LLAMACPP_ENDPOINT
            || endpoint == imperium_config::VLLM_ENDPOINT;
        let runtime = self.local.get().filter(|_| local);
        runtime
            .and_then(|runtime| LocalModels::reasons(&*runtime.supervisor, model))
            .unwrap_or_else(|| imperium_api::reasoning_by_name(model))
    }

    /// Pins or unpins a session, records it with the session so it survives a restart, and
    /// tells every client.
    pub fn set_pinned(&self, session: &Session, pinned: bool) -> std::io::Result<()> {
        let path = self.meta_path(session.id());
        // Sessions that are not remembered (project agents) have no meta file; their pin lasts
        // until the daemon stops.
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut meta: serde_json::Value =
                serde_json::from_str(&text).map_err(std::io::Error::other)?;
            meta["pinned"] = serde_json::Value::Bool(pinned);
            std::fs::write(&path, meta.to_string())?;
        }
        self.placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session.id().to_string())
            .or_default()
            .pinned = pinned;
        self.announce(
            imperium_protocol::methods::SESSION_UPDATED,
            self.summary(session),
        );
        Ok(())
    }

    /// A session as lists show it, with its environment and folders.
    pub fn summary(&self, session: &Session) -> SessionSummary {
        let placement = self
            .placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session.id())
            .cloned()
            .unwrap_or_default();
        SessionSummary {
            environment_id: placement.environment_id,
            folders: placement.folders,
            persistent: placement.persistent,
            worktrees: placement.worktrees,
            pinned: placement.pinned,
            branched_from: placement.branched_from,
            comparison_id: placement.comparison_id,
            read_only: placement.read_only,
            started_by: placement.started_by,
            pull_request_url: self.pr_monitor.url_of(session.id()),
            created_at: created_at(&self.journal_path(session.id())),
            title: placement
                .title
                .unwrap_or_else(|| titles::from_first_message(&session.title())),
            reasoning: session
                .model()
                .is_some_and(|model| self.model_reasons(&model)),
            ..summary(session)
        }
    }

    /// Kills every session's background commands and waits up to `timeout` for the kills, so
    /// none outlives the daemon (called on shutdown).
    pub async fn stop_background_processes(&self, timeout: Duration) {
        let mut sessions = self.sessions();
        sessions.extend(self.project_agents.sessions());
        let mut stops = tokio::task::JoinSet::new();
        for session in &sessions {
            let processes = session.workspace().processes().clone();
            stops.spawn(async move { processes.stop_all_and_wait(timeout).await });
        }
        let _ = tokio::time::timeout(timeout, stops.join_all()).await;
    }

    /// Every running session, ordered by id.
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        let mut sessions: Vec<_> = self
            .sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .values()
            .cloned()
            .collect();
        sessions.sort_by(|a, b| a.id().cmp(b.id()));
        sessions
    }

    /// Stops tracking a session; once its other handles are gone it ends after its current
    /// turn. Its journal stays, so it can be resumed later.
    pub fn forget_session(&self, id: &str) {
        let removed = self
            .sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .remove(id);
        self.forget_browser(id);
        // Its background commands end with it, even while something still holds the session.
        if let Some(session) = removed {
            session.stop_processes();
        }
        self.placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        self.checkpoints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }

    /// A running session's checkpoints, when it records them.
    pub(crate) fn checkpoints_of(
        &self,
        id: &str,
    ) -> Option<Arc<crate::checkpoints::SessionCheckpoints>> {
        self.checkpoints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Starts a new session from session `id` as it was when its journal was `journal_bytes`
    /// long, with `messages` (its saved root conversation then) as its conversation. Returns the
    /// new session's id; the original is left as it is.
    pub(crate) fn fork_session(
        &self,
        id: &str,
        journal_bytes: u64,
        messages: Option<Vec<u8>>,
    ) -> Result<String, String> {
        let sessions = self.paths.data_dir().join("sessions");
        let mut meta: serde_json::Value = std::fs::read_to_string(self.meta_path(id))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .ok_or("the session is not recorded, so it cannot be forked")?;
        let journal = std::fs::read(sessions.join(format!("{id}.jsonl")))
            .map_err(|error| format!("could not read the session's journal: {error}"))?;
        let limit = usize::try_from(journal_bytes)
            .unwrap_or(usize::MAX)
            .min(journal.len());
        // Whole lines only.
        let cut = journal[..limit]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1);
        let new_id = new_session_id().map_err(|error| error.to_string())?;
        let new_journal = sessions.join(format!("{new_id}.jsonl"));
        std::fs::write(&new_journal, &journal[..cut]).map_err(|error| error.to_string())?;
        if let Some(messages) = messages {
            std::fs::write(imperium_engine::messages_path(&new_journal), messages)
                .map_err(|error| error.to_string())?;
        }
        if let Some(fields) = meta.as_object_mut() {
            let title = fields
                .get("title")
                .and_then(serde_json::Value::as_str)
                .filter(|title| !title.trim().is_empty())
                .map(|title| format!("{title} (rewound)"));
            if let Some(title) = title {
                fields.insert("title".into(), title.into());
            }
            fields.insert("pinned".into(), false.into());
            fields.insert("forked_from".into(), id.into());
            fields.remove("deferred_brief");
            // The pull request belongs to the session it was bound to.
            fields.remove("pull_request");
        }
        std::fs::write(self.meta_path(&new_id), meta.to_string())
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.resume_from_meta(&new_id, &meta, &new_journal, false) {
            let _ = std::fs::remove_file(self.meta_path(&new_id));
            return Err(error.to_string());
        }
        Ok(new_id)
    }

    /// A running session's browser, which the user may take over.
    pub fn browser_of(&self, id: &str) -> Option<BrowserSetup> {
        self.browsers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Lets go of a session's browser handle, so the browser ends with the session.
    pub(crate) fn forget_browser(&self, id: &str) {
        self.browsers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .get(id)
            .cloned()
    }
}
