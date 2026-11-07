use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use imperium_config::{Paths, Settings};
use imperium_core::{
    CatalogModel, LocalModels, ModelError, Session, SessionConfig, SessionMemory, new_session_id,
    resolve_model,
};
use imperium_extensions::{ConfiguredHook, Extensions, Sources};
use imperium_memory::Store;
use imperium_protocol::methods::{LocalModelListResult, ModelListResult, ModelRow};
use imperium_protocol::session::AgentEvent;
use imperium_tools::Workspace;
use imperium_workflow::{ContextIndex, Pack};
use tokio::sync::broadcast;

use crate::local::LocalRuntime;

/// State shared by every connection: configuration and the running sessions.
#[derive(Debug)]
pub struct DaemonState {
    started: Instant,
    paths: Paths,
    settings: Settings,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    local: Option<LocalRuntime>,
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
        Arc::new(DaemonState {
            started: Instant::now(),
            paths,
            settings,
            sessions: Mutex::new(HashMap::new()),
            local,
        })
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
        let workspace = Workspace::new(Path::new(project_dir))
            .map_err(|error| CreateError::Project(project_dir.to_string(), error))?;
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
            memory: self.settings.memory.enabled.then(|| SessionMemory {
                store: Store::new(self.paths.data_dir(), Some(Path::new(project_dir))),
                settings: self.settings.memory.clone(),
            }),
            max_steps: self.settings.agents.max_steps,
        })
        .map_err(|error| CreateError::Start(error.to_string()))?;
        let events = session.subscribe();
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
