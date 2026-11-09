//! Project agents: one long-lived session per project with `agent = true`, woken only by
//! events (schedules, finished sessions, messages from the user) and dispatching work into new
//! sessions with `start_session`. Between events it is idle and uses no tokens. It runs only
//! while the daemon runs.
//!
//! Environment agents use the same machinery keyed by environment: an environment with
//! `persistent_agent` on keeps one session that is always available, dispatches into the same
//! environment, resumes its conversation after restarts (its id is kept with the environment),
//! and compacts its conversation when a turn starts over the model's budget.
//!
//! The sessions an agent starts are background sessions: they rank below the user's sessions
//! and pause while the machine is busy. With `prefer_quiet` (the default) a session is created
//! at once but gets its brief only when the machine is quiet, or after `max_defer`. The waiting
//! brief is kept with the session, so a restart resumes the wait instead of losing it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, NaiveDate};
use imperium_automation::parse_duration;
use imperium_config::ProjectSettings;
use imperium_engine::{
    LaunchFuture, Load, LoadView, Session, SessionLauncher, Waited, wait_for_quiet,
};
use imperium_protocol::session::{AgentEvent, SessionEvent};
use tokio::sync::broadcast;

use crate::scheduler::DEFAULT_MAX_DEFER;
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

/// A dispatched session's brief that was waiting for a quiet machine when the daemon stopped
/// (restored by [`DaemonState::resume_sessions`]).
#[derive(Debug)]
pub(crate) struct PendingBrief {
    pub project: String,
    pub session: Arc<Session>,
    pub events: broadcast::Receiver<AgentEvent>,
    pub brief: String,
    /// When it gets its brief whatever the load.
    pub until_unix_ms: u64,
}

/// Starts an agent for every `[[projects]]` entry with `agent = true`, then sends the briefs that
/// were waiting before a restart.
pub fn start(daemon: &Arc<DaemonState>) {
    let mut launchers = HashMap::new();
    for project in daemon.settings().projects.clone() {
        if !project.agent {
            continue;
        }
        match start_one(daemon, &project) {
            Ok(launcher) => {
                launchers.insert(canonical(&project.path), launcher);
            }
            Err(error) => {
                tracing::warn!(project = %project.path.display(), %error, "project agent did not start");
            }
        }
    }
    for pending in daemon.take_pending_briefs() {
        let launcher = launchers.get(&canonical(Path::new(&pending.project)));
        if launcher.is_none() {
            tracing::warn!(
                session = %pending.session.id(),
                "the project no longer has an agent; the waiting brief is sent with nobody to report to"
            );
        }
        let wait = Duration::from_millis(pending.until_unix_ms.saturating_sub(unix_ms()));
        dispatch(
            Arc::clone(daemon),
            pending.session,
            pending.events,
            pending.brief,
            launcher
                .map(|launcher| launcher.prefer_quiet)
                .unwrap_or(true),
            wait,
            launcher.and_then(|launcher| launcher.agent.get().cloned()),
            launcher.map(|launcher| launcher.claim()),
        );
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

fn start_one(
    daemon: &Arc<DaemonState>,
    project: &ProjectSettings,
) -> Result<Arc<Launcher>, String> {
    let path = project.path.display().to_string();
    let max_defer = match project.max_defer.as_deref().map(parse_duration) {
        Some(Ok(length)) => length,
        Some(Err(error)) => {
            tracing::warn!(project = %path, %error, "max_defer is not a length of time; using 2 h");
            DEFAULT_MAX_DEFER
        }
        None => DEFAULT_MAX_DEFER,
    };
    let launcher = Arc::new(Launcher {
        prefer_quiet: project.prefer_quiet,
        max_defer,
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
    Ok(launcher)
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
            ..
        } = event.event
        {
            let Some(daemon) = daemon.upgrade() else {
                return;
            };
            daemon.bridge_permission(&session, request_id, &tool, summary);
        } else if let SessionEvent::QuestionsAsked {
            request_id,
            questions,
        } = event.event
        {
            crate::state::answer_with_recommendations(&session, request_id, &questions);
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
/// it is off (or the environment is gone), archiving its session so its history stays listed
/// with the other archived sessions. A start that fails (no folders yet, no model) is logged;
/// the next change to the environment tries again.
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
            if let Err(error) = daemon.archive_retired_agent(session.id()) {
                tracing::warn!(environment = environment_id, %error, "could not archive the environment agent");
                daemon.forget_session(session.id());
            }
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
        // An environment has no settings for deferring: its agent's sessions wait for a quiet
        // machine, up to the default deferral.
        prefer_quiet: true,
        max_defer: DEFAULT_MAX_DEFER,
        target: Target::Environment(env.id.clone()),
        agent: OnceLock::new(),
        active: Arc::new(AtomicUsize::new(0)),
        today: Mutex::new((Local::now().date_naive(), 0)),
        max_sessions: ENVIRONMENT_MAX_SESSIONS,
        max_per_day: ENVIRONMENT_MAX_PER_DAY,
    });
    // The same session as before when its journal is still there and it is not running as an
    // ordinary session (unarchived by hand); archived when the agent was turned off, it comes
    // back out of the archive.
    let resume_id = env.agent_session_id.clone().filter(|id| {
        daemon
            .data_dir()
            .join("sessions")
            .join(format!("{id}.jsonl"))
            .is_file()
            && daemon.session(id).is_none()
    });
    if let Some(id) = &resume_id {
        daemon.reclaim_archived(id);
    }
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
    /// Hold a session back until the machine is quiet, for at most `max_defer`.
    prefer_quiet: bool,
    max_defer: Duration,
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

    /// Counts a session restored after a restart as running, whatever the limits: it was
    /// already allowed to start.
    fn claim(&self) -> Release {
        self.active.fetch_add(1, Ordering::SeqCst);
        Release(Arc::clone(&self.active))
    }
}

impl SessionLauncher for Launcher {
    fn launch<'a>(&'a self, brief: &'a str, model: Option<&'a str>) -> LaunchFuture<'a> {
        Box::pin(async move {
            let daemon = self.daemon.upgrade().ok_or("the daemon is stopping")?;
            self.reserve()?;
            let release = Release(Arc::clone(&self.active));
            // What it starts is background work: it ranks below the user's sessions.
            let options = SessionOptions {
                background: true,
                ..SessionOptions::default()
            };
            let created = match &self.target {
                Target::Folder(project) => daemon.create_session_with(project, model, options),
                Target::Environment(env) => daemon.create_in_environment(env, model, options),
            };
            let (session, events) = created.map_err(|error| error.to_string())?;
            let id = session.id().to_string();
            dispatch(
                daemon,
                session,
                events,
                brief.to_string(),
                self.prefer_quiet,
                self.max_defer,
                self.agent.get().cloned(),
                Some(release),
            );
            Ok(id)
        })
    }
}

/// Gives a dispatched session its brief, once the machine is quiet if `prefer_quiet` (for at
/// most `max_defer`), and reports the outcome to the agent, if there is one. The agent is not
/// waiting: the report arrives as a message when the session ends.
#[allow(clippy::too_many_arguments)]
fn dispatch(
    daemon: Arc<DaemonState>,
    session: Arc<Session>,
    events: broadcast::Receiver<AgentEvent>,
    brief: String,
    prefer_quiet: bool,
    max_defer: Duration,
    agent: Option<Weak<Session>>,
    release: Option<Release>,
) {
    let id = session.id().to_string();
    // The session exists at once, but while the machine is busy it waits to start.
    let view: Arc<dyn LoadView> = daemon.load_view();
    let waiting = prefer_quiet && view.level() != Load::Quiet && !session.run_now().released();
    if waiting {
        session.defer(format!(
            "waiting for a quiet machine (it is {}; starts anyway after {} min)",
            view.level().label(),
            max_defer.as_secs().div_ceil(60)
        ));
        let until =
            unix_ms().saturating_add(u64::try_from(max_defer.as_millis()).unwrap_or(u64::MAX));
        daemon.set_deferred_brief(&id, Some((&brief, until)));
    }
    tokio::spawn(async move {
        if waiting {
            // After `max_defer`, or when a person runs it now, it runs whatever the load, and
            // does not give way again.
            if wait_for_quiet(&*view, session.run_now(), Some(max_defer)).await != Waited::Quiet {
                daemon.release_run_now(&session);
            }
            session.undefer();
        }
        let result = if let Err(stop) = crate::budgets::check(&daemon, &id, false) {
            Err(stop.message)
        } else if session.send(brief).is_ok() {
            daemon.set_deferred_brief(&id, None);
            daemon
                .wait_for_turn(&session, events, SESSION_TIMEOUT)
                .await
        } else {
            Err("the session closed before it started".to_string())
        };
        drop(release);
        let message = match result {
            Ok(report) => format!(
                "Session {id} finished. Its report:

{report}"
            ),
            Err(error) => format!("Session {id} did not finish: {error}"),
        };
        if let Some(agent) = agent.and_then(|agent| agent.upgrade()) {
            let _ = agent.send(message);
        }
    });
}

/// Frees a running-session slot when dropped, however the session ends.
struct Release(Arc<AtomicUsize>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
