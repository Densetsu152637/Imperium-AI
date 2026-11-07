use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use imperium_config::{Paths, Settings};
use imperium_core::{
    BrowserSetup, CatalogModel, LocalModels, ModelError, Session, SessionConfig, SessionMemory,
    WebTools, new_session_id, resolve_model,
};
use imperium_extensions::{ConfiguredHook, Extensions, Sources};
use imperium_media::{ImageBackend, Media, Service};
use imperium_memory::Store;
use imperium_protocol::jsonrpc::Notification;
use imperium_protocol::methods::{LocalModelListResult, ModelListResult, ModelRow};
use imperium_protocol::session::{AgentEvent, PermissionDecision, ROOT_AGENT, SessionEvent};
use imperium_tools::{Sandbox, Workspace};
use imperium_web::{SearchProvider, Web};
use imperium_workflow::{ContextIndex, Pack};
use tokio::sync::broadcast;

use crate::approvals::Approvals;
use crate::local::LocalRuntime;
use crate::pets::PetEngine;
use crate::phone::Devices;
use crate::project_agent::ProjectAgents;
use crate::scheduler::Scheduler;

/// Options for [`DaemonState::create_session_with`].
#[derive(Debug, Default)]
pub struct SessionOptions {
    /// Tools that run without asking.
    pub pre_approved: Vec<String>,
    pub launcher: Option<Arc<dyn imperium_core::SessionLauncher>>,
    pub instructions: Option<String>,
}

/// State shared by every connection: configuration and the running sessions.
#[derive(Debug)]
pub struct DaemonState {
    started: Instant,
    paths: Paths,
    settings: Settings,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    local: Option<LocalRuntime>,
    /// Notifications for every connected client, such as schedule runs.
    announcements: broadcast::Sender<Notification>,
    scheduler: OnceLock<Arc<Scheduler>>,
    approvals: Approvals,
    project_agents: ProjectAgents,
    pets: OnceLock<Option<Arc<PetEngine>>>,
    devices: Devices,
    phone_address: OnceLock<std::net::SocketAddr>,
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
    /// ([`LocalRuntime::start`]).
    pub fn new(paths: Paths, settings: Settings, local: Option<LocalRuntime>) -> Arc<Self> {
        let devices = Devices::load(paths.data_dir().join("devices.json"));
        Arc::new(DaemonState {
            started: Instant::now(),
            paths,
            settings,
            sessions: Mutex::new(HashMap::new()),
            local,
            announcements: broadcast::channel(256).0,
            scheduler: OnceLock::new(),
            approvals: Approvals::default(),
            project_agents: ProjectAgents::default(),
            pets: OnceLock::new(),
            devices,
            phone_address: OnceLock::new(),
        })
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

    /// Starts GitHub auto-fix when it is enabled and repositories are watched.
    pub fn start_autofix(self: &Arc<Self>, github: Arc<dyn imperium_github::GitHub>) {
        crate::autofix::start(self, github);
    }

    pub fn scheduler(&self) -> Option<&Arc<Scheduler>> {
        self.scheduler.get()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
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
            let api_key = endpoint_settings.api_key.as_ref().and_then(|reference| {
                imperium_config::load_secret(&reference.secret)
                    .ok()
                    .flatten()
            });
            Some(Service {
                base_url: endpoint_settings.base_url.trim_end_matches('/').to_string(),
                api_key,
                model: model.to_string(),
            })
        };
        let image = settings
            .image
            .as_ref()
            .and_then(|image| match image.backend.as_str() {
                "comfyui" => {
                    let workflow = std::fs::read_to_string(image.workflow.as_ref()?).ok()?;
                    Some(ImageBackend::ComfyUi {
                        url: image
                            .comfyui_url
                            .clone()
                            .unwrap_or_else(|| "http://127.0.0.1:8188".into())
                            .trim_end_matches('/')
                            .to_string(),
                        workflow,
                    })
                }
                _ => service(image.endpoint.as_deref()?, &image.model).map(ImageBackend::OpenAi),
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
            imperium_config::load_secret(name).ok().flatten()
        };
        let provider = match web.search.as_str() {
            "searxng" if !web.searxng_url.is_empty() => Some(SearchProvider::Searxng {
                url: web.searxng_url.clone(),
            }),
            "brave" => key().map(|api_key| SearchProvider::Brave { api_key }),
            "tavily" => key().map(|api_key| SearchProvider::Tavily { api_key }),
            _ => None,
        };
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

    /// Where UI mods are installed.
    pub fn mods_dir(&self) -> std::path::PathBuf {
        self.paths.config_dir().join("mods")
    }

    /// Local models, the hardware they are planned against, and what is loaded.
    pub fn local_models(&self) -> LocalModelListResult {
        self.local
            .as_ref()
            .map_or_else(crate::local::disabled, LocalRuntime::listing)
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
        pre_approved: Vec<String>,
        timeout: std::time::Duration,
    ) -> (bool, String, Option<String>) {
        let options = SessionOptions {
            pre_approved,
            ..SessionOptions::default()
        };
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
        let model = resolve_model(&self.settings, model, imperium_config::load_secret)?;
        let catalog = self.catalog();
        let (mut mcp, mut notices) = crate::mcp::session_servers(
            &self.settings,
            Path::new(project_dir),
            imperium_config::load_secret,
        );
        let mut extensions = self.extensions(Path::new(project_dir));
        let context = self.context(Path::new(project_dir), &mut notices);
        mcp.append(&mut extensions.servers);
        notices.append(&mut extensions.notices);
        let id = new_session_id().map_err(|error| CreateError::Start(error.to_string()))?;
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
            routing: self.settings.routing.clone(),
            local: self.local.as_ref().map(|local| {
                let supervisor: Arc<dyn LocalModels> = local.supervisor.clone();
                supervisor
            }),
            mcp,
            notices,
            extensions,
            context,
            enforce: self.settings.workflow.enforce,
            pre_approved: options.pre_approved,
            launcher: options.launcher,
            instructions: options.instructions,
            web: self.web_tools(),
            media: self.media(),
            memory: self.settings.memory.enabled.then(|| SessionMemory {
                store: Store::new(self.paths.data_dir(), Some(Path::new(project_dir))),
                settings: self.settings.memory.clone(),
            }),
            max_steps: self.settings.agents.max_steps,
        })
        .map_err(|error| CreateError::Start(error.to_string()))?;
        let events = session.subscribe();
        if let Some(pets) = self.pets() {
            pets.watch(id.clone(), session.subscribe());
        }
        let session = Arc::new(session);
        self.sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .insert(id, Arc::clone(&session));
        Ok((session, events))
    }

    /// Workflow packs and the project's instructions and documentation, unless the workflow is
    /// turned off. Packs that cannot be read are reported in `notices`.
    fn context(&self, project_dir: &Path, notices: &mut Vec<String>) -> Option<Arc<ContextIndex>> {
        let workflow = &self.settings.workflow;
        if !workflow.enabled {
            return None;
        }
        let packs = workflow
            .packs
            .iter()
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
                match resolve_model(
                    &self.settings,
                    Some(&entry.spec),
                    imperium_config::load_secret,
                ) {
                    Ok(model) => Some(CatalogModel {
                        id: entry.id.clone(),
                        model,
                        tier: entry.tier,
                        location: entry.location,
                        tags: entry.tags.clone(),
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
