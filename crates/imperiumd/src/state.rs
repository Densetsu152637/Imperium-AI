use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use imperium_config::{KeychainSecrets, ModelPlacement, Paths, SecretStore, Settings};
use imperium_core::{
    Background, CatalogModel, ChatClients, LoadView, LocalModels, ModelError, Session,
    SessionConfig, SessionMemory, ToolProvider, new_session_id, resolve_model,
};
use imperium_extensions::{ConfiguredHook, Extensions, Sources};
use imperium_local::supervisor::SwapDirection;
use imperium_mcp::McpTools;
use imperium_media::{ImageConfig, ImageGenerators, Media, MediaTools, Service};
use imperium_memory::Store;
use imperium_protocol::jsonrpc::Notification;
use imperium_protocol::methods::{
    LocalModelListResult, MODEL_SWAPPED_IN, MODEL_SWAPPED_OUT, ModelListResult, ModelRow,
    ModelSwap, SESSION_CREATED, SessionSummary,
};
use imperium_protocol::session::{
    AgentEvent, PermissionDecision, ROOT_AGENT, ResumePolicy, SessionEvent, SessionStatus,
};
use imperium_tools::{Sandbox, Workspace};
use imperium_web::{BrowserSetup, SearchConfig, SearchProviders, Web, WebTools};
use imperium_workflow::{ContextIndex, Pack};
use tokio::sync::broadcast;

use crate::approvals::Approvals;
use crate::downloads::Downloads;
use crate::load::{self, LoadMonitor};
use crate::local::LocalRuntime;
use crate::pets::PetEngine;
use crate::phone::Devices;
use crate::project_agent::PendingBrief;
use crate::project_agent::ProjectAgents;
use crate::scheduler::Scheduler;

/// Options for [`DaemonState::create_session_with`].
#[derive(Debug, Default)]
pub struct SessionOptions {
    /// Continue this earlier session (after a restart) instead of starting a new one.
    pub resume_id: Option<String>,
    /// Tools that run without asking.
    pub pre_approved: Vec<String>,
    pub launcher: Option<Arc<dyn imperium_core::SessionLauncher>>,
    pub instructions: Option<String>,
    /// Started by a schedule or a project agent: it gives way to the user's own sessions.
    pub background: bool,
    /// A background session that must not give way to the load: a person ran it now, or it
    /// waited out its `max_defer`.
    pub run_now: bool,
}

/// State shared by every connection: configuration and the running sessions.
#[derive(Debug)]
pub struct DaemonState {
    started: Instant,
    paths: Paths,
    settings: Settings,
    /// Where API keys and tokens are read from: the OS keychain unless the caller chose another.
    secrets: Arc<dyn SecretStore>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Set at start when there are model folders, or after the first download.
    local: OnceLock<LocalRuntime>,
    downloads: Downloads,
    /// The Hugging Face Hub to use instead of the one from the environment (for tests).
    hub: OnceLock<imperium_local::hf::Hub>,
    /// Notifications for every connected client, such as schedule runs.
    announcements: broadcast::Sender<Notification>,
    scheduler: OnceLock<Arc<Scheduler>>,
    approvals: Approvals,
    project_agents: ProjectAgents,
    pets: OnceLock<Option<Arc<PetEngine>>>,
    devices: Devices,
    phone_address: OnceLock<std::net::SocketAddr>,
    /// Pull requests sessions watch: session id to (repo, number).
    watches: Mutex<HashMap<String, (String, u64)>>,
    /// Turns what the daemon sees into quiet, busy, or saturated ([`start_sampling`]).
    monitor: Arc<LoadMonitor>,
    /// A load view to use instead of the monitor (for tests).
    load_override: OnceLock<Arc<dyn LoadView>>,
    /// Briefs of project-agent sessions that were waiting for a quiet machine when the daemon
    /// stopped, until the project agents start and pick them up.
    pending_briefs: Mutex<Vec<PendingBrief>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("project folder {0}: {1}")]
    Project(String, std::io::Error),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("could not start the session: {0}")]
    Start(String),
}

impl DaemonState {
    /// `local` is the local model runtime, when `models.folders` are configured
    /// ([`LocalRuntime::start`]). Secrets come from the OS keychain.
    pub fn new(paths: Paths, settings: Settings, local: Option<LocalRuntime>) -> Arc<Self> {
        DaemonState::with_secrets(paths, settings, local, Arc::new(KeychainSecrets))
    }

    /// Like [`DaemonState::new`], reading every secret from `secrets`.
    pub fn with_secrets(
        paths: Paths,
        settings: Settings,
        local: Option<LocalRuntime>,
        secrets: Arc<dyn SecretStore>,
    ) -> Arc<Self> {
        let devices = Devices::load(paths.data_dir().join("devices.json"));
        let monitor = Arc::new(LoadMonitor::new(settings.load.clone()));
        let state = Arc::new(DaemonState {
            started: Instant::now(),
            paths,
            settings,
            secrets,
            sessions: Mutex::new(HashMap::new()),
            local: local.map(OnceLock::from).unwrap_or_default(),
            downloads: Downloads::default(),
            hub: OnceLock::new(),
            announcements: broadcast::channel(256).0,
            scheduler: OnceLock::new(),
            approvals: Approvals::default(),
            project_agents: ProjectAgents::default(),
            pets: OnceLock::new(),
            devices,
            phone_address: OnceLock::new(),
            watches: Mutex::new(HashMap::new()),
            monitor,
            load_override: OnceLock::new(),
            pending_briefs: Mutex::new(Vec::new()),
        });
        state.apply_load_view();
        if let Some(local) = state.local.get()
            && tokio::runtime::Handle::try_current().is_ok()
        {
            state.announce_swaps(local);
        }
        state
    }

    /// Starts reading the machine so the load view follows it (until then it reads quiet).
    pub fn start_load_monitor(self: &Arc<Self>) {
        load::start_sampling(self);
    }

    pub(crate) fn load_monitor(&self) -> Arc<LoadMonitor> {
        Arc::clone(&self.monitor)
    }

    /// Whether the machine is quiet, busy, or saturated, for work that gives way to the user.
    pub fn load_view(&self) -> Arc<dyn LoadView> {
        self.load_override.get().cloned().unwrap_or_else(|| {
            let monitor: Arc<dyn LoadView> = self.monitor.clone();
            monitor
        })
    }

    /// Uses `view` instead of the monitor, such as a fixed one in tests. Call once, before
    /// sessions start.
    pub fn use_load_view(&self, view: Arc<dyn LoadView>) {
        if self.load_override.set(view).is_ok() {
            self.apply_load_view();
        }
    }

    /// Tells the local models how background calls give way.
    fn apply_load_view(&self) {
        if let Some(local) = self.local.get() {
            local.supervisor.set_background_policy(
                self.load_view(),
                Duration::from_secs(self.settings.load.interactive_hold_min * 60),
            );
        }
    }

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

    /// Slots in use and calls queued on the local models.
    pub(crate) fn local_activity(&self) -> imperium_local::supervisor::Activity {
        self.local
            .get()
            .map(|local| local.supervisor.activity())
            .unwrap_or_default()
    }

    pub(crate) fn recent_swaps(&self) -> u32 {
        self.local
            .get()
            .map_or(0, |local| local.supervisor.recent_swaps())
    }

    /// Tells every client when a local model is swapped out or in (`model.swapped_out`,
    /// `model.swapped_in`).
    fn announce_swaps(self: &Arc<Self>, local: &LocalRuntime) {
        let mut swaps = local.supervisor.swap_events();
        let state = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let event = match swaps.recv().await {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let Some(state) = state.upgrade() else {
                    return;
                };
                let method = match event.direction {
                    SwapDirection::Out => MODEL_SWAPPED_OUT,
                    SwapDirection::In => MODEL_SWAPPED_IN,
                };
                state.announce(
                    method,
                    ModelSwap {
                        model_id: event.model_id,
                        seconds: event.seconds,
                        bytes: event.bytes,
                        slots: event.slots,
                        reason: event.reason.to_string(),
                    },
                );
            }
        });
    }

    /// Starts running `[[schedules]]`. Call once, inside the Tokio runtime.
    pub fn start_scheduler(self: &Arc<Self>) -> Arc<Scheduler> {
        Arc::clone(self.scheduler.get_or_init(|| Scheduler::start(self)))
    }

    pub fn devices(&self) -> &Devices {
        &self.devices
    }

    /// Starts the phone listener when `phone.bind` is set. Call once.
    pub async fn start_phone(self: &Arc<Self>) -> std::io::Result<Option<std::net::SocketAddr>> {
        let Some(bind) = &self.settings.phone.bind else {
            return Ok(None);
        };
        let address = bind
            .parse()
            .map_err(|error| std::io::Error::other(format!("phone.bind {bind:?}: {error}")))?;
        let bound = crate::phone::start(self, address).await?;
        let _ = self.phone_address.set(bound);
        Ok(Some(bound))
    }

    /// The phone app's address, when the phone listener is on.
    pub fn phone_url(&self) -> Option<String> {
        self.phone_address
            .get()
            .map(|address| format!("http://{address}/"))
    }

    /// Starts the pet engine (when pets are on and one is installed). Call once.
    pub fn start_pets(self: &Arc<Self>) {
        let engine = PetEngine::start(self);
        let _ = self.pets.set(engine);
    }

    pub fn pets(&self) -> Option<&Arc<PetEngine>> {
        self.pets.get().and_then(Option::as_ref)
    }

    pub fn config_dir(&self) -> &Path {
        self.paths.config_dir()
    }

    pub fn project_agents(&self) -> &ProjectAgents {
        &self.project_agents
    }

    /// Starts the agents of projects with `agent = true`.
    pub fn start_project_agents(self: &Arc<Self>) {
        crate::project_agent::start(self);
    }

    /// Wakes a project's agent with a message; false when the project has no agent.
    pub fn tell_project_agent(&self, project: &Path, text: String) -> bool {
        self.project_agents
            .get(project)
            .is_some_and(|agent| agent.send(text).is_ok())
    }

    pub fn approvals(&self) -> &Approvals {
        &self.approvals
    }

    /// Starts GitHub auto-fix when it is enabled and repositories are watched, and the watcher
    /// for pull requests sessions follow.
    pub fn start_autofix(self: &Arc<Self>, github: Arc<dyn imperium_github::GitHub>) {
        let poll = std::time::Duration::from_secs(self.settings.github.autofix.poll_s.max(10));
        crate::autofix::watch_pull_requests(self, Arc::clone(&github), poll);
        crate::autofix::start(self, github);
    }

    pub fn scheduler(&self) -> Option<&Arc<Scheduler>> {
        self.scheduler.get()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The store every secret is read from.
    pub fn secrets(&self) -> &Arc<dyn SecretStore> {
        &self.secrets
    }

    pub fn data_dir(&self) -> &Path {
        self.paths.data_dir()
    }

    /// Sends a notification to every connected client.
    pub fn announce(&self, method: &str, params: impl serde::Serialize) {
        let params = serde_json::to_value(params).expect("notifications serialise to JSON");
        // No receivers just means no client is connected.
        let _ = self.announcements.send(Notification::new(method, params));
    }

    pub fn announcements(&self) -> broadcast::Receiver<Notification> {
        self.announcements.subscribe()
    }

    /// Media backends from `[media]`, using `[endpoints]` for URLs and keys.
    fn media(&self) -> Option<Arc<Media>> {
        let settings = &self.settings.media;
        let service = |endpoint: &str, model: &str| -> Option<Service> {
            let endpoint_settings = self.settings.endpoints.get(endpoint)?;
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
    fn web_tools(&self) -> Option<WebTools> {
        let web = &self.settings.web;
        if !web.enabled {
            return None;
        }
        let key = || {
            let name = &web.api_key.as_ref()?.secret;
            self.secrets.load(name).ok().flatten()
        };
        let config = SearchConfig {
            searxng_url: &web.searxng_url,
            api_key: &key,
        };
        let provider = SearchProviders::default().build(&web.search, &config);
        if web.search != "none" && provider.is_none() {
            tracing::warn!(provider = %web.search, "web search is not configured completely (URL or key missing)");
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
            });
        Some(WebTools {
            web: Web::new(provider),
            allow_domains: web.allow_domains.clone(),
            browser,
        })
    }

    /// The isolation tier commands run under, from `[isolation]`.
    fn sandbox(&self) -> Sandbox {
        let isolation = &self.settings.isolation;
        match isolation.tier.as_str() {
            "os" => Sandbox::Os {
                network: isolation.network,
            },
            "container" => Sandbox::Container {
                runtime: isolation.container_runtime.clone(),
                image: isolation.container_image.clone(),
                network: isolation.network,
            },
            _ => Sandbox::Policy,
        }
    }

    /// Re-opens sessions active within `agents.resume_days`, from their journals and saved
    /// conversations. Returns how many continued.
    pub fn resume_sessions(&self) -> usize {
        let dir = self.paths.data_dir().join("sessions");
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(
                self.settings.agents.resume_days * 86_400,
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
            let (Some(meta), true) = (meta, recent) else {
                continue;
            };
            let project = meta["project_dir"].as_str().unwrap_or_default();
            let model = meta["model"].as_str();
            let options = SessionOptions {
                resume_id: Some(id.clone()),
                background: meta["background"].as_bool().unwrap_or(false),
                run_now: meta["run_now"].as_bool().unwrap_or(false),
                ..SessionOptions::default()
            };
            match self.create_session_with(project, model, options) {
                Ok((session, events)) => {
                    resumed += 1;
                    // A brief still waiting for a quiet machine is sent once the project
                    // agents start ([`crate::project_agent::start`]).
                    if let Some(brief) = meta["deferred_brief"]["text"].as_str() {
                        lock_pending(&self.pending_briefs).push(PendingBrief {
                            project: project.to_string(),
                            session,
                            events,
                            brief: brief.to_string(),
                            until_unix_ms: meta["deferred_brief"]["until_unix_ms"]
                                .as_u64()
                                .unwrap_or(0),
                        });
                    }
                }
                Err(error) => tracing::warn!(session = %id, %error, "could not resume the session"),
            }
        }
        resumed
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
    fn update_meta(
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

    /// The configured resume policy for usage limits.
    fn resume_policy(&self) -> ResumePolicy {
        let resume = &self.settings.resume;
        ResumePolicy {
            mode: resume.mode.clone(),
            at_time: resume.at_time.clone(),
            retry_every_minutes: resume.retry_every_minutes,
            max_wait_hours: resume.max_wait_hours,
        }
    }

    /// Links a session to a pull request it watches; the watcher marks it merged when the
    /// pull request merges. `None` stops watching.
    pub fn watch(&self, session: &Session, target: Option<(String, u64)>) {
        let key = session.id().to_string();
        match target {
            Some((repo, number)) => {
                session.set_watch(Some((SessionStatus::Watching, format!("{repo}#{number}"))));
                lock_watches(&self.watches).insert(key, (repo, number));
            }
            None => {
                session.set_watch(None);
                lock_watches(&self.watches).remove(&key);
            }
        }
    }

    /// Pull requests sessions watch, by session id.
    pub fn watched(&self) -> Vec<(String, String, u64)> {
        lock_watches(&self.watches)
            .iter()
            .map(|(id, (repo, number))| (id.clone(), repo.clone(), *number))
            .collect()
    }

    /// Where UI mods are installed.
    pub fn mods_dir(&self) -> std::path::PathBuf {
        self.paths.config_dir().join("mods")
    }

    /// Local models, the hardware they are planned against, and what is loaded.
    pub fn local_models(&self) -> LocalModelListResult {
        self.local
            .get()
            .map_or_else(crate::local::disabled, LocalRuntime::listing)
    }

    /// The memory local models may use, when the local runtime is running.
    pub fn local_budget(&self) -> Option<imperium_local::planner::Budget> {
        self.local.get().map(LocalRuntime::budget)
    }

    /// Scans the model folders again (after a download), starting the local runtime when it
    /// was not running for want of folders. Must run inside the Tokio runtime; it blocks.
    pub fn rescan_local_models(self: &Arc<Self>) {
        if let Some(local) = self.local.get() {
            return local.rescan(&self.paths, &self.settings);
        }
        let Some(started) = LocalRuntime::start(&self.paths, &self.settings) else {
            return;
        };
        if self.local.set(started).is_ok() {
            if let Some(local) = self.local.get() {
                self.announce_swaps(local);
            }
        } else if let Some(local) = self.local.get() {
            // Another download started it first, perhaps before this one's files landed.
            local.rescan(&self.paths, &self.settings);
        }
    }

    pub fn downloads(&self) -> &Downloads {
        &self.downloads
    }

    /// The Hugging Face Hub: `HF_ENDPOINT` and a token from `HF_TOKEN` or the secret store,
    /// read on each use so a newly stored token is picked up.
    pub fn hub(&self) -> imperium_local::hf::Hub {
        self.hub
            .get()
            .cloned()
            .unwrap_or_else(|| imperium_local::hf::Hub::from_env(self.secrets.as_ref()))
    }

    /// Uses `hub` instead of the environment's, such as a local fake in tests. Call once.
    pub fn use_hub(&self, hub: imperium_local::hf::Hub) {
        let _ = self.hub.set(hub);
    }

    pub fn uptime_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
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
        let (session, events) = match self.create_session_with(project_dir, model, options) {
            Ok(created) => created,
            Err(error) => return (false, error.to_string(), None),
        };
        let session_id = Some(session.id().to_string());
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
                    } => self.bridge_permission(session, request_id, &tool, summary),
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
        let answer = self.approvals.request(
            self,
            "tool",
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
        let workspace = Workspace::new(Path::new(project_dir))
            .map_err(|error| CreateError::Project(project_dir.to_string(), error))?
            .with_sandbox(self.sandbox());
        let model = resolve_model(&self.settings, model, |name| self.secrets.load(name))?;
        let catalog = self.catalog();
        let (mut mcp, mut notices) =
            crate::mcp::session_servers(&self.settings, Path::new(project_dir), |name| {
                self.secrets.load(name)
            });
        let mut extensions = self.extensions(Path::new(project_dir));
        let context = self.context(Path::new(project_dir), &mut notices);
        mcp.append(&mut extensions.servers);
        notices.append(&mut extensions.notices);
        let resume = options.resume_id.is_some();
        let id = match options.resume_id.clone() {
            Some(id) => id,
            None => new_session_id().map_err(|error| CreateError::Start(error.to_string()))?,
        };
        let remember = options.launcher.is_none();
        // Tool providers in the order their tools are offered (MCP tools always come first).
        let mut tools: Vec<Arc<dyn ToolProvider>> = Vec::new();
        if !mcp.is_empty() {
            tools.push(Arc::new(McpTools::new(mcp)));
        }
        if let Some(media) = self.media() {
            tools.push(Arc::new(MediaTools::new(media)));
        }
        if let Some(web) = self.web_tools() {
            tools.push(Arc::new(web));
        }
        let model_spec = model.spec.clone();
        let session = Session::spawn(SessionConfig {
            journal_path: self
                .paths
                .data_dir()
                .join("sessions")
                .join(format!("{id}.jsonl")),
            id: id.clone(),
            workspace,
            model,
            catalog,
            clients: ChatClients::default(),
            routing: self.settings.routing.clone(),
            local: self.local.get().map(|local| {
                let supervisor: Arc<dyn LocalModels> = local.supervisor.clone();
                supervisor
            }),
            tools,
            notices,
            extensions,
            context,
            enforce: self.settings.workflow.enforce,
            pre_approved: options.pre_approved,
            resume,
            resume_policy: self.resume_policy(),
            launcher: options.launcher,
            instructions: options.instructions,
            background: options.background.then(|| Background {
                load: self.load_view(),
            }),
            memory: self.settings.memory.enabled.then(|| SessionMemory {
                store: Store::new(self.paths.data_dir(), Some(Path::new(project_dir))),
                settings: self.settings.memory.clone(),
            }),
            max_steps: self.settings.agents.max_steps,
        })
        .map_err(|error| CreateError::Start(error.to_string()))?;
        // Ordinary sessions are remembered so they continue after a restart; project agents
        // start afresh with the daemon.
        if remember {
            let meta = serde_json::json!({
                "project_dir": project_dir,
                "model": model_spec,
                "background": options.background,
                "run_now": options.run_now,
            });
            let path = self
                .paths
                .data_dir()
                .join("sessions")
                .join(format!("{id}.meta.json"));
            if let Err(error) = std::fs::write(&path, meta.to_string()) {
                tracing::warn!(path = %path.display(), %error, "could not record the session");
            }
        }
        if options.run_now {
            session.run_now().release();
        }
        let events = session.subscribe();
        if let Some(pets) = self.pets() {
            pets.watch(id.clone(), session.subscribe());
        }
        let session = Arc::new(session);
        self.sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .insert(id, Arc::clone(&session));
        self.announce(SESSION_CREATED, summary(&session));
        Ok((session, events))
    }

    /// Workflow packs and the project's instructions and documentation, unless the workflow is
    /// turned off. Packs that cannot be read are reported in `notices`.
    fn context(&self, project_dir: &Path, notices: &mut Vec<String>) -> Option<Arc<ContextIndex>> {
        let workflow = &self.settings.workflow;
        if !workflow.enabled {
            return None;
        }
        let builtin = (workflow.packs.is_empty() && workflow.default_pack)
            .then(bundled_pack)
            .flatten();
        let packs = workflow
            .packs
            .iter()
            .chain(builtin.as_ref())
            .filter_map(|root| match Pack::load(root) {
                Ok(pack) => Some(pack),
                Err(error) => {
                    notices.push(format!("workflow pack skipped: {error}"));
                    None
                }
            })
            .collect();
        Some(Arc::new(ContextIndex::new(packs, project_dir)))
    }

    /// Skills, hooks, slash commands, and plugins for a session in `project_dir`.
    fn extensions(&self, project_dir: &Path) -> Extensions {
        imperium_extensions::load(&Sources {
            config_dir: self.paths.config_dir().to_path_buf(),
            project_dir: project_dir.to_path_buf(),
            plugin_dirs: self.settings.extensions.plugin_dirs.clone(),
            hooks: self
                .settings
                .hooks
                .iter()
                .map(|hook| ConfiguredHook {
                    event: hook.event.clone(),
                    matcher: hook.matcher.clone(),
                    command: hook.command.clone(),
                    timeout_s: hook.timeout_s,
                })
                .collect(),
            server_timeout: Duration::from_secs(60),
        })
    }

    /// Resolves `[[models.catalog]]` for routing subagents. Entries that cannot be used (an
    /// unknown endpoint, a missing key) are skipped with a warning rather than failing the session.
    fn catalog(&self) -> Vec<CatalogModel> {
        self.settings
            .models
            .catalog
            .iter()
            .filter_map(|entry| {
                match resolve_model(&self.settings, Some(&entry.spec), |name| {
                    self.secrets.load(name)
                }) {
                    Ok(model) => Some(CatalogModel {
                        id: entry.id.clone(),
                        model,
                        tier: entry.tier,
                        location: entry.location,
                        tags: entry.tags.clone(),
                        cpu_lane: entry.placement == ModelPlacement::Cpu,
                    }),
                    Err(error) => {
                        tracing::warn!(model = %entry.spec, %error, "skipping catalog model");
                        None
                    }
                }
            })
            .collect()
    }

    /// The configured model table, with prices from each endpoint's pricing.
    pub fn model_table(&self) -> ModelListResult {
        let models = self
            .settings
            .models
            .catalog
            .iter()
            .map(|entry| {
                let (endpoint_id, model) = entry.spec.split_once(':').unwrap_or((&entry.spec, ""));
                let endpoint = self.settings.endpoints.get(endpoint_id);
                let pricing = endpoint.and_then(|endpoint| endpoint.pricing.get(model));
                ModelRow {
                    id: entry.id.clone(),
                    spec: entry.spec.clone(),
                    location: entry.location.as_str().into(),
                    tier: entry.tier.as_str().into(),
                    tags: entry.tags.clone(),
                    input_usd_per_mtok: pricing.map(|p| p.input_usd_per_mtok),
                    output_usd_per_mtok: pricing.map(|p| p.output_usd_per_mtok),
                    endpoint_configured: endpoint.is_some(),
                    context_window: entry.context_window,
                    enabled: true,
                }
            })
            .collect();
        ModelListResult {
            models,
            prefer: self.settings.routing.prefer.as_str().into(),
            rules: u32::try_from(self.settings.routing.rules.len()).unwrap_or(u32::MAX),
        }
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

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .get(id)
            .cloned()
    }
}

/// The bundled default workflow pack: `policy/agent-custom-workflow` in the app's resource
/// folder, beside the daemon (or one level up, as archives lay it out), or in the source tree for
/// development builds.
fn bundled_pack() -> Option<PathBuf> {
    let mut candidates = bundled_dirs("policy/agent-custom-workflow");
    candidates
        .push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy/agent-custom-workflow"));
    candidates
        .into_iter()
        .find(|candidate| candidate.join("AGENTS.md").is_file())
}

/// Where files shipped with Imperium may be, most specific first: the folder named by
/// `IMPERIUM_RESOURCE_DIR` (the desktop app sets it to its resource folder when it starts the
/// daemon, since installers keep resources apart from binaries), then beside the daemon and one
/// level up.
pub(crate) fn bundled_dirs(relative: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(resources) = std::env::var_os("IMPERIUM_RESOURCE_DIR") {
        dirs.push(PathBuf::from(resources).join(relative));
    }
    if let Some(beside) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        dirs.push(beside.join(relative));
        dirs.push(beside.join("..").join(relative));
    }
    dirs
}

fn lock_watches(
    watches: &Mutex<HashMap<String, (String, u64)>>,
) -> std::sync::MutexGuard<'_, HashMap<String, (String, u64)>> {
    watches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_pending(
    pending: &Mutex<Vec<PendingBrief>>,
) -> std::sync::MutexGuard<'_, Vec<PendingBrief>> {
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A session as lists show it.
pub fn summary(session: &Session) -> SessionSummary {
    let (status, status_detail) = session.status();
    SessionSummary {
        session_id: session.id().to_string(),
        project_dir: session.project_dir().to_string(),
        model: session.model().to_string(),
        isolation: session.isolation().to_string(),
        title: session.title(),
        status,
        status_detail,
        resume_policy: session.resume_policy(),
        background: session.is_background(),
        hold: session.hold(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_workflow_pack_is_found_and_loads() {
        let pack = bundled_pack().expect("the source tree has policy/agent-custom-workflow");
        let loaded = Pack::load(&pack).unwrap();
        assert!(loaded.roles.contains_key("root") && loaded.roles.contains_key("leaf"));
        assert!(!loaded.candidates.is_empty());
    }
}
