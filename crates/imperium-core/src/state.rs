use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant};

use imperium_config::{
    Config, KeychainSecrets, Layer, ModelPlacement, Paths, SecretStore, Settings,
};
use imperium_engine::{
    Background, CatalogModel, ChatClients, LoadView, LocalModels, ModelError, ResolvedModel,
    Session, SessionConfig, SessionMemory, TaskRequest, ToolProvider, new_session_id,
    resolve_model, route,
};
use imperium_extensions::{ConfiguredHook, Extensions, Sources};
use imperium_local::supervisor::SwapDirection;
use imperium_mcp::McpTools;
use imperium_media::{ImageConfig, ImageGenerators, Media, MediaTools, Service};
use imperium_memory::Store;
use imperium_protocol::jsonrpc::Notification;
use imperium_protocol::methods::{
    EnvironmentRuntime, LocalModelListResult, MODEL_RESIDENCY, MODEL_SWAPPED_IN, MODEL_SWAPPED_OUT,
    ModelListResult, ModelResidency, ModelRow, ModelSwap, SESSION_CREATED, SessionDeleteParams,
    SessionDeleteResult, SessionSummary, SessionWorktree,
};
use imperium_protocol::session::{
    AgentEvent, PermissionDecision, ROOT_AGENT, ResumePolicy, SessionEvent, SessionStatus,
};
use imperium_tools::{ProcessRunner, Remote, Runner, Sandbox, Workspace};
use imperium_web::{BrowserSetup, PreviewTools, SearchConfig, SearchProviders, Web, WebTools};
use imperium_workflow::{ContextIndex, Pack, personality};
use tokio::sync::broadcast;

use crate::approvals::Approvals;
use crate::bench::Bench;
use crate::downloads::Downloads;
use crate::environments::{EnvironmentError, Environments};
use crate::load::{self, LoadMonitor};
use crate::local::LocalRuntime;
use crate::pets::PetEngine;
use crate::phone::Devices;
use crate::project_agent::PendingBrief;
use crate::project_agent::ProjectAgents;
use crate::scheduler::Scheduler;
use crate::suggestions::{SuggestTool, Suggestions};

mod agent_config;
mod archive;
mod branch;
mod model_awareness;
mod session_search;
mod titles;
mod workspaces;
pub use archive::ArchiveError;
pub(crate) use workspaces::ROOT_SETTING as WORKSPACES_ROOT_SETTING;

/// Options for [`DaemonState::create_session_with`].
#[derive(Debug, Default)]
pub struct SessionOptions {
    /// Continue this earlier session (after a restart) instead of starting a new one.
    pub resume_id: Option<String>,
    /// Tools that run without asking.
    pub pre_approved: Vec<String>,
    pub launcher: Option<Arc<dyn imperium_engine::SessionLauncher>>,
    pub instructions: Option<String>,
    /// Started by a schedule or a project agent: it gives way to the user's own sessions.
    pub background: bool,
    /// A background session that must not give way to the load: a person ran it now, or it
    /// waited out its `max_defer`.
    pub run_now: bool,
    /// The environment the session works in, recorded with it.
    pub environment_id: Option<String>,
    /// Folders the session may use besides its working directory.
    pub folders: Vec<String>,
    /// Start without a model when none was given and `models.default` is not set: messages
    /// wait until one is chosen. Otherwise a missing model fails the creation.
    pub may_wait_for_model: bool,
    /// Where the session's tools run (its environment's runtime).
    pub runtime: EnvironmentRuntime,
    /// The environment's persistent agent session.
    pub persistent: bool,
    /// Compact the conversation when a turn starts over the model's budget.
    pub auto_compact: bool,
    /// The id for a new session (one is made when unset).
    pub id: Option<String>,
    /// Start on git worktrees (environment sessions only), on `worktree_branch` or
    /// `imperium/<session id>`.
    pub worktree: bool,
    pub worktree_branch: Option<String>,
    /// Worktrees a resumed session was started on.
    pub worktrees: Vec<SessionWorktree>,
    /// The reasoning effort to start with; `models.effort` when unset.
    pub effort: Option<imperium_api::Effort>,
    /// The permission mode to start in; when unset, `permissions.mode` for sessions people
    /// start, and `default` for unattended ones (background or with pre-approved tools), which
    /// nobody is there to answer for.
    pub permission_mode: Option<imperium_api::PermissionMode>,
}

/// Where a session works: its environment and every folder, the working directory first.
#[derive(Debug, Clone, Default)]
struct Placement {
    environment_id: Option<String>,
    folders: Vec<String>,
    persistent: bool,
    worktrees: Vec<SessionWorktree>,
    /// Listed first; recorded in the session's meta file so it survives a restart.
    pinned: bool,
    /// The session's title, once it has one (see [`titles`]); recorded like the pin.
    title: Option<String>,
    /// The user gave the title: a generated one never replaces it.
    title_by_user: bool,
    /// A project agent: like an environment's persistent agent, it runs while the daemon does
    /// and is never archived.
    project_agent: bool,
    /// Branched from another session (`session.branch`); recorded in the meta file.
    branched_from: Option<imperium_protocol::methods::BranchOrigin>,
    /// The comparison a branch belongs to (`session.compare`).
    comparison_id: Option<String>,
}

/// State shared by every connection: configuration and the running sessions.
#[derive(Debug)]
pub struct DaemonState {
    /// This state itself, for tasks and tools started where only `&self` is at hand.
    me: Weak<DaemonState>,
    started: Instant,
    paths: Paths,
    settings: RwLock<Arc<Settings>>,
    /// Serialises saves of model settings.
    pub(crate) config_saves: Mutex<()>,
    /// Serialises reads and writes of the config change journal (`config_changes`).
    pub(crate) config_journal: Mutex<()>,
    /// The configuration layers the settings were built from, so a save rebuilds them with
    /// only the app file replaced; `None` when the caller gave settings alone.
    config_layers: Mutex<Option<Vec<Layer>>>,
    /// Where API keys and tokens are read from: the OS keychain unless the caller chose another.
    secrets: Arc<dyn SecretStore>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Each session's environment and folders, by session id.
    placements: Mutex<HashMap<String, Placement>>,
    environments: Environments,
    /// Set at start when there are model folders, or after the first download.
    local: OnceLock<LocalRuntime>,
    downloads: Downloads,
    bench: Bench,
    /// The latest llama.cpp runtime pack installation (`runtime.install`).
    pub(crate) runtime_install: Mutex<Option<imperium_protocol::methods::RuntimeInstallJob>>,
    /// The latest package-manager update of an external `llama-server`.
    pub(crate) runtime_update: Mutex<Option<imperium_protocol::methods::RuntimeUpdateJob>>,
    /// The Hugging Face Hub to use instead of the one from the environment (for tests).
    hub: OnceLock<imperium_local::hf::Hub>,
    /// The `gh` to use instead of the one on the PATH (for tests).
    gh: OnceLock<PathBuf>,
    /// Notifications for every connected client, such as schedule runs.
    announcements: broadcast::Sender<Notification>,
    scheduler: OnceLock<Arc<Scheduler>>,
    approvals: Approvals,
    project_agents: ProjectAgents,
    pets: OnceLock<Option<Arc<PetEngine>>>,
    devices: Devices,
    phone_address: OnceLock<std::net::SocketAddr>,
    /// The OpenAI-compatible API server for other tools.
    openai: crate::openai::OpenAiApi,
    /// Pull requests sessions watch: session id to (repo, number).
    watches: Mutex<HashMap<String, (String, u64)>>,
    /// Turns what the daemon sees into quiet, busy, or saturated ([`start_sampling`]).
    monitor: Arc<LoadMonitor>,
    /// A load view to use instead of the monitor (for tests).
    load_override: OnceLock<Arc<dyn LoadView>>,
    /// Briefs of project-agent sessions that were waiting for a quiet machine when the daemon
    /// stopped, until the project agents start and pick them up.
    pending_briefs: Mutex<Vec<PendingBrief>>,
    /// Archived sessions, which no longer run, as lists show them, by session id.
    archived: Mutex<HashMap<String, SessionSummary>>,
    /// Signalled when a client asks the daemon to stop.
    shutdown: tokio::sync::Notify,
    /// What `mcp.test` last learned about each connector.
    connectors: crate::connectors::StatusCache,
    /// Model download suggestions offered to sessions.
    suggestions: Suggestions,
    /// Runs processes for remote runtimes; a fake in tests.
    runner: OnceLock<Arc<dyn Runner>>,
    /// Machine usage for `system.resources` and its watchers.
    resources: Arc<crate::resources::ResourceMonitor>,
    /// Global and environment permission rules as running sessions see them, and the global
    /// path grants.
    permissions: crate::permissions::PermissionStore,
    /// Budgets and the usage ledger they count.
    budgets: crate::budgets::BudgetBook,
    /// The session search index, opened on first use.
    session_index: OnceLock<Arc<imperium_memory::sessions::SessionIndex>>,
    /// What calls to each model measured, for model cards.
    measurements: model_awareness::Measurements,
    /// Model card vectors for `list_models` searches by meaning.
    model_cards: imperium_memory::models::CardIndex,
    /// Each running session's checkpoints, when it records them.
    checkpoints: Mutex<HashMap<String, Arc<crate::checkpoints::SessionCheckpoints>>>,
    /// Budget stops the person approved, by session, budget, and window.
    budget_confirmed: Mutex<HashSet<String>>,
    /// Test hook: the day session gates count as today (`None`: the local date).
    #[cfg(any(test, feature = "testing"))]
    budget_day: Mutex<Option<chrono::NaiveDate>>,
    /// Each running session's browser, for the user to take over and drive.
    browsers: Mutex<HashMap<String, BrowserSetup>>,
    /// Test hook: make `session.compare` fail after creating this many branches (0: never).
    compare_fault: std::sync::atomic::AtomicUsize,
    /// Serialises moves of the environments' working folders.
    workspaces_lock: Mutex<()>,
    /// Why the working folders are not where the configuration says (a move that failed at start).
    workspaces_warning: Mutex<Option<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("project folder {0}: {1}")]
    Project(String, std::io::Error),
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("could not start the session: {0}")]
    Start(String),
    #[error("the environment's runtime: {0}")]
    Runtime(String),
    #[error("could not start on a worktree: {0}")]
    Worktree(String),
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
        let environments = Environments::load(paths.data_dir().join("environments.json"));
        let suggest = settings.models.suggest_downloads;
        let permissions = crate::permissions::PermissionStore::new(&settings.permissions);
        let budgets = crate::budgets::BudgetBook::load(
            paths.data_dir().join("usage.json"),
            settings.budgets.clone(),
        );
        let state = Arc::new_cyclic(|me| DaemonState {
            me: me.clone(),
            started: Instant::now(),
            paths,
            settings: RwLock::new(Arc::new(settings)),
            config_saves: Mutex::new(()),
            config_journal: Mutex::new(()),
            config_layers: Mutex::new(None),
            secrets,
            sessions: Mutex::new(HashMap::new()),
            placements: Mutex::new(HashMap::new()),
            environments,
            local: local.map(OnceLock::from).unwrap_or_default(),
            downloads: Downloads::default(),
            bench: Bench::default(),
            runtime_install: Mutex::new(None),
            runtime_update: Mutex::new(None),
            hub: OnceLock::new(),
            gh: OnceLock::new(),
            announcements: broadcast::channel(256).0,
            scheduler: OnceLock::new(),
            approvals: Approvals::default(),
            project_agents: ProjectAgents::default(),
            pets: OnceLock::new(),
            devices,
            phone_address: OnceLock::new(),
            openai: crate::openai::OpenAiApi::default(),
            watches: Mutex::new(HashMap::new()),
            monitor,
            load_override: OnceLock::new(),
            pending_briefs: Mutex::new(Vec::new()),
            archived: Mutex::new(HashMap::new()),
            shutdown: tokio::sync::Notify::new(),
            connectors: crate::connectors::StatusCache::default(),
            suggestions: Suggestions::new(suggest),
            runner: OnceLock::new(),
            resources: Arc::default(),
            permissions,
            budgets,
            session_index: OnceLock::new(),
            measurements: model_awareness::Measurements::default(),
            model_cards: imperium_memory::models::CardIndex::new(),
            checkpoints: Mutex::new(HashMap::new()),
            budget_confirmed: Mutex::new(HashSet::new()),
            #[cfg(any(test, feature = "testing"))]
            budget_day: Mutex::new(None),
            browsers: Mutex::new(HashMap::new()),
            compare_fault: std::sync::atomic::AtomicUsize::new(0),
            workspaces_lock: Mutex::new(()),
            workspaces_warning: Mutex::new(None),
        });
        state.apply_load_view();
        if let Some(local) = state.local.get() {
            local.set_runtime_hooks(crate::llama_runtime::Hooks::new(&state));
            if tokio::runtime::Handle::try_current().is_ok() {
                state.announce_swaps(local);
                state.announce_moves(local);
            }
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
                Duration::from_secs(self.settings().load.interactive_hold_min * 60),
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

    /// Tells every client each move of local-model memory, with its reason
    /// (`model.residency`).
    fn announce_moves(self: &Arc<Self>, local: &LocalRuntime) {
        let mut moves = local.supervisor.moves();
        let state = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let moved = match moves.recv().await {
                    Ok(moved) => moved,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let Some(state) = state.upgrade() else {
                    return;
                };
                let bytes = match moved.action {
                    imperium_local::residency::Action::SetBudget { vram_bytes, .. } => vram_bytes,
                    _ => moved.bytes,
                };
                state.announce(
                    MODEL_RESIDENCY,
                    ModelResidency {
                        action: moved.action.name().to_string(),
                        model_id: moved.action.model().map(str::to_string),
                        holder: moved.action.holder().map(str::to_string),
                        reason: moved.reason.name().to_string(),
                        ok: moved.ok,
                        detail: moved.detail,
                        seconds: moved.seconds,
                        bytes,
                    },
                );
            }
        });
    }

    /// What runs processes for remote runtimes (`docker`, `wsl.exe`, `ssh`).
    pub fn runner(&self) -> Arc<dyn Runner> {
        Arc::clone(self.runner.get_or_init(|| Arc::new(ProcessRunner)))
    }

    /// Uses `runner` for remote runtimes instead of starting real processes (for tests). Call
    /// before the first session.
    pub fn use_runner(&self, runner: Arc<dyn Runner>) {
        let _ = self.runner.set(runner);
    }

    /// The workspace for a session in `project_dir` (plus `folders`) under `runtime`, with the
    /// path grants and permission rules of `environment_id` and the global ones. A remote
    /// runtime gets only the environment's grants, since the global ones are paths on this
    /// machine.
    fn workspace(
        &self,
        project_dir: &str,
        folders: &[String],
        runtime: &EnvironmentRuntime,
        environment_id: Option<&str>,
    ) -> Result<Workspace, CreateError> {
        let env = environment_id.and_then(|id| self.environments.get(id));
        let permissions = self.permissions.for_session(
            env.as_ref()
                .map(|env| (env.id.as_str(), env.permissions.rules.as_slice())),
        );
        let mut grants: Vec<imperium_tools::permissions::PathGrant> = env
            .iter()
            .flat_map(|env| &env.permissions.grants)
            .map(crate::permissions::grant_from_wire)
            .collect();
        Ok(self
            .bare_workspace(project_dir, folders, runtime, &mut grants)?
            .with_grants(&grants)
            .with_permissions(permissions))
    }

    /// The workspace's folders and sandbox; adds the global grants to `grants` for a local one.
    fn bare_workspace(
        &self,
        project_dir: &str,
        folders: &[String],
        runtime: &EnvironmentRuntime,
        grants: &mut Vec<imperium_tools::permissions::PathGrant>,
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
                network: true,
            },
            _ => self.sandbox(),
        };
        grants.extend(self.permissions.global_grants());
        Ok(Workspace::new(Path::new(project_dir))
            .map_err(|error| CreateError::Project(project_dir.to_string(), error))?
            .with_folders(folders)
            .map_err(|error| CreateError::Project(folders.join(", "), error))?
            .with_sandbox(sandbox))
    }

    /// What connector tests last learned about each MCP server.
    pub fn connector_status(&self) -> &crate::connectors::StatusCache {
        &self.connectors
    }

    /// Model download suggestions and the live `models.suggest_downloads` switch.
    pub fn suggestions(&self) -> &Suggestions {
        &self.suggestions
    }

    /// For suggestions: whether `routing.prefer` is `local`, and what each local model can do.
    pub fn suggestion_inputs(&self) -> (bool, Vec<imperium_local::suggest::LocalCapability>) {
        let prefer_local = self.settings().routing.prefer == imperium_config::Preference::Local;
        let local = self.local.get().map_or_else(Vec::new, |local| {
            local
                .supervisor
                .models()
                .iter()
                .map(|model| imperium_local::suggest::LocalCapability {
                    context: model.info.context_length,
                    vision: model.mmproj.is_some(),
                })
                .collect()
        });
        (prefer_local, local)
    }

    /// Whether `model` (a spec or model table id) can view images: its model table row is
    /// tagged `vision`, or it is a local llama.cpp model with a multimodal projector beside it.
    pub fn model_has_vision(&self, model: &str) -> bool {
        let settings = self.settings();
        let tagged = settings.models.catalog.iter().any(|entry| {
            (entry.spec == model || entry.id == model)
                && entry
                    .tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case("vision"))
        });
        if tagged {
            return true;
        }
        let Some(id) = model.strip_prefix(&format!("{}:", imperium_config::LLAMACPP_ENDPOINT))
        else {
            return false;
        };
        self.local.get().is_some_and(|local| {
            local
                .supervisor
                .models()
                .iter()
                .any(|found| found.id == id && found.mmproj.is_some())
        })
    }

    /// Media backends from `[media]` (for push-to-talk transcription).
    pub fn media_backends(&self) -> Option<Arc<Media>> {
        self.media()
    }

    /// Whether routing an ordinary task (default tier, no tags) for a session on `model` picks
    /// a local model; `None` when the session's model cannot be resolved.
    pub fn routes_locally(&self, model: &str) -> Option<bool> {
        let fallback = self.resolve_model(Some(model)).ok()?;
        let local = self.local.get().map(|local| {
            let supervisor: Arc<dyn LocalModels> = local.supervisor.clone();
            supervisor
        });
        let routed = route(
            &self.catalog(),
            &self.settings().routing,
            &fallback,
            TaskRequest::default(),
            local.as_deref(),
        );
        Some(routed.local)
    }

    /// Starts running `[[schedules]]`. Call once, inside the Tokio runtime.
    pub fn start_scheduler(self: &Arc<Self>) -> Arc<Scheduler> {
        Arc::clone(self.scheduler.get_or_init(|| Scheduler::start(self)))
    }

    pub fn devices(&self) -> &Devices {
        &self.devices
    }

    pub(crate) fn openai(&self) -> &crate::openai::OpenAiApi {
        &self.openai
    }

    /// Starts the phone listener when `phone.bind` is set. Call once.
    pub async fn start_phone(self: &Arc<Self>) -> std::io::Result<Option<std::net::SocketAddr>> {
        let Some(bind) = &self.settings().phone.bind else {
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

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn config_dir(&self) -> &Path {
        self.paths.config_dir()
    }

    pub fn project_agents(&self) -> &ProjectAgents {
        &self.project_agents
    }

    /// Starts the agents of projects with `agent = true`, and the persistent agents of
    /// environments that have one turned on.
    pub fn start_project_agents(self: &Arc<Self>) {
        crate::project_agent::start(self);
        crate::project_agent::start_environment_agents(self);
    }

    /// Starts or stops an environment's persistent agent to match its setting.
    pub fn sync_environment_agent(self: &Arc<Self>, environment_id: &str) {
        crate::project_agent::sync_environment_agent(self, environment_id);
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
        let poll = std::time::Duration::from_secs(self.settings().github.autofix.poll_s.max(10));
        crate::autofix::watch_pull_requests(self, Arc::clone(&github), poll);
        crate::autofix::start(self, github);
    }

    pub fn scheduler(&self) -> Option<&Arc<Scheduler>> {
        self.scheduler.get()
    }

    /// The settings in effect. Saving model settings replaces the snapshot, so callers read it
    /// when they need it rather than keeping it.
    pub fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.settings.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Keeps the layers `config` merged, which the settings were built from: saving model
    /// settings then changes only what the app file sets, never what other files say now.
    pub fn remember_config(&self, config: &Config) {
        *self
            .config_layers
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(config.layers().to_vec());
    }

    /// The layers kept by [`DaemonState::remember_config`].
    pub(crate) fn config_layers(&self) -> Option<Vec<Layer>> {
        self.config_layers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replaces the settings snapshot, such as after the app saved model settings.
    pub(crate) fn replace_settings(&self, settings: Settings) {
        *self
            .settings
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(settings);
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
    fn web_tools(&self) -> Option<WebTools> {
        let web = &self.settings().web;
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
                takeover: Arc::default(),
            });
        Some(WebTools {
            web: Web::new(provider),
            allow_domains: web.allow_domains.clone(),
            browser,
        })
    }

    /// The isolation tier commands run under, from `[isolation]`.
    fn sandbox(&self) -> Sandbox {
        let isolation = &self.settings().isolation;
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
    fn resume_from_meta(
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
        let resume = &self.settings().resume;
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

    /// The local model runtime, when it is running.
    /// This state as an `Arc`, for work handed to another task; `None` while it is dropped.
    pub(crate) fn this(&self) -> Option<Arc<DaemonState>> {
        self.me.upgrade()
    }

    pub(crate) fn local_runtime(&self) -> Option<&LocalRuntime> {
        self.local.get()
    }

    /// Local models, the hardware they are planned against, and what is loaded.
    pub fn local_models(&self) -> LocalModelListResult {
        let settings = self.settings();
        match self.local.get() {
            Some(local) => local.listing_with(&settings),
            None => LocalModelListResult {
                configured_folders: settings
                    .models
                    .folders
                    .iter()
                    .map(|folder| folder.display().to_string())
                    .collect(),
                default_model: settings.models.default.clone(),
                ..crate::local::disabled()
            },
        }
    }

    /// The resource monitor behind `system.resources`.
    pub fn resource_monitor(&self) -> &Arc<crate::resources::ResourceMonitor> {
        &self.resources
    }

    /// Local models loaded now, in use or idle.
    pub fn loaded_local_models(&self) -> Vec<imperium_local::supervisor::LoadedStatus> {
        self.local
            .get()
            .map(|local| local.supervisor.loaded())
            .unwrap_or_default()
    }

    /// Unloads an idle local model now.
    ///
    /// A model with a slot in use, a call queued, or an agent mid-turn on it is not unloaded.
    pub async fn unload_local_model(&self, model: &str) -> Result<(), String> {
        let residency = self
            .local
            .get()
            .map(|local| Arc::clone(&local.residency))
            .ok_or_else(|| format!("{model} is not loaded: no local runtime is running"))?;
        residency.unload(model).await
    }

    /// The memory local models may use, when the local runtime is running.
    pub fn local_budget(&self) -> Option<imperium_local::planner::Budget> {
        self.local.get().map(LocalRuntime::budget)
    }

    /// Scans the model folders again (after a download), starting the local runtime when it
    /// was not running for want of folders. Must run inside the Tokio runtime; it blocks.
    pub fn rescan_local_models(self: &Arc<Self>) {
        if let Some(local) = self.local.get() {
            local.rescan(&self.paths, &self.settings());
            return self.apply_local_settings(local);
        }
        let Some(started) = LocalRuntime::start(&self.paths, &self.settings()) else {
            return;
        };
        if self.local.set(started).is_ok() {
            if let Some(local) = self.local.get() {
                local.set_runtime_hooks(crate::llama_runtime::Hooks::new(self));
                self.apply_local_settings(local);
                self.announce_swaps(local);
                self.announce_moves(local);
            }
        } else if let Some(local) = self.local.get() {
            // Another download started it first, perhaps before this one's files landed.
            local.rescan(&self.paths, &self.settings());
            self.apply_local_settings(local);
        }
    }

    /// Hands the local runtime the per-model settings in effect, read under the lock that
    /// serialises saves: a save is never overwritten by a snapshot taken before it.
    fn apply_local_settings(&self, local: &LocalRuntime) {
        let _saving = self
            .config_saves
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        local.apply_model_settings(&self.settings());
    }

    pub fn downloads(&self) -> &Downloads {
        &self.downloads
    }

    /// Local model benchmark runs ([`crate::bench`]).
    pub fn bench(&self) -> &Bench {
        &self.bench
    }

    /// The Hugging Face Hub: `HF_ENDPOINT` and a token from `HF_TOKEN` or the secret store,
    /// read on each use so a newly stored token is picked up.
    pub fn hub(&self) -> imperium_local::hf::Hub {
        self.hub
            .get()
            .cloned()
            .unwrap_or_else(|| imperium_local::hf::Hub::from_env(self.secrets.as_ref()))
    }

    /// Uses `gh` (GitHub's CLI) at this path instead of the one on the PATH, such as a fake in
    /// tests. Call once.
    pub fn use_gh(&self, gh: PathBuf) {
        let _ = self.gh.set(gh);
    }

    /// The `gh` set with [`Self::use_gh`], if any.
    pub(crate) fn gh_override(&self) -> Option<PathBuf> {
        self.gh.get().cloned()
    }

    /// Uses `hub` instead of the environment's, such as a local fake in tests. Call once.
    pub fn use_hub(&self, hub: imperium_local::hf::Hub) {
        let _ = self.hub.set(hub);
    }

    /// Asks the daemon to stop; `main` waits on [`Self::shutdown_requested`].
    pub fn request_shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// Completes when a client has asked the daemon to stop.
    pub async fn shutdown_requested(&self) {
        self.shutdown.notified().await;
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
        let mut workspace = self.workspace(
            project_dir,
            &options.folders,
            &options.runtime,
            options.environment_id.as_deref(),
        )?;
        // An environment's sessions also get its repositories' worktrees, unless it turned them
        // off. A session started on its own worktrees keeps to them, so its edits stay apart
        // from the user's checkout.
        if options.worktrees.is_empty()
            && options
                .environment_id
                .as_deref()
                .and_then(|id| self.environments.get(id))
                .is_some_and(|env| env.project_worktrees)
        {
            workspace = workspace.with_project_worktrees();
        }
        let unattended = options.background || !options.pre_approved.is_empty();
        let permission_mode = options.permission_mode.unwrap_or(if unattended {
            imperium_api::PermissionMode::Default
        } else {
            self.settings().permissions.mode
        });
        workspace.permissions().set_mode(permission_mode);
        let mut placement = Placement {
            environment_id: options.environment_id.clone(),
            folders: workspace.folders_display(),
            persistent: options.persistent,
            worktrees: options.worktrees.clone(),
            pinned: false,
            title: None,
            title_by_user: false,
            project_agent: options.launcher.is_some(),
            branched_from: None,
            comparison_id: None,
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
        let model = match self.resolve_model(model) {
            Ok(model) => Some(model),
            Err(ModelError::NoDefault) if options.may_wait_for_model => None,
            Err(error) => return Err(error.into()),
        };
        let catalog = self.session_catalog();
        // Connectors added or changed in the app since the daemon started apply to new sessions.
        let fresh = crate::connectors::saved_settings(&self.paths);
        let (mcp, mut notices) = crate::mcp::session_servers(
            fresh.as_ref().unwrap_or(&*self.settings()),
            project_dir_path,
            |name| self.secrets.load(name),
        );
        // Plugins' servers are not among `mcp`: they start when an agent activates the plugin.
        let mut extensions = self.extensions(project_dir_path);
        let context = self.context(project_dir_path, &mut notices);
        let personality = self.personality(&mut notices);
        notices.append(&mut extensions.notices);
        let resume = options.resume_id.is_some();
        let id = match options.resume_id.clone().or_else(|| options.id.clone()) {
            Some(id) => id,
            None => new_session_id().map_err(|error| CreateError::Start(error.to_string()))?,
        };
        let remember = options.launcher.is_none();
        // Files attached from outside the session's folders are copied into its attachments
        // folder, which its tools may read. A remote runtime cannot reach this machine's
        // folders, so it gets no grant (attached text is inlined either way).
        let workspace = if workspace.runtime().is_none() {
            let store = crate::attachments::store_dir(self.paths.data_dir(), &id);
            match std::fs::create_dir_all(&store) {
                Ok(()) => workspace.with_grants(&[imperium_tools::permissions::PathGrant {
                    path: store.display().to_string(),
                    mode: imperium_tools::permissions::PathMode::Read,
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
        let _workspaces = self
            .workspaces_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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
        }
        // Tool providers in the order their tools are offered (MCP tools always come first).
        let mut tools: Vec<Arc<dyn ToolProvider>> = Vec::new();
        if !mcp.is_empty() {
            tools.push(Arc::new(McpTools::new(mcp)));
        }
        if let Some(media) = self.media() {
            tools.push(Arc::new(MediaTools::new(media)));
        }
        if let Some(web) = self.web_tools() {
            if let Some(browser) = &web.browser {
                self.browsers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id.clone(), browser.clone());
            }
            tools.push(Arc::new(web));
        }
        // A preview's dev server runs where the session's commands do, so only a session on
        // this machine offers one; it stops when the session's tools are dropped.
        if !remote {
            tools.push(Arc::new(PreviewTools::new(workspace.clone())));
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
        let model_spec = model.as_ref().map(|model| model.spec.clone());
        let effort = options.effort.unwrap_or(self.settings().models.effort);
        let journal_path = self
            .paths
            .data_dir()
            .join("sessions")
            .join(format!("{id}.jsonl"));
        // Writer subagents started with `access: "write"` get worktrees of their own (local
        // folders only).
        let workspace = if remote {
            workspace
        } else {
            workspace.with_writer_trees(Arc::new(crate::writers::GitWriterTrees::new(
                &id,
                &journal_path,
                self.worktree_dir(),
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
        let session = Session::spawn(SessionConfig {
            effort,
            journal_path,
            budget_gate: Some(Arc::new(crate::budgets::CallGate::new(
                self.me.clone(),
                id.clone(),
            ))),
            checkpoints: checkpoints.clone().map(|checkpoints| {
                let hook: Arc<dyn imperium_engine::TurnCheckpoints> = checkpoints;
                hook
            }),
            id: id.clone(),
            workspace,
            model,
            catalog,
            clients: ChatClients::default(),
            routing: self.settings().routing.clone(),
            local: self.local.get().map(|local| {
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
            instructions: options.instructions,
            personality,
            background: options.background.then(|| Background {
                load: self.load_view(),
            }),
            memory: self.settings().memory.enabled.then(|| SessionMemory {
                store: Store::new(self.paths.data_dir(), (!remote).then_some(project_dir_path)),
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
                            let root = me
                                .upgrade()
                                .map_or_else(|| fallback.clone(), |state| state.workspaces_root());
                            crate::workspaces::memory_file(&root, &environment).unwrap_or_default()
                        })
                    }),
            }),
            max_steps: self.settings().agents.max_steps,
            auto_compact: options.auto_compact,
            auto_compact_at: Some(self.settings().context.auto_compact_at),
            keep_agent_state: remember && self.settings().sessions.agent_state_keep,
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
                "environment_id": placement.environment_id,
                "folders": placement.folders,
                "worktrees": placement.worktrees,
                "pinned": placement.pinned,
                "title": placement.title,
                "title_by_user": placement.title_by_user,
                "branched_from": placement.branched_from,
                "comparison_id": placement.comparison_id,
                "effort": effort.as_str(),
                "permission_mode": permission_mode.as_str(),
            });
            let path = self.meta_path(&id);
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
        if let Some(me) = self.me.upgrade() {
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
        self.announce(SESSION_CREATED, self.summary(&session));
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
        if env.folders.is_empty() {
            return Err(EnvironmentError::Invalid(format!(
                "the environment {} has no folders yet; add one first",
                env.name
            ))
            .into());
        }
        let mut options = options;
        let mut folders = env.folders.clone();
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
            let dir = self.worktree_dir();
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
        self.create_session_with(first, model, options)
    }

    /// Stops and forgets a session so it does not continue after a restart, removing its
    /// worktrees when asked (their branches stay). An environment's persistent agent is refused:
    /// it is turned off in its environment instead.
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
                "this is its environment's persistent agent; turn it off in the environment".into(),
            );
        }
        self.forget_session(id);
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
        let mut result = SessionDeleteResult::default();
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
                (result.removed_worktrees, result.failed) = crate::worktrees::remove(&own);
            }
        }
        // So do the worktrees of its writer subagents, on the same terms (branches stay).
        let journal = sessions.join(format!("{id}.jsonl"));
        if remove_worktrees {
            let writers = crate::writers::GitWriterTrees::recorded(&journal);
            if !writers.is_empty() {
                let (removed, failed) = crate::worktrees::remove(&writers);
                result.removed_worktrees.extend(removed);
                result.failed.extend(failed);
            }
        }
        crate::writers::GitWriterTrees::forget(&journal);
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
    fn worktree_dir(&self) -> Option<PathBuf> {
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

    /// Worktree folders that running and archived sessions work in.
    fn worktree_paths_in_use(&self) -> Vec<String> {
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

    /// The budgets and the usage they count.
    pub fn budgets(&self) -> &crate::budgets::BudgetBook {
        &self.budgets
    }

    /// Whether the person approved this budget stop (an `ask` budget) for the session.
    pub(crate) fn budget_confirmed(&self, key: &str) -> bool {
        self.budget_confirmed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(key)
    }

    /// The day session gates count as today ([`DaemonState::set_budget_today`] overrides it in
    /// tests).
    pub(crate) fn budget_today(&self) -> chrono::NaiveDate {
        #[cfg(any(test, feature = "testing"))]
        if let Some(day) = *self
            .budget_day
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return day;
        }
        chrono::Local::now().date_naive()
    }

    /// Test hook: session gates (`stops_for`, so turn-start checks and `CallGate`) count `day`
    /// as today; recording usage, standings, and alerts keep using the real date. `None` undoes
    /// it. Compiled only for this crate's tests or with the `testing` feature.
    #[cfg(any(test, feature = "testing"))]
    pub fn set_budget_today(&self, day: Option<chrono::NaiveDate>) {
        *self
            .budget_day
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = day;
    }

    pub(crate) fn confirm_budget(&self, key: &str) {
        self.budget_confirmed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_string());
    }

    /// The permission rules running sessions check calls against, and the global grants.
    pub fn permissions(&self) -> &crate::permissions::PermissionStore {
        &self.permissions
    }

    /// Applies environment `id`'s saved rules to its running sessions (its grants apply to
    /// sessions started from now on) and tells every client.
    pub fn environment_permissions_changed(&self, id: &str) {
        if let Some(env) = self.environments.get(id) {
            self.permissions.set_environment(id, &env.permissions.rules);
        }
        self.announce_environments();
    }

    /// Tells every client the environments changed.
    pub fn announce_environments(&self) {
        self.announce(
            imperium_protocol::methods::ENVIRONMENTS_CHANGED,
            self.environment_list(),
        );
    }

    fn meta_path(&self, id: &str) -> PathBuf {
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

    /// Workflow packs and the project's instructions and documentation, unless the workflow is
    /// turned off. Packs that cannot be read are reported in `notices`.
    fn context(&self, project_dir: &Path, notices: &mut Vec<String>) -> Option<Arc<ContextIndex>> {
        let workflow = &self.settings().workflow;
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
                // Folders the user configured are trusted; the bundled snapshot is not.
                Ok(pack) if workflow.packs.contains(root) => Some(pack.trusted()),
                Ok(pack) => Some(pack),
                Err(error) => {
                    notices.push(format!("workflow pack skipped: {error}"));
                    None
                }
            })
            .collect();
        Some(Arc::new(ContextIndex::new(packs, project_dir)))
    }

    /// Where personalities are looked up: `<config>/personalities/`, the workflow folders,
    /// then the bundled pack (with the default compiled in as the last resort).
    pub(crate) fn personality_sources(&self) -> personality::Sources {
        personality::Sources {
            config_dir: Some(self.paths.config_dir().to_path_buf()),
            packs: self.settings().workflow.packs.clone(),
            built_in: bundled_pack(),
        }
    }

    /// The root agent's personality block for a new session, from `policy.personality`. An id
    /// that is not found falls back to the default, with a notice in `notices`.
    fn personality(&self, notices: &mut Vec<String>) -> Option<String> {
        let selection = self
            .personality_sources()
            .select(&self.settings().policy.personality);
        if let Some(warning) = selection.warning {
            tracing::warn!("{warning}");
            notices.push(warning);
        }
        selection
            .personality
            .map(|personality| personality.instructions())
    }

    /// Skills, hooks, slash commands, and plugins for a session in `project_dir`.
    fn extensions(&self, project_dir: &Path) -> Extensions {
        imperium_extensions::load(&Sources {
            config_dir: self.paths.config_dir().to_path_buf(),
            project_dir: project_dir.to_path_buf(),
            plugin_dirs: self.settings().extensions.plugin_dirs.clone(),
            disabled_plugins: self.settings().extensions.disabled_plugins.clone(),
            hooks: self
                .settings()
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
            lazy: self.settings().extensions.lazy,
            eager: self.settings().extensions.eager.clone(),
        })
    }

    /// Resolves a model a client asked a session to switch to: a model table id or an
    /// `<endpoint>:<model>` spec, or `None` for `models.default`.
    pub fn resolve_model(&self, requested: Option<&str>) -> Result<ResolvedModel, ModelError> {
        let settings = self.settings();
        let spec = requested.map(|requested| {
            settings
                .models
                .catalog
                .iter()
                .find(|entry| entry.id == requested)
                .map_or(requested, |entry| entry.spec.as_str())
        });
        resolve_model(&settings, spec, |name| self.secrets.load(name))
    }

    /// Checks, without loading or starting anything, that a model a client picked for a session
    /// can be asked for: a local model needs the local runtime and must be among the models it
    /// found. Whether it loads (its server, the memory it needs) is found out when the session
    /// first sends it a message.
    pub fn check_selectable(&self, model: &ResolvedModel) -> Result<(), String> {
        if !model.local {
            return Ok(());
        }
        let Some(local) = self.local.get() else {
            return Err(format!(
                "{} is a local model, but no local models are set up; add a model folder in \
                 Settings → Models",
                model.spec
            ));
        };
        if !LocalModels::has_model(&*local.supervisor, &model.model) {
            return Err(format!(
                "{} was not found in the model folders; rescan them, or pick another model",
                model.spec
            ));
        }
        Ok(())
    }

    /// Resolves `[[models.catalog]]` for routing subagents. Entries that cannot be used (an
    /// unknown endpoint, a missing key) are skipped with a warning rather than failing the session.
    fn catalog(&self) -> Vec<CatalogModel> {
        let settings = self.settings();
        settings
            .models
            .catalog
            .iter()
            .filter(|entry| entry.enabled)
            .filter_map(|entry| {
                match resolve_model(&settings, Some(&entry.spec), |name| self.secrets.load(name)) {
                    Ok(model) => Some(CatalogModel {
                        id: entry.id.clone(),
                        model,
                        tier: entry.tier,
                        location: entry.location,
                        tags: entry.tags.clone(),
                        named_only: entry.placement == ModelPlacement::Cpu,
                    }),
                    Err(error) => {
                        tracing::warn!(model = %entry.spec, %error, "skipping catalog model");
                        None
                    }
                }
            })
            .collect()
    }

    /// Records a completed model call's speed for the model's card.
    pub(crate) fn record_model_call(
        &self,
        spec: &str,
        tokens_per_s: Option<f64>,
        first_token_ms: Option<u64>,
    ) {
        self.measurements.record(spec, tokens_per_s, first_token_ms);
    }

    /// The configured model table, with prices from each endpoint's pricing.
    pub fn model_table(&self) -> ModelListResult {
        let settings = self.settings();
        let models = settings
            .models
            .catalog
            .iter()
            .map(|entry| {
                let (endpoint_id, model) = entry.spec.split_once(':').unwrap_or((&entry.spec, ""));
                let endpoint = settings.endpoints.get(endpoint_id);
                let pricing = endpoint.and_then(|endpoint| endpoint.pricing.get(model));
                ModelRow {
                    id: entry.id.clone(),
                    spec: entry.spec.clone(),
                    alias: entry.alias.clone(),
                    location: entry.location.as_str().into(),
                    tier: entry.tier.as_str().into(),
                    tags: entry.tags.clone(),
                    input_usd_per_mtok: pricing.map(|p| p.input_usd_per_mtok),
                    output_usd_per_mtok: pricing.map(|p| p.output_usd_per_mtok),
                    endpoint_configured: endpoint.is_some(),
                    context_window: entry.context_window,
                    enabled: entry.enabled,
                    reasoning: self.model_reasons(&entry.spec),
                }
            })
            .collect();
        ModelListResult {
            models,
            prefer: self.settings().routing.prefer.as_str().into(),
            rules: u32::try_from(self.settings().routing.rules.len()).unwrap_or(u32::MAX),
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

/// Whether two paths name the same folder (spelled alike, or the same place on disk).
fn same_path(a: &str, b: &str) -> bool {
    a == b
        || matches!((Path::new(a).canonicalize(), Path::new(b).canonicalize()), (Ok(x), Ok(y)) if x == y)
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

/// A session as lists show it, without its environment ([`DaemonState::summary`] adds it).
fn summary(session: &Session) -> SessionSummary {
    let (status, status_detail) = session.status();
    SessionSummary {
        session_id: session.id().to_string(),
        project_dir: session.project_dir().to_string(),
        environment_id: None,
        folders: vec![session.project_dir().to_string()],
        model: session.model(),
        effort: Some(session.effort().as_str().to_string()),
        permission_mode: Some(session.permission_mode().as_str().to_string()),
        reasoning: false,
        isolation: session.isolation().to_string(),
        title: session.title(),
        status,
        status_detail,
        resume_policy: session.resume_policy(),
        background: session.is_background(),
        hold: session.hold(),
        persistent: false,
        worktrees: Vec::new(),
        pinned: false,
        archived: false,
        archived_at: None,
        created_at: None,
        branched_from: None,
        comparison_id: None,
    }
}

/// When a session was created: its journal is written when the session starts.
pub(crate) fn created_at(journal: &Path) -> Option<u64> {
    let created = std::fs::metadata(journal).ok()?.created().ok()?;
    let since = created.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// Answers a batch of questions with the agent's own recommendations: an unattended session
/// (a project agent, a scheduled task) has nobody to ask.
pub fn answer_with_recommendations(
    session: &Session,
    request_id: u64,
    questions: &[imperium_protocol::session::Question],
) {
    let answers = questions
        .iter()
        .map(|question| question.recommended.clone())
        .collect();
    let _ = session.answer(request_id, answers);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(home: &Path, extra: &str) -> Arc<DaemonState> {
        let settings = imperium_config::Config::from_layers([
            imperium_config::Layer::defaults(),
            imperium_config::Layer::new(
                imperium_config::Source::User("test".into()),
                extra.parse().unwrap(),
            ),
        ])
        .settings()
        .unwrap();
        DaemonState::new(imperium_config::Paths::under(home), settings, None)
    }

    /// The root agent's personality block for a new session, and its notices.
    fn personality(home: &Path, extra: &str) -> (Option<String>, Vec<String>) {
        let mut notices = Vec::new();
        let block = state_with(home, extra).personality(&mut notices);
        (block, notices)
    }

    #[test]
    fn with_no_configuration_the_root_personality_is_marcus_aurelius() {
        let home = tempfile::tempdir().unwrap();
        let (block, notices) = personality(home.path(), "");
        let block = block.unwrap();
        assert!(
            block.contains("<personality name=\"Marcus Aurelius\">"),
            "{block}"
        );
        assert!(block.contains("Stoic"));
        assert!(notices.is_empty());
    }

    #[test]
    fn the_configured_personality_wins_and_none_turns_it_off() {
        let home = tempfile::tempdir().unwrap();
        let (block, _) = personality(home.path(), "[policy]\npersonality = \"luxion\"\n");
        assert!(block.unwrap().contains("<personality name=\"Luxion\">"));
        for off in ["none", ""] {
            let (block, notices) =
                personality(home.path(), &format!("[policy]\npersonality = \"{off}\"\n"));
            assert!(block.is_none() && notices.is_empty(), "{off:?}");
        }
    }

    #[test]
    fn a_local_personality_overrides_the_pack_one() {
        let home = tempfile::tempdir().unwrap();
        let paths = imperium_config::Paths::under(home.path());
        let local = paths.config_dir().join("personalities");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("luxion.md"), "# Luxion\n\nMY OWN LUXION\n").unwrap();
        let (block, _) = personality(home.path(), "[policy]\npersonality = \"luxion\"\n");
        assert!(block.unwrap().contains("MY OWN LUXION"));
    }

    #[test]
    fn an_unknown_personality_falls_back_to_the_default_with_a_notice() {
        let home = tempfile::tempdir().unwrap();
        let (block, notices) = personality(home.path(), "[policy]\npersonality = \"nobody\"\n");
        assert!(block.unwrap().contains("Marcus Aurelius"));
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("nobody"), "{notices:?}");
    }

    #[test]
    fn the_bundled_workflow_pack_is_found_and_loads() {
        let pack = bundled_pack().expect("the source tree has policy/agent-custom-workflow");
        let loaded = Pack::load(&pack).unwrap();
        assert!(loaded.roles.contains_key("root") && loaded.roles.contains_key("leaf"));
        assert!(!loaded.candidates.is_empty());
    }
}
