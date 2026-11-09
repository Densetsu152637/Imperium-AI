//! Project agents: one long-lived session per project with `agent = true`, woken only by
//! events (schedules, finished sessions, messages from the user) and dispatching work into new
//! sessions with `start_session`. Between events it is idle and uses no tokens. It runs only
//! while the daemon runs.
//!
//! Environment agents use the same machinery keyed by environment: an environment with
//! `persistent_agent` on keeps one session that is always available, dispatches into the same
//! environment, resumes its conversation after restarts (its id is kept with the environment),
//! and compacts its conversation when a turn starts over the model's budget.

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

const ENVIRONMENT_INSTRUCTIONS: &str = "You are this environment's persistent agent: the user's \
long-lived assistant for the folders of this environment. Your conversation continues across \
days and daemon restarts; older turns are summarised when it grows, so keep important facts, \
decisions, and an open-task list in memory (project-private) as well. Answer and act directly \
for small things; for larger work call start_session with a self-contained brief (it runs in \
this environment) and end your turn; its report arrives as a message.";

/// Limits on the sessions an environment agent may start.
const ENVIRONMENT_MAX_SESSIONS: usize = 2;
const ENVIRONMENT_MAX_PER_DAY: u32 = 20;

/// The running project agents, by canonical project folder, and environment agents, by
/// environment id.
#[derive(Debug, Default)]
pub struct ProjectAgents {
    agents: Mutex<HashMap<PathBuf, Arc<Session>>>,
    environments: Mutex<HashMap<String, Arc<Session>>>,
}

impl ProjectAgents {
    fn lock_environments(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.environments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Stops tracking an environment's agent (the caller forgets its session).
    pub fn forget_environment(&self, environment_id: &str) -> Option<Arc<Session>> {
        self.lock_environments().remove(environment_id)
    }

    /// An environment's persistent agent session, if it is running.
    pub fn environment(&self, environment_id: &str) -> Option<Arc<Session>> {
        self.lock_environments().get(environment_id).cloned()
    }

    /// Environments with a running persistent agent and their sessions' ids, by environment id.
    pub fn environments(&self) -> Vec<(String, String)> {
        let mut agents: Vec<_> = self
            .lock_environments()
            .iter()
            .map(|(env, session)| (env.clone(), session.id().to_string()))
            .collect();
        agents.sort();
        agents
    }

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
        target: Target::Folder(path.clone()),
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

/// Starts the persistent agent of every environment that has one turned on.
pub fn start_environment_agents(daemon: &Arc<DaemonState>) {
    for env in daemon.environments().list() {
        if env.persistent_agent {
            sync_environment_agent(daemon, &env.id);
        }
    }
}

/// Starts an environment's persistent agent when it is on and not running, and stops it when
/// it is off (or the environment is gone). A start that fails (no folders yet, no model) is
/// logged; the next change to the environment tries again.
pub fn sync_environment_agent(daemon: &Arc<DaemonState>, environment_id: &str) {
    let env = daemon.environments().get(environment_id);
    let wanted = env.as_ref().is_some_and(|env| env.persistent_agent);
    let running = daemon.project_agents().environment(environment_id);
    match (wanted, running) {
        (true, None) => {
            if let Some(env) = env
                && let Err(error) = start_environment_agent(daemon, &env)
            {
                tracing::warn!(environment = %env.name, %error, "environment agent did not start");
            }
        }
        (false, Some(session)) => {
            daemon.project_agents().forget_environment(environment_id);
            daemon.forget_session(session.id());
            tracing::info!(environment = environment_id, "environment agent stopped");
        }
        _ => {}
    }
}

fn start_environment_agent(
    daemon: &Arc<DaemonState>,
    env: &imperium_protocol::methods::EnvironmentInfo,
) -> Result<(), String> {
    let launcher = Arc::new(Launcher {
        daemon: Arc::downgrade(daemon),
        target: Target::Environment(env.id.clone()),
        agent: OnceLock::new(),
        active: Arc::new(AtomicUsize::new(0)),
        today: Mutex::new((Local::now().date_naive(), 0)),
        max_sessions: ENVIRONMENT_MAX_SESSIONS,
        max_per_day: ENVIRONMENT_MAX_PER_DAY,
    });
    // The same session as before when its journal is still there.
    let resume_id = env.agent_session_id.clone().filter(|id| {
        daemon
            .data_dir()
            .join("sessions")
            .join(format!("{id}.jsonl"))
            .is_file()
    });
    let model = resume_id.as_ref().and_then(|id| {
        imperium_engine::journalled_model(
            &daemon
                .data_dir()
                .join("sessions")
                .join(format!("{id}.jsonl")),
        )
    });
    let (session, _events) = daemon
        .create_in_environment(
            &env.id,
            model.as_deref(),
            SessionOptions {
                resume_id,
                launcher: Some(Arc::clone(&launcher) as Arc<dyn SessionLauncher>),
                instructions: Some(ENVIRONMENT_INSTRUCTIONS.into()),
                persistent: true,
                auto_compact: true,
                ..SessionOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
    let _ = launcher.agent.set(Arc::downgrade(&session));
    if env.agent_session_id.as_deref() != Some(session.id()) {
        daemon
            .environments()
            .set_agent_session(&env.id, Some(session.id().to_string()))
            .map_err(|error| error.to_string())?;
        daemon.announce_environments();
    }
    daemon
        .project_agents()
        .lock_environments()
        .insert(env.id.clone(), session);
    tracing::info!(environment = %env.name, "environment agent started");
    Ok(())
}

/// Where a launcher's sessions work.
#[derive(Debug)]
enum Target {
    /// A project folder (a `[[projects]]` agent).
    Folder(String),
    /// An environment (its persistent agent).
    Environment(String),
}

/// Starts sessions for one project's (or environment's) agent and reports back to it.
#[derive(Debug)]
struct Launcher {
    daemon: Weak<DaemonState>,
    target: Target,
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
            let created = match &self.target {
                Target::Folder(project) => {
                    daemon.create_session_with(project, model, SessionOptions::default())
                }
                Target::Environment(env) => {
                    daemon.create_in_environment(env, model, SessionOptions::default())
                }
            };
            let (session, events) = created.map_err(|error| error.to_string())?;
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
