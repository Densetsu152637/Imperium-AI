use super::*;

impl DaemonState {
    /// Records an agent's changed profile with the sessions it binds, so a session of an agent
    /// deleted later is not looser than the agent was.
    pub(crate) fn agent_permissions_changed(
        &self,
        agent_id: &str,
        permissions: &imperium_protocol::agents::AgentPermissions,
    ) {
        let bound: Vec<String> = self
            .placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, placement)| placement.agent_id.as_deref() == Some(agent_id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in bound {
            self.update_meta(&id, |meta| {
                meta.insert(
                    "agent_permissions".into(),
                    serde_json::to_value(permissions).unwrap_or_default(),
                );
            });
        }
    }

    pub fn project_agents(&self) -> &ProjectAgents {
        &self.project_agents
    }

    /// Whether a session of the user's own (not background, not a persistent agent) in the
    /// environment has a turn running, waits on the user, or had activity within `idle`.
    pub(crate) fn environment_user_active(&self, environment_id: &str, idle: Duration) -> bool {
        for session in self.sessions() {
            let placement = self
                .placements
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(session.id())
                .cloned()
                .unwrap_or_default();
            if placement.environment_id.as_deref() != Some(environment_id) {
                continue;
            }
            // Talking to one of the environment's persistent agents is being active in it.
            if placement.persistent {
                let talked = self
                    .project_agents
                    .environment_agents()
                    .into_iter()
                    .find(|(_, _, session_id)| session_id == session.id())
                    .is_some_and(|(_, agent_id, _)| {
                        self.initiative.chatted_within(&agent_id, idle_ms(idle))
                    });
                if talked {
                    return true;
                }
                continue;
            }
            if session.is_background() {
                continue;
            }
            if matches!(
                session.status().0,
                SessionStatus::Working | SessionStatus::NeedsAttention
            ) {
                return true;
            }
            let recent = self
                .session_activity_ms(session.id())
                .is_some_and(|at| imperium_api::time::unix_ms().saturating_sub(at) < idle_ms(idle));
            if recent {
                return true;
            }
        }
        false
    }

    /// Starts the agents of projects with `agent = true`, and the persistent agents of
    /// environments that have been given any.
    pub fn start_project_agents(self: &Arc<Self>) {
        crate::project_agent::start(self);
        crate::project_agent::start_environment_agents(self);
    }

    /// Starts an environment's persistent agents that are not running and stops the ones that
    /// were deleted.
    pub fn sync_environment_agent(self: &Arc<Self>, environment_id: &str) {
        crate::project_agent::sync_environment_agent(self, environment_id);
    }

    /// Wakes a project's agent with a message; false when the project has no agent.
    pub fn tell_project_agent(&self, project: &Path, text: String) -> bool {
        self.project_agents
            .get(project)
            .is_some_and(|agent| agent.send(text).is_ok())
    }

    /// Wakes the agents of environment `environment_id` with a message: the one persistent agent
    /// `to_agent` names (by name or id, ignoring case), else all of them and the project agent
    /// attached to the environment (`[[projects]]` `environment`). Returns how many got it.
    pub fn tell_environment_agents(
        &self,
        environment_id: &str,
        to_agent: Option<&str>,
        text: &str,
    ) -> usize {
        let Some(env) = self.environments.get(environment_id) else {
            return 0;
        };
        let wanted: Vec<String> = env
            .agents
            .iter()
            .filter(|agent| {
                to_agent.is_none_or(|name| {
                    agent.id == name
                        || agent.name.trim().to_lowercase() == name.trim().to_lowercase()
                })
            })
            .map(|agent| agent.id.clone())
            .collect();
        let mut delivered = 0;
        for (agent_id, session) in self.project_agents.agents_of(environment_id) {
            if wanted.contains(&agent_id) && session.send(text.to_string()).is_ok() {
                delivered += 1;
            }
        }
        if to_agent.is_none()
            && self
                .project_agents
                .get(&crate::project_agent::environment_key(environment_id))
                .is_some_and(|agent| agent.send(text.to_string()).is_ok())
        {
            delivered += 1;
        }
        delivered
    }

    /// What environment `environment_id`'s own configuration files change, for
    /// `environments.config`.
    pub fn environment_config(&self, environment_id: &str) -> EnvironmentConfigResult {
        let folder = self
            .environments
            .get(environment_id)
            .filter(|env| !env.runtime.is_remote())
            .and_then(|env| env.folders.first().cloned());
        let trusted = folder
            .as_deref()
            .is_some_and(|folder| self.settings().mcp.trusts(Path::new(folder)));
        let settings = self.environment_settings(Some(environment_id));
        EnvironmentConfigResult {
            folder,
            files: settings
                .files
                .iter()
                .map(|file| file.display().to_string())
                .collect(),
            trusted,
            applied: settings.applied,
            refused: settings.refused,
        }
    }

    /// The settings a new session in `environment_id` starts with: the user's, narrowed by the
    /// environment's own `.imperium/config.toml` in its first folder, which can only narrow
    /// them ([`EnvironmentConfig`]). Sessions outside an environment, on a remote runtime, or
    /// in an environment without a folder have no such files.
    pub fn environment_settings(&self, environment_id: Option<&str>) -> EnvironmentSettings {
        let base = self.settings();
        let folder = environment_id
            .and_then(|id| self.environments.get(id))
            .filter(|env| !env.runtime.is_remote())
            .and_then(|env| env.folders.first().cloned().map(|folder| (env.id, folder)));
        match folder {
            Some((id, folder)) => EnvironmentConfig::load(Path::new(&folder), &id).apply(&base),
            None => EnvironmentSettings::unchanged((*base).clone()),
        }
    }

    pub fn environments(&self) -> &Environments {
        &self.environments
    }

    /// The environments, with which of them can start sessions on git worktrees.
    pub fn environment_list(&self) -> imperium_protocol::methods::EnvironmentListResult {
        let environments = self.environments.list();
        let git = environments
            .iter()
            .filter(|env| env.runtime == imperium_protocol::methods::EnvironmentRuntime::Local)
            .filter(|env| {
                env.folders
                    .iter()
                    .any(|folder| crate::worktrees::in_repository(Path::new(folder)))
            })
            .map(|env| env.id.clone())
            .collect();
        imperium_protocol::methods::EnvironmentListResult { environments, git }
    }

    /// Applies environment `id`'s saved rules to its running sessions (its grants apply to
    /// sessions started from now on) and tells every client.
    pub fn environment_permissions_changed(&self, id: &str) {
        if let Some(env) = self.environments.get(id) {
            self.permissions.set_environment(id, &env.permissions.rules);
        }
        self.announce_environments();
    }

    /// Refuses file-tool writes into the plugin folders every known environment's merged settings
    /// list (a trusted project's `.imperium/config.toml` can add folders, which skip the load-time
    /// gate). Run when the daemon starts and whenever the environments change, so a folder is
    /// guarded before any session in it loads; a session's own load adds its folders too.
    pub(crate) fn guard_environment_plugin_folders(&self) {
        for env in self.environments.list() {
            let merged = self.environment_settings(Some(&env.id));
            for dir in &merged.settings.extensions.plugin_dirs {
                imperium_policy::permissions::protect_dir(dir);
                imperium_policy::permissions::refuse_writes_under(dir);
            }
        }
    }

    /// Tells every client the environments changed.
    pub fn announce_environments(&self) {
        self.guard_environment_plugin_folders();
        self.announce(
            imperium_protocol::methods::ENVIRONMENTS_CHANGED,
            self.environment_list(),
        );
    }
}
