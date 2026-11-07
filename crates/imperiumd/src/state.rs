use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use imperium_config::{Paths, Settings};
use imperium_core::{ModelError, Session, SessionConfig, new_session_id, resolve_model};
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;
use tokio::sync::broadcast;

/// State shared by every connection: configuration and the running sessions.
#[derive(Debug)]
pub struct DaemonState {
    started: Instant,
    paths: Paths,
    settings: Settings,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
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
    pub fn new(paths: Paths, settings: Settings) -> Arc<Self> {
        Arc::new(DaemonState {
            started: Instant::now(),
            paths,
            settings,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn uptime_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Starts a session and returns it with a subscription that sees all of its events.
    pub fn create_session(
        &self,
        project_dir: &str,
        model: Option<&str>,
    ) -> Result<(Arc<Session>, broadcast::Receiver<SessionEvent>), CreateError> {
        let workspace = Workspace::new(Path::new(project_dir))
            .map_err(|error| CreateError::Project(project_dir.to_string(), error))?;
        let model = resolve_model(&self.settings, model, imperium_config::load_secret)?;
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

    pub fn session(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .expect("session map lock is never poisoned by a panic while held")
            .get(id)
            .cloned()
    }
}
