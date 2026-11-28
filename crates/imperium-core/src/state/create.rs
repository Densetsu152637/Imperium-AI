use super::*;

impl DaemonState {
    /// The workspace for a session in `project_dir` (plus `folders`) under `runtime`, with the
    /// path grants and permission rules of `environment_id` and the global ones. A remote
    /// runtime gets only the environment's grants, since the global ones are paths on this
    /// machine.
    pub(super) fn workspace(
        &self,
        project_dir: &str,
        folders: &[String],
        runtime: &EnvironmentRuntime,
        environment_id: Option<&str>,
        isolation: &IsolationSettings,
        fixed_rules: Vec<imperium_api::PermissionRule>,
    ) -> Result<Workspace, CreateError> {
        let env = environment_id.and_then(|id| self.environments.get(id));
        let permissions = self
            .permissions
            .for_session(
                env.as_ref()
                    .map(|env| (env.id.as_str(), env.permissions.rules.as_slice())),
            )
            .with_fixed_rules(fixed_rules);
        let mut grants: Vec<imperium_api::PathGrant> = env
            .iter()
            .flat_map(|env| &env.permissions.grants)
            .map(crate::permissions::grant_from_wire)
            .collect();
        Ok(self
            .bare_workspace(project_dir, folders, runtime, isolation, &mut grants)?
            .with_grants(&grants)
            .with_permissions(permissions))
    }

    /// The workspace's folders and sandbox; adds the global grants to `grants` for a local one.
    pub(super) fn bare_workspace(
        &self,
        project_dir: &str,
        folders: &[String],
        runtime: &EnvironmentRuntime,
        isolation: &IsolationSettings,
        grants: &mut Vec<imperium_api::PathGrant>,
    ) -> Result<Workspace, CreateError> {
        let remote = match runtime {
            EnvironmentRuntime::Local
            | EnvironmentRuntime::Docker {
                container: None, ..
            } => None,
            EnvironmentRuntime::Docker {
                container: Some(container),
                ..
            } => Some(Remote::Docker {
                container: container.clone(),
            }),
            EnvironmentRuntime::Wsl { distro } => Some(Remote::Wsl {
                distro: distro.clone(),
            }),
            EnvironmentRuntime::Ssh { host } => Some(Remote::Ssh { host: host.clone() }),
        };
        // `docker exec` needs the Docker socket too, which a server does not have to give.
        if self.settings().daemon.server && matches!(remote, Some(Remote::Docker { .. })) {
            return Err(CreateError::UnavailableOnServer(
                imperium_protocol::jsonrpc::server_features::CONTAINER_TIER,
            ));
        }
        if let Some(remote) = remote {
            let all: Vec<String> = std::iter::once(project_dir.to_string())
                .chain(folders.iter().cloned())
                .collect();
            return Workspace::remote(remote, &all, self.runner())
                .map_err(|error| CreateError::Runtime(error.to_string()));
        }
        // A Docker image without a container: each command runs in a fresh container with the
        // folders mounted, as the container isolation tier does.
        let sandbox = match runtime {
            EnvironmentRuntime::Docker { image, .. } => Sandbox::Container {
                runtime: "docker".into(),
                image: image.clone(),
                network: isolation.network,
            },
            _ => sandbox_for(isolation),
        };
        // A server has no Docker socket to give (it would be root on the host): neither the
        // container tier nor an image runtime starts there.
        if self.settings().daemon.server && matches!(sandbox, Sandbox::Container { .. }) {
            return Err(CreateError::UnavailableOnServer(
                imperium_protocol::jsonrpc::server_features::CONTAINER_TIER,
            ));
        }
        grants.extend(self.permissions.global_grants());
        Ok(Workspace::new(Path::new(project_dir))
            .map_err(|error| CreateError::Project(project_dir.to_string(), error))?
            .with_folders(folders)
            .map_err(|error| CreateError::Project(folders.join(", "), error))?
            .with_sandbox(sandbox))
    }

    /// The profile of the persistent agent `agent_id` of `environment_id`, as the agent has it
    /// now, and the profile as the protocol carries it. An agent that is gone keeps the profile
    /// its running sessions hold; after a restart the one recorded with the session, and with
    /// no record the strictest profile there is: a session never gets looser than its agent.
    pub(super) fn agent_profile(
        &self,
        environment_id: Option<&str>,
        agent_id: &str,
        recorded: Option<&imperium_protocol::agents::AgentPermissions>,
    ) -> (
        imperium_policy::AgentProfile,
        imperium_protocol::agents::AgentPermissions,
    ) {
        let live = environment_id
            .and_then(|id| self.environments.get(id))
            .and_then(|env| env.agents.into_iter().find(|agent| agent.id == agent_id))
            .map(|agent| agent.permissions);
        let permissions = match live {
            Some(permissions) => permissions,
            None => match self.permissions.known_agent(agent_id) {
                Some(known) => return known,
                None => recorded
                    .cloned()
                    .unwrap_or_else(crate::permissions::strictest_agent_permissions),
            },
        };
        (
            self.permissions.agent_profile(agent_id, &permissions),
            permissions,
        )
    }

    /// Media backends from `[media]`, using `[endpoints]` for URLs and keys.
    pub(super) fn media(&self) -> Option<Arc<Media>> {
        let all = self.settings();
        let settings = &all.media;
        let service = |endpoint: &str, model: &str| -> Option<Service> {
            let endpoint_settings = all.endpoints.get(endpoint)?;
            let api_key = endpoint_settings
                .api_key
                .as_ref()
                .and_then(|reference| self.secrets.load(&reference.secret).ok().flatten());
            Some(Service {
                base_url: endpoint_settings.base_url.trim_end_matches('/').to_string(),
                api_key,
                model: model.to_string(),
            })
        };
        let image = settings.image.as_ref().and_then(|image| {
            let config = ImageConfig {
                comfyui_url: image.comfyui_url.as_deref(),
                workflow: image.workflow.as_deref(),
                service: &|| service(image.endpoint.as_deref()?, &image.model),
            };
            ImageGenerators::default().build(&image.backend, &config)
        });
        let speech = settings.speech.as_ref().and_then(|speech| {
            Some((
                service(&speech.endpoint, &speech.model)?,
                speech.voice.clone().unwrap_or_else(|| "alloy".into()),
            ))
        });
        let transcription = settings
            .transcription
            .as_ref()
            .and_then(|transcription| service(&transcription.endpoint, &transcription.model));
        let media = Media {
            image,
            speech,
            transcription,
        };
        (!media.is_empty()).then(|| Arc::new(media))
    }

    /// Web tools from `[web]`; a provider whose key is missing is left out with a warning.
    /// `downloads` is the folder `browser_download` saves into unless `web.download_dir` names
    /// one, or `None` where the session cannot take downloads (a remote or read-only one).
    /// The folder is judged as a write by the session's workspace only when it is the default
    /// one (a folder the user named in `web.download_dir` is theirs to choose).
    pub(super) fn web_tools(&self, downloads: Option<(PathBuf, Workspace)>) -> Option<WebTools> {
        let web = &self.settings().web;
        if !web.enabled {
            return None;
        }
        // A provider's own key, else the shared `web.api_key`.
        let key = |provider: &str| {
            let own = match provider {
                "brave" => web.brave_key.as_ref(),
                "tavily" => web.tavily_key.as_ref(),
                "kagi" => web.kagi_key.as_ref(),
                "exa" => web.exa_key.as_ref(),
                _ => None,
            };
            let name = &own.or(web.api_key.as_ref())?.secret;
            self.secrets.load(name).ok().flatten()
        };
        let config = SearchConfig {
            searxng_url: &web.searxng_url,
            api_key: &key,
        };
        // `web.search` first, then each `web.search_fallback` once, in order. A provider whose
        // URL or key is missing is left out with a warning, never with its settings logged.
        let registry = SearchProviders::default();
        let mut chain: Vec<(String, Arc<dyn imperium_web::SearchProvider>)> = Vec::new();
        for name in std::iter::once(&web.search).chain(&web.search_fallback) {
            if name == "none" || chain.iter().any(|(known, _)| known == name) {
                continue;
            }
            match registry.build(name, &config) {
                Some(provider) => chain.push((name.clone(), provider)),
                None => {
                    tracing::warn!(provider = %name, "web search is not configured completely (URL or key missing)");
                }
            }
        }
        let browser = web
            .browser
            .then(|| {
                web.browser_path
                    .clone()
                    .or_else(imperium_web::browser::find_browser)
            })
            .flatten()
            .map(|program| BrowserSetup {
                program,
                profile_dir: self.paths.data_dir().join("browser"),
                headless: web.headless,
                instance: Arc::default(),
                takeover: Arc::default(),
            });
        Some(WebTools {
            web: Web::with_chain(
                chain,
                FetchOptions {
                    cache_dir: Some(self.paths.cache_dir().join("web")),
                    cache_ttl: Duration::from_secs(web.cache_ttl_s),
                    cache_max_bytes: web.cache_max_mb.saturating_mul(1024 * 1024),
                    respect_robots: web.robots == "respect",
                    robots_fail_closed: web.robots_unreachable == "deny",
                    archive_base: web
                        .archive_fallback
                        .then(|| imperium_web::WAYBACK_URL.to_string()),
                },
            ),
            allow_domains: web.allow_domains.clone(),
            downloads: downloads.filter(|_| web.download_max_mb > 0).map(
                |(default_dir, workspace)| {
                    let chosen = web
                        .download_dir
                        .clone()
                        .filter(|dir| !dir.as_os_str().is_empty());
                    imperium_web::Downloads {
                        workspace: chosen.is_none().then_some(workspace),
                        dir: chosen.unwrap_or(default_dir),
                        max_bytes: web.download_max_mb.saturating_mul(1024 * 1024),
                    }
                },
            ),
            browser,
        })
    }

    /// The permission mode an unattended session (a background one, or one with pre-approved
    /// tools) starts in with `settings` (an environment's narrowed ones): `default`, never the
    /// user's own `permissions.mode` (plan would stall it), but a stricter mode the
    /// environment's own file asked for.
    pub(crate) fn unattended_mode(&self, settings: &Settings) -> imperium_api::PermissionMode {
        let strictness = |mode: imperium_api::PermissionMode| {
            imperium_api::PermissionMode::ALL
                .iter()
                .position(|known| *known == mode)
        };
        let default = imperium_api::PermissionMode::Default;
        let narrowed = settings.permissions.mode != self.settings().permissions.mode;
        if narrowed && strictness(settings.permissions.mode) < strictness(default) {
            settings.permissions.mode
        } else {
            default
        }
    }

    /// Starts a session and returns it with a subscription that sees all of its events.
    pub fn create_session(
        &self,
        project_dir: &str,
        model: Option<&str>,
    ) -> Result<(Arc<Session>, broadcast::Receiver<AgentEvent>), CreateError> {
        self.create_session_with(project_dir, model, SessionOptions::default())
    }

    /// Runs `prompt` in a new session and waits for the root's turn to end:
    /// `(ok, the reply or error, session id)`. Permission requests become approval requests.
    pub async fn run_prompt(
        self: &Arc<Self>,
        project_dir: &str,
        model: Option<&str>,
        prompt: String,
        options: SessionOptions,
        timeout: std::time::Duration,
    ) -> (bool, String, Option<String>) {
        let source = options.budget_source.clone();
        let created = self.create_session_with(project_dir, model, options);
        if let (Ok((session, _)), Some(source)) = (&created, source) {
            self.budgets().tag_session(session.id(), &source);
        }
        self.run_created(created, prompt, timeout).await
    }

    /// Like [`DaemonState::run_prompt`], in an environment: all its folders, its permissions
    /// and its own configuration file apply, as to a session started there by hand.
    pub async fn run_prompt_in_environment(
        self: &Arc<Self>,
        environment_id: &str,
        model: Option<&str>,
        prompt: String,
        options: SessionOptions,
        timeout: std::time::Duration,
    ) -> (bool, String, Option<String>) {
        let created = self.create_in_environment(environment_id, model, options);
        self.run_created(created, prompt, timeout).await
    }

    pub(super) async fn run_created(
        self: &Arc<Self>,
        created: Result<(Arc<Session>, broadcast::Receiver<AgentEvent>), CreateError>,
        prompt: String,
        timeout: std::time::Duration,
    ) -> (bool, String, Option<String>) {
        let (session, events) = match created {
            Ok(created) => created,
            Err(error) => return (false, error.to_string(), None),
        };
        let session_id = Some(session.id().to_string());
        if let Err(stop) = crate::budgets::check(self, session.id(), false) {
            return (false, stop.message, session_id);
        }
        if session.send(prompt).is_err() {
            return (
                false,
                "the session closed before it started".into(),
                session_id,
            );
        }
        match self.wait_for_turn(&session, events, timeout).await {
            Ok(reply) => (true, reply, session_id),
            Err(message) => (false, message, session_id),
        }
    }

    /// Waits for the root agent's turn to end, returning its reply. Permission requests from
    /// any of the session's agents are queued as approval requests, since nobody may be
    /// watching the session.
    pub async fn wait_for_turn(
        self: &Arc<Self>,
        session: &Arc<Session>,
        mut events: broadcast::Receiver<AgentEvent>,
        timeout: std::time::Duration,
    ) -> Result<String, String> {
        let mut reply = String::new();
        let outcome = tokio::time::timeout(timeout, async {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err("the session ended".to_string());
                    }
                };
                let root = event.agent_id == ROOT_AGENT;
                match event.event {
                    SessionEvent::PermissionRequested {
                        request_id,
                        tool,
                        summary,
                        ..
                    } => self.bridge_permission(session, request_id, &tool, summary),
                    SessionEvent::QuestionsAsked {
                        request_id,
                        questions,
                    } => answer_with_recommendations(session, request_id, &questions),
                    SessionEvent::TextDelta { text } if root => reply.push_str(&text),
                    SessionEvent::TurnCompleted { .. } if root => return Ok(()),
                    SessionEvent::TurnFailed { message } if root => return Err(message),
                    _ => {}
                }
            }
        })
        .await;
        match outcome {
            Ok(Ok(())) => Ok(reply.trim().to_string()),
            Ok(Err(message)) => Err(message),
            Err(_) => Err(format!(
                "timed out after {} s (an approval may be waiting)",
                timeout.as_secs()
            )),
        }
    }

    /// Answers a session's permission request through the approval queue.
    pub fn bridge_permission(
        self: &Arc<Self>,
        session: &Arc<Session>,
        request_id: u64,
        tool: &str,
        summary: String,
    ) {
        let answer = self.approvals.request_for_session(
            self,
            "tool",
            session.id(),
            format!("{tool} in session {}: {summary}", session.id()),
        );
        let session = Arc::clone(session);
        tokio::spawn(async move {
            let decision = if answer.await.unwrap_or(false) {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny
            };
            let _ = session.respond(request_id, decision);
        });
    }

    /// Starts a session with extra options: pre-approved tools, or a project agent's launcher
    /// and instructions.
    pub fn create_session_with(
        &self,
        project_dir: &str,
        model: Option<&str>,
        options: SessionOptions,
    ) -> Result<(Arc<Session>, broadcast::Receiver<AgentEvent>), CreateError> {
        let eval = options.eval.as_ref().map(|setup| Arc::clone(&setup.0));
        // The user's settings, narrowed by the environment's own configuration files.
        let env_config = self.environment_settings(options.environment_id.as_deref());
        let settings = &env_config.settings;
        let mut notices: Vec<String> = Vec::new();
        let mut fixed_rules = Vec::new();
        for rule in &env_config.permission_rules {
            match imperium_policy::permissions::validate_rule(rule) {
                Ok(()) => fixed_rules.push(rule.clone()),
                Err(error) => notices.push(format!(
                    "The environment's configuration has a permission rule that was ignored: \
                     {error}"
                )),
            }
        }
        if !env_config.refused.is_empty() {
            notices.push(format!(
                "The environment's own configuration was only partly applied (it can only \
                 narrow your settings): {}",
                env_config.refused.join("; ")
            ));
        }
        if let Some(id) = &options.environment_id {
            self.budgets
                .set_environment_limits(id, env_config.budgets.clone());
        }
        let mut workspace = self.workspace(
            project_dir,
            &options.folders,
            &options.runtime,
            options.environment_id.as_deref(),
            &settings.isolation,
            fixed_rules,
        )?;
        // A session bound to an agent is limited by the agent's profile, and fails closed.
        let bound = options.agent_id.as_deref().map(|agent_id| {
            self.agent_profile(
                options.environment_id.as_deref(),
                agent_id,
                options.agent_recorded.as_ref(),
            )
        });
        if let Some((profile, _)) = &bound {
            workspace = workspace
                .with_profile(profile.clone())
                .with_protected_folders([self.paths.config_dir(), self.paths.data_dir()]);
        }
        if options.read_only {
            workspace = workspace.with_read_only();
        }
        // Commands drop the GitHub CLI's login unless the user listed this environment.
        let session_gh = options.environment_id.as_ref().is_some_and(|id| {
            self.settings()
                .github
                .session_gh
                .iter()
                .any(|listed| listed == id)
        });
        workspace = workspace.with_session_gh(session_gh);
        // An environment's sessions also get its repositories' worktrees, unless it turned them
        // off. A session started on its own worktrees keeps to them, so its edits stay apart
        // from the user's checkout.
        if options.worktrees.is_empty()
            && eval.is_none()
            && options
                .environment_id
                .as_deref()
                .and_then(|id| self.environments.get(id))
                .is_some_and(|env| env.project_worktrees)
        {
            workspace = workspace.with_project_worktrees();
        }
        let unattended = options.background || !options.pre_approved.is_empty();
        // Saved allow rules that are not applied, named once: in the first session someone
        // watches.
        if !unattended && eval.is_none() {
            let notice = self
                .over_broad_notice
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            notices.extend(notice);
        }
        let permission_mode = options.permission_mode.unwrap_or(if unattended {
            self.unattended_mode(settings)
        } else {
            settings.permissions.mode
        });
        workspace.permissions().set_mode(permission_mode);
        if options.capped {
            workspace.permissions().cap_for_life();
        }
        let mut placement = Placement {
            environment_id: options.environment_id.clone(),
            agent_id: options.agent_id.clone(),
            folders: workspace.folders_display(),
            persistent: options.persistent,
            worktrees: options.worktrees.clone(),
            pinned: false,
            title: None,
            title_by_user: false,
            project_agent: options.launcher.is_some(),
            branched_from: None,
            comparison_id: None,
            read_only: options.read_only,
            started_by: options.started_by.clone(),
        };
        // A remote runtime's folders are not on this machine: project files (instructions,
        // skills, project memory) are not read from them.
        let remote = workspace.runtime().is_some();
        let local_project = if remote {
            self.paths.data_dir().join("runtimes").join("none")
        } else {
            PathBuf::from(project_dir)
        };
        let project_dir_path = local_project.as_path();
        // A model table id or a spec, as the model pickers offer them.
        // A resumed session keeps the model it was journalled with even if the user has since
        // switched it off; a new session is refused one.
        // (One that never had a model, resumed, is refused an off default like a new one: it
        // waits for a model instead.)
        let resolved = if let Some(eval) = &eval {
            Ok(eval.model.clone())
        } else if options.resume_id.is_some() && model.is_some() {
            self.resolve_model(model)
        } else {
            self.resolve_usable_model(model)
        };
        // A resumed session on a local model does not start it while local models are off: it
        // waits for a model like one that never had any, and keeps the recorded model for when
        // they are turned on again.
        let mut parked_spec = None;
        let model = match resolved {
            Ok(model)
                if eval.is_none()
                    && options.resume_id.is_some()
                    && model.local
                    && self.settings().models.local_off_for(&model.spec) =>
            {
                tracing::info!(
                    model = %model.spec,
                    "a resumed session's local model is not started: local models are off"
                );
                parked_spec = Some(model.spec);
                None
            }
            Ok(model) => Some(model),
            Err(ModelError::NoDefault) if options.may_wait_for_model => None,
            // No model named and the default is off: the session waits for a model as it does
            // with no default.
            Err(ModelError::Disabled(_)) if model.is_none() && options.may_wait_for_model => None,
            Err(error) => return Err(error.into()),
        };
        // An evaluation session's subagents use its own model, never one of the user's.
        let catalog = if eval.is_some() {
            Vec::new()
        } else {
            self.session_catalog()
        };
        // Connectors added or changed in the app since the daemon started apply to new sessions.
        let fresh = crate::connectors::saved_settings(&self.paths);
        // The servers the environment's own file adds (it needs the user's trust for that) start
        // beside the connectors, the user's of the same name winning.
        let running = self.settings();
        let mut mcp_settings = fresh.unwrap_or_else(|| (*running).clone());
        for (name, server) in &settings.mcp.servers {
            // Only what the environment's file added: a connector the user removed since the
            // daemon started stays removed.
            if !running.mcp.servers.contains_key(name) {
                mcp_settings
                    .mcp
                    .servers
                    .entry(name.clone())
                    .or_insert_with(|| server.clone());
            }
        }
        let (mut mcp, mcp_notices) = if eval.is_some() {
            (Vec::new(), Vec::new())
        } else {
            crate::mcp::session_servers(&mcp_settings, project_dir_path, |name| {
                self.secrets.load(name)
            })
        };
        notices.extend(mcp_notices);
        // Plugins' servers are not among `mcp`: they start when an agent activates the plugin.
        let mut extensions = if eval.is_some() {
            imperium_extensions::Extensions::default()
        } else {
            self.extensions(
                project_dir_path,
                settings,
                bound.as_ref().map(|(profile, _)| profile),
                options.environment_id.as_deref(),
                &env_config.repo_disabled_plugins,
            )
        };
        // An evaluation that runs an agent file as its root reads the file the way a delegation
        // would find it.
        let root_agent = match eval.as_ref().and_then(|eval| eval.agent.as_deref()) {
            Some(name) => Some(
                self.extensions(
                    project_dir_path,
                    settings,
                    None,
                    options.environment_id.as_deref(),
                    &env_config.repo_disabled_plugins,
                )
                .agent(name)
                .cloned()
                .ok_or_else(|| CreateError::Runtime(format!("no agent file named {name:?}")))?,
            ),
            None => None,
        };
        // A profile also keeps servers from starting at all: a project's own `.mcp.json` names
        // commands, and an agent that may write files must not get one run by starting a session.
        if let Some((profile, _)) = &bound {
            mcp.retain(|server| profile.allows_mcp(Some(&server.name)));
            let profile = profile.clone();
            extensions
                .plugins
                .set_filter(Arc::new(move |name| profile.allows_plugin(name)));
        }
        let (context, personality) = if eval.is_some() {
            (None, None)
        } else {
            (
                self.context(project_dir_path, &mut notices),
                self.personality(&settings.policy.personality, &mut notices),
            )
        };
        notices.append(&mut extensions.notices);
        let resume = options.resume_id.is_some();
        let id = match options.resume_id.clone().or_else(|| options.id.clone()) {
            Some(id) => id,
            None => new_session_id().map_err(|error| CreateError::Start(error.to_string()))?,
        };
        let remember = options.launcher.is_none() && eval.is_none();
        // Files attached from outside the session's folders are copied into its attachments
        // folder, which its tools may read. A remote runtime cannot reach this machine's
        // folders, so it gets no grant (attached text is inlined either way).
        let workspace = if workspace.runtime().is_none() && eval.is_none() {
            let store = crate::attachments::store_dir(self.paths.data_dir(), &id);
            match std::fs::create_dir_all(&store) {
                Ok(()) => workspace.with_grants(&[imperium_api::PathGrant {
                    path: store.display().to_string(),
                    mode: imperium_api::PathMode::Read,
                }]),
                Err(error) => {
                    tracing::warn!(%error, "could not create the session's attachments folder");
                    workspace
                }
            }
        } else {
            workspace
        };
        // Its own scratch folder, in its environment's working folder: made now (an unarchived
        // session gets a new, empty one), deleted when the session is archived or deleted.
        let local = workspace.runtime().is_none();
        // Held until the session is in the maps, so a move of the working folders sees it.
        let _workspaces = (!options.workspaces_locked).then(|| {
            self.workspaces_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        });
        let workspace = match (&placement.environment_id, remember && local) {
            (Some(environment), true) => match self.session_scratch(environment, &id) {
                Some(dir) => workspace.with_scratch(&dir),
                None => workspace,
            },
            _ => workspace,
        };
        if resume {
            // A resumed session keeps its pin and its title.
            let meta = std::fs::read_to_string(self.meta_path(&id))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .unwrap_or_default();
            placement.pinned = meta["pinned"].as_bool() == Some(true);
            placement.title = meta["title"]
                .as_str()
                .filter(|title| !title.trim().is_empty())
                .map(str::to_string);
            placement.title_by_user =
                placement.title.is_some() && meta["title_by_user"].as_bool() == Some(true);
            placement.branched_from = serde_json::from_value(meta["branched_from"].clone()).ok();
            placement.comparison_id = meta["comparison_id"].as_str().map(str::to_string);
            placement.started_by = serde_json::from_value(meta["started_by"].clone()).ok();
        }
        // Older project memory of the environment's folder is in its Memory.md before the
        // session reads it (a no-op once merged).
        if let Some(environment) = &placement.environment_id {
            self.migrate_environment_memory(environment);
        }
        // Tool providers in the order their tools are offered (MCP tools always come first).
        let mut tools: Vec<Arc<dyn ToolProvider>> = Vec::new();
        if !mcp.is_empty() {
            let auths = crate::mcp::oauth_sources(&mcp, &self.secrets, self.connector_status());
            tools.push(Arc::new(McpTools::new(mcp).with_auth(auths)));
        }
        if let Some(media) = self.media() {
            tools.push(Arc::new(MediaTools::new(media)));
        }
        // Files a page offers are saved in the session's scratch folder (which its agents may
        // read), or its working folder when it has none; never on a remote machine's behalf.
        let downloads = (!remote && !options.read_only).then(|| {
            (
                workspace
                    .scratch()
                    .unwrap_or_else(|| workspace.root())
                    .join("downloads"),
                workspace.clone(),
            )
        });
        if let Some(web) = self.web_tools(downloads).filter(|_| eval.is_none()) {
            if let Some(browser) = &web.browser {
                self.browsers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id.clone(), browser.clone());
            }
            tools.push(Arc::new(web));
        }
        // A preview's dev server runs where the session's commands do, so only a session on
        // this machine offers one; it stops when the session's tools are dropped. A read-only
        // mount gets none, since a dev server writes to its folders.
        if !remote && !options.read_only {
            tools.push(Arc::new(PreviewTools::new(workspace.clone())));
        }
        if let Some(agent_id) = &options.initiative_agent {
            tools.push(Arc::new(crate::initiative::InitiativeTools {
                daemon: self.me.clone(),
                agent_id: agent_id.clone(),
            }));
        }
        if self.suggestions.enabled() {
            tools.push(Arc::new(SuggestTool {
                daemon: self.me.clone(),
                session_id: id.clone(),
            }));
        }
        if self.settings().memory.session_search {
            tools.push(Arc::new(session_search::SessionSearchTool {
                daemon: self.me.clone(),
                session_id: id.clone(),
            }));
        }
        let list_models = self.settings().models.list_models;
        if list_models != imperium_config::ListModelsScope::Off {
            tools.push(Arc::new(model_awareness::ListModelsTool {
                daemon: self.me.clone(),
                catalog: catalog.clone(),
                subagents: list_models == imperium_config::ListModelsScope::All,
            }));
        }
        // Every session may change the app config through the daemon, wherever it runs; the
        // permission rules decide whether each change asks.
        tools.push(Arc::new(agent_config::AgentConfigTools {
            daemon: self.me.clone(),
            session_id: id.clone(),
        }));
        // An evaluation session offers its own tools: the engine's and the offline web tools.
        if let Some(eval) = &eval {
            tools = eval.tools.clone();
        }
        let model_spec = model
            .as_ref()
            .map(|model| model.spec.clone())
            .or(parked_spec);
        let effort = options.effort.unwrap_or(self.settings().models.effort);
        let journal_path = self
            .paths
            .data_dir()
            .join("sessions")
            .join(format!("{id}.jsonl"));
        // Writer subagents started with `access: "write"` get worktrees of their own (local
        // folders only, and not for a read-only mount).
        let workspace = if remote || options.read_only {
            workspace
        } else {
            workspace.with_writer_trees(Arc::new(crate::writers::GitWriterTrees::new(
                &id,
                &journal_path,
                self.worktree_dir_for(options.environment_id.as_deref()),
            )))
        };
        // Remembered sessions on this machine checkpoint their repositories' files before each
        // turn that changes them.
        let checkpoints =
            (remember && !remote && self.settings().sessions.checkpoints).then(|| {
                Arc::new(crate::checkpoints::SessionCheckpoints::new(
                    self.me.clone(),
                    self.paths.data_dir(),
                    &id,
                    journal_path.clone(),
                    workspace
                        .roots()
                        .into_iter()
                        .map(Path::to_path_buf)
                        .collect(),
                ))
            });
        // Nobody reads the turns of a background session, one with pre-approved tools (auto-fix),
        // or a project or persistent agent: the harness aids that talk to a person stay off.
        let attended = eval
            .as_ref()
            .map_or_else(|| session_attended(&options), |eval| eval.flags.attended);
        let eval_flags = eval.as_ref().map(|eval| eval.flags.clone());
        let session = Session::spawn_with(
            SessionConfig {
                attended,
                effort,
                journal_path,
                budget_gate: {
                    let gate: Arc<dyn imperium_engine::BudgetGate> = match &eval {
                        Some(eval) => eval.gate.clone(),
                        None => {
                            Arc::new(crate::budgets::CallGate::new(self.me.clone(), id.clone()))
                        }
                    };
                    Some(gate)
                },
                checkpoints: checkpoints.clone().map(|checkpoints| {
                    let hook: Arc<dyn imperium_engine::TurnCheckpoints> = checkpoints;
                    hook
                }),
                id: id.clone(),
                workspace,
                model,
                catalog,
                clients: eval
                    .as_ref()
                    .map_or_else(ChatClients::default, |eval| eval.clients()),
                routing: settings.routing.clone(),
                local: self.local.get().filter(|_| eval.is_none()).map(|local| {
                    let supervisor: Arc<dyn LocalModels> = local.supervisor.clone();
                    supervisor
                }),
                tools,
                notices,
                extensions,
                context,
                enforce: self.settings().workflow.enforce,
                pre_approved: options.pre_approved,
                resume,
                resume_policy: self.resume_policy(),
                launcher: options.launcher,
                instructions: match (options.instructions, env_config.instructions.as_deref()) {
                    _ if eval.is_some() => None,
                    (Some(own), Some(added)) => {
                        Some(format!("{own}\n\n{}", environment_text(added)))
                    }
                    (Some(own), None) => Some(own),
                    (None, Some(added)) => Some(environment_text(added)),
                    (None, None) => None,
                },
                personality,
                background: options.background.then(|| Background {
                    load: self.load_view(),
                }),
                memory: (self.settings().memory.enabled && eval.is_none()).then(|| {
                    SessionMemory {
                        // The global store only; an environment's Memory.md lives in its working folder,
                        // outside the project, so a read-only session may write it (under the policy).
                        store: Store::new(self.paths.data_dir()),
                        settings: self.settings().memory.clone(),
                        // Looked up on each use, so it follows the working folders when they move.
                        environment: placement
                            .environment_id
                            .clone()
                            .filter(|environment| crate::workspaces::safe_id(environment))
                            .map(|environment| {
                                let me = self.me.clone();
                                let fallback = self.workspaces_root();
                                imperium_memory::environment::EnvironmentMemory::live(move || {
                                    let root = me.upgrade().map_or_else(
                                        || fallback.clone(),
                                        |state| state.workspaces_root(),
                                    );
                                    crate::workspaces::memory_file(&root, &environment)
                                        .unwrap_or_default()
                                })
                            }),
                    }
                }),
                max_steps: eval_flags
                    .as_ref()
                    .map_or(self.settings().agents.max_steps, |flags| flags.max_steps),
                auto_compact: eval_flags
                    .as_ref()
                    .map_or(options.auto_compact, |flags| flags.auto_compact),
                auto_compact_at: match &eval_flags {
                    Some(flags) => flags.auto_compact_at,
                    None => Some(self.settings().context.auto_compact_at),
                },
                harness: match &eval_flags {
                    Some(flags) => imperium_config::HarnessSettings {
                        dereference: flags.harness,
                        state: flags.harness,
                        hints: flags.harness,
                        clarify: flags.harness,
                        tool_loading: flags.tool_loading,
                        result_spill: flags.result_spill,
                        clear_stale_results: flags.clear_stale_results,
                        parallel_reads: flags.parallel_reads,
                        ..imperium_config::HarnessSettings::default()
                    },
                    None => self.settings().harness.clone(),
                },
                attachments: self.settings().attachments.clone(),
                auto_permissions: self.settings().permissions.auto.clone(),
                keep_agent_state: remember && self.settings().sessions.agent_state_keep,
                knowledge: imperium_engine::KnowledgeConfig {
                    settings: match &eval_flags {
                        Some(flags) => imperium_config::KnowledgeSettings {
                            enabled: flags.knowledge,
                            ..imperium_config::KnowledgeSettings::default()
                        },
                        None => self.settings().knowledge.clone(),
                    },
                    models: Some(Arc::new(crate::knowledge::ModelSwitch::new(
                        self.me.clone(),
                    ))),
                    cache_lifetime_s: settings
                        .models
                        .catalog
                        .iter()
                        .filter_map(|entry| {
                            entry
                                .cache_lifetime_s
                                .map(|seconds| (entry.spec.clone(), u64::from(seconds)))
                        })
                        .collect(),
                },
            },
            imperium_engine::SessionOptions {
                replay: eval.as_ref().and_then(|eval| eval.subagent_replay.clone()),
                root_agent,
                tool_edits: eval
                    .as_ref()
                    .map(|eval| eval.run.tool_edits())
                    .unwrap_or_default(),
                tool_watch: eval.as_ref().map(|eval| eval.run.tool_watch()),
                ..imperium_engine::SessionOptions::default()
            },
        )
        .map_err(|error| CreateError::Start(error.to_string()))?;
        // Ordinary sessions are remembered so they continue after a restart; project agents
        // start afresh with the daemon.
        if remember {
            let meta = serde_json::json!({
                "project_dir": project_dir,
                "model": model_spec,
                "background": options.background,
                "unattended": !attended,
                "capped": options.capped,
                "run_now": options.run_now,
                "environment_id": placement.environment_id,
                "folders": placement.folders,
                "worktrees": placement.worktrees,
                "pinned": placement.pinned,
                "title": placement.title,
                "title_by_user": placement.title_by_user,
                "branched_from": placement.branched_from,
                "comparison_id": placement.comparison_id,
                "subfolder": options.subfolder,
                "read_only": placement.read_only,
                "started_by": placement.started_by,
                "effort": effort.as_str(),
                "permission_mode": permission_mode.as_str(),
                "agent_id": options.agent_id,
                "agent_permissions": bound.as_ref().map(|(_, permissions)| permissions),
            });
            let path = self.meta_path(&id);
            if let Err(error) = write_meta(&path, &meta.to_string()) {
                tracing::warn!(path = %path.display(), %error, "could not record the session");
            }
        }
        if options.run_now {
            session.run_now().release();
        }
        let events = session.subscribe();
        if let Some(pets) = self.pets().filter(|_| eval.is_none()) {
            pets.watch(id.clone(), session.subscribe());
        }
        if let Some(me) = self.me.upgrade().filter(|_| eval.is_none()) {
            crate::suggestions::watch(&me, id.clone(), session.subscribe());
            if placement.title.is_none() {
                titles::watch(&me, id.clone(), session.subscribe());
            }
            crate::resources::watch_session(&me, id.clone(), session.subscribe());
            crate::budgets::watch_session(
                &me,
                id.clone(),
                placement.environment_id.clone(),
                session.subscribe(),
            );
        }
        let session = Arc::new(session);
        if let Some(eval) = &eval {
            crate::eval::watch(&session, Arc::clone(&eval.run));
            self.evals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), Arc::clone(&eval.run));
        }
        if let Some(checkpoints) = checkpoints {
            self.checkpoints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.clone(), checkpoints);
        }
        self.placements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), placement);
        self.sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .insert(id, Arc::clone(&session));
        // An evaluation session is not announced: it belongs to the connection that made it.
        if eval.is_none() {
            self.announce(SESSION_CREATED, self.summary(&session));
        }
        Ok((session, events))
    }

    /// Starts a session in an environment: its first folder is the working directory and every
    /// folder is open to the session's tools.
    pub fn create_in_environment(
        &self,
        environment_id: &str,
        model: Option<&str>,
        options: SessionOptions,
    ) -> Result<(Arc<Session>, broadcast::Receiver<AgentEvent>), CreateError> {
        let env = self
            .environments
            .get(environment_id)
            .ok_or_else(|| EnvironmentError::NotFound(environment_id.to_string()))?;
        let mut options = options;
        if options.subfolder.is_some() && env.folders.is_empty() {
            return Err(EnvironmentError::Invalid(format!(
                "the environment {} has no folders to narrow a session to",
                env.name
            ))
            .into());
        }
        if (options.subfolder.is_some() || options.read_only)
            && (options.worktree || !options.worktrees.is_empty())
        {
            return Err(CreateError::Worktree(
                "a session narrowed to a subfolder or mounted read-only cannot start on a \
                 worktree"
                    .into(),
            ));
        }
        // Home with no project folders works in its session's own scratch folder. The folder
        // is made under the working-folders lock, which the session keeps until it exists.
        let _home_lock;
        // The id of a new Home session whose folder this call made, to take it back if the
        // session does not start.
        let mut made_home_folder = None;
        if env.folders.is_empty() && env.builtin && !options.workspaces_locked {
            if options.worktree {
                return Err(CreateError::Worktree(
                    "Home has no project folders to start on a worktree".into(),
                ));
            }
            // The model is checked first, so a refusal leaves no folder behind.
            // (An evaluation session brings its own model.)
            let resolved = if options.eval.is_some() {
                Ok(None)
            } else if options.resume_id.is_some() && model.is_some() {
                self.resolve_model(model).map(Some)
            } else {
                self.resolve_usable_model(model).map(Some)
            };
            match resolved {
                Ok(_) => {}
                Err(ModelError::NoDefault) if options.may_wait_for_model => {}
                Err(ModelError::Disabled(_)) if model.is_none() && options.may_wait_for_model => {}
                Err(error) => return Err(error.into()),
            }
            _home_lock = self
                .workspaces_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            options.workspaces_locked = true;
            let id = match options.resume_id.clone().or_else(|| options.id.clone()) {
                Some(id) => id,
                None => new_session_id().map_err(|error| CreateError::Start(error.to_string()))?,
            };
            let dir = crate::workspaces::ensure_session_dir(&self.workspaces_root(), &env.id, &id)
                .map_err(|error| {
                    CreateError::Start(format!("could not make Home's working folder: {error}"))
                })?;
            if options.resume_id.is_none() {
                made_home_folder = Some(id.clone());
            }
            options.id = Some(id);
            options.home_dir = Some(dir.display().to_string());
        }
        if env.folders.is_empty() && options.home_dir.is_none() {
            return Err(EnvironmentError::Invalid(format!(
                "the environment {} has no folders yet; add one first",
                env.name
            ))
            .into());
        }
        let mut folders = match options.home_dir.take() {
            Some(dir) => vec![dir],
            None => env.folders.clone(),
        };
        // Narrowed to a subfolder: it is the session's only folder, so the environment's other
        // folders (and the rest of this one) are closed to its tools.
        if let Some(sub) = options.subfolder.take() {
            let narrowed = crate::environments::narrowed_folder(&env.runtime, &env.folders, &sub)?;
            options.subfolder = Some(narrowed.clone());
            folders = vec![narrowed];
        }
        if !options.worktrees.is_empty() {
            // A resumed session goes back to its worktrees.
            folders = crate::worktrees::apply(&folders, &options.worktrees);
        } else if options.worktree {
            if env.runtime.is_remote() {
                return Err(CreateError::Worktree(
                    "worktrees are made only for folders on this machine".into(),
                ));
            }
            let id = match options.id.clone() {
                Some(id) => id,
                None => new_session_id().map_err(|error| CreateError::Start(error.to_string()))?,
            };
            let branch = options
                .worktree_branch
                .clone()
                .filter(|branch| !branch.trim().is_empty())
                .unwrap_or_else(|| format!("imperium/{}", &id[..id.len().min(8)]));
            let dir = self.worktree_dir_for(Some(&env.id));
            let (used, made) = crate::worktrees::create(&folders, branch.trim(), dir.as_deref())
                .map_err(CreateError::Worktree)?;
            folders = used;
            options.worktrees = made;
            options.id = Some(id);
        }
        let (first, rest) = folders.split_first().expect("checked above");
        let options = SessionOptions {
            environment_id: Some(env.id.clone()),
            folders: rest.to_vec(),
            runtime: env.runtime.clone(),
            ..options
        };
        let created = self.create_session_with(first, model, options);
        if created.is_err()
            && let Some(id) = made_home_folder
        {
            self.remove_session_scratch(&env.id, &id);
        }
        created
    }
}
