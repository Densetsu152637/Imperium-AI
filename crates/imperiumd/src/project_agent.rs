//! Project agents: one long-lived session per project with `agent = true`, woken only by
//! events (schedules, finished sessions, messages from the user) and dispatching work into new
//! sessions with `start_session`. Between events it is idle and uses no tokens. It runs only
//! while the daemon runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use chrono::{Local, NaiveDate};
use imperium_config::ProjectSettings;
use imperium_engine::{LaunchFuture, Session, SessionLauncher};
use imperium_protocol::session::SessionEvent;
use tokio::sync::broadcast;

use crate::{DaemonState, SessionOptions};

/// Longest a dispatched session may run before its report is given up on.
const SESSION_TIMEOUT: Duration = Duration::from_secs(3600);

const INSTRUCTIONS: &str = "You are this project's agent: a long-lived coordinator. You are \
woken by events: schedules firing, sessions you started finishing (their report arrives as a \
message), and messages from the user. For each event decide what to do. Answer or act \
directly only for small things; for real work call start_session with a self-contained brief, \
then end your turn: you use no tokens while sessions run. When a report arrives, check it and \
decide whether anything follows. Keep a short backlog in memory (project-private) so you can \
resume after a restart. Start only the sessions that are needed.";

/// The running project agents, by canonical project folder.
#[derive(Debug, Default)]
pub struct ProjectAgents {
    agents: Mutex<HashMap<PathBuf, Arc<Session>>>,
}

impl ProjectAgents {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Arc<Session>>> {
        self.agents.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The agent session for a project folder, if it has one.
    pub fn get(&self, project: &Path) -> Option<Arc<Session>> {
        self.lock().get(&canonical(project)).cloned()
    }

    /// Projects with a running agent and their agent sessions.
    pub fn list(&self) -> Vec<(PathBuf, String)> {
        let mut agents: Vec<_> = self
            .lock()
            .iter()
            .map(|(path, session)| (path.clone(), session.id().to_string()))
            .collect();
        agents.sort();
        agents
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Starts an agent for every `[[projects]]` entry with `agent = true`.
pub fn start(daemon: &Arc<DaemonState>) {
    for project in daemon.settings().projects.clone() {
        if !project.agent {
            continue;
        }
        if let Err(error) = start_one(daemon, &project) {
            tracing::warn!(project = %project.path.display(), %error, "project agent did not start");
        }
    }
}

fn start_one(daemon: &Arc<DaemonState>, project: &ProjectSettings) -> Result<(), String> {
    let path = project.path.display().to_string();
    let launcher = Arc::new(Launcher {
        daemon: Arc::downgrade(daemon),
        project: path.clone(),
        agent: OnceLock::new(),
        active: Arc::new(AtomicUsize::new(0)),
        today: Mutex::new((Local::now().date_naive(), 0)),
        max_sessions: project.max_sessions,
        max_per_day: project.max_sessions_per_day,
    });
    let (session, events) = daemon
        .create_session_with(
            &path,
            project.agent_model.as_deref(),
            SessionOptions {
                launcher: Some(Arc::clone(&launcher) as Arc<dyn SessionLauncher>),
                instructions: Some(INSTRUCTIONS.into()),
                ..SessionOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
    let _ = launcher.agent.set(Arc::downgrade(&session));
    daemon
        .project_agents()
        .lock()
        .insert(canonical(&project.path), Arc::clone(&session));
    tokio::spawn(answer_permissions(Arc::downgrade(daemon), session, events));
    tracing::info!(project = %path, "project agent started");
    Ok(())
}

/// The agent's own permission requests go to the approval queue: nobody may be watching.
async fn answer_permissions(
    daemon: Weak<DaemonState>,
    session: Arc<Session>,
    mut events: broadcast::Receiver<imperium_protocol::session::AgentEvent>,
) {
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if let SessionEvent::PermissionRequested {
            request_id,
            tool,
            summary,
        } = event.event
        {
            let Some(daemon) = daemon.upgrade() else {
                return;
            };
            daemon.bridge_permission(&session, request_id, &tool, summary);
        }
    }
}

/// Starts sessions for one project's agent and reports back to it.
#[derive(Debug)]
struct Launcher {
    daemon: Weak<DaemonState>,
    project: String,
    agent: OnceLock<Weak<Session>>,
    active: Arc<AtomicUsize>,
    /// Sessions started today.
    today: Mutex<(NaiveDate, u32)>,
    max_sessions: usize,
    max_per_day: u32,
}

impl Launcher {
    fn reserve(&self) -> Result<(), String> {
        if self.active.load(Ordering::SeqCst) >= self.max_sessions {
            return Err(format!(
                "{} sessions are already running (max_sessions); wait for a report",
                self.max_sessions
            ));
        }
        let mut today = self.today.lock().unwrap_or_else(PoisonError::into_inner);
        let date = Local::now().date_naive();
        if today.0 != date {
            *today = (date, 0);
        }
        if today.1 >= self.max_per_day {
            return Err(format!(
                "the daily limit of {} sessions is reached (max_sessions_per_day)",
                self.max_per_day
            ));
        }
        today.1 += 1;
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl SessionLauncher for Launcher {
    fn launch<'a>(&'a self, brief: &'a str, model: Option<&'a str>) -> LaunchFuture<'a> {
        Box::pin(async move {
            let daemon = self.daemon.upgrade().ok_or("the daemon is stopping")?;
            self.reserve()?;
            let release = Release(Arc::clone(&self.active));
            let (session, events) = daemon
                .create_session_with(&self.project, model, SessionOptions::default())
                .map_err(|error| error.to_string())?;
            let id = session.id().to_string();
            session
                .send(brief.to_string())
                .map_err(|_| "the session closed before it started".to_string())?;
            // The agent is not waiting: the report arrives as a message when the session ends.
            let agent = self.agent.get().cloned();
            let report_id = id.clone();
            tokio::spawn(async move {
                let result = daemon
                    .wait_for_turn(&session, events, SESSION_TIMEOUT)
                    .await;
                drop(release);
                let message = match result {
                    Ok(report) => format!(
                        "Session {report_id} finished. Its report:

{report}"
                    ),
                    Err(error) => format!("Session {report_id} did not finish: {error}"),
                };
                if let Some(agent) = agent.and_then(|agent| agent.upgrade()) {
                    let _ = agent.send(message);
                }
            });
            Ok(id)
        })
    }
}

/// Frees a running-session slot when dropped, however the session ends.
struct Release(Arc<AtomicUsize>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
