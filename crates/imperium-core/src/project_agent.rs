//! Project agents: one long-lived session per project with `agent = true`, woken only by
//! events (schedules, finished sessions, messages from the user) and dispatching work into new
//! sessions with `start_session`. Between events it is idle and uses no tokens. It runs only
//! while the daemon runs.
//!
//! Environment agents use the same machinery keyed by agent: an environment holds zero or more
//! persistent agents, each keeping one session that is always available, dispatching into the
//! same environment, resuming its conversation after restarts (its session id is kept with the
//! agent), and compacting its conversation when a turn starts over the model's budget. Agents
//! of one environment run independently, each with its own name, role and session limits.
//!
//! The sessions an agent starts are background sessions: they rank below the user's sessions
//! and pause while the machine is busy. With `prefer_quiet` (the default) a session is created
//! at once but gets its brief only when the machine is quiet, or after `max_defer`. The waiting
//! brief is kept with the session, so a restart resumes the wait instead of losing it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, NaiveDate};
use imperium_automation::parse_duration;
use imperium_config::ProjectSettings;
use imperium_engine::{
    LaunchFuture, Load, LoadView, Session, SessionLauncher, Waited, wait_for_quiet,
};
use imperium_protocol::agents::EnvironmentAgent;
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

const ENVIRONMENT_INSTRUCTIONS: &str = "You are one of this environment's persistent agents: a \
long-lived assistant for the folders of this environment. Your conversation continues across \
days and daemon restarts; older turns are summarised when it grows, so keep important facts, \
decisions, and an open-task list in memory (project-private) as well. Answer and act directly \
for small things; for larger work call start_session with a self-contained brief (it runs in \
this environment) and end your turn; its report arrives as a message.";

/// Limits on the sessions an environment agent may start.
const ENVIRONMENT_MAX_SESSIONS: usize = 2;
const ENVIRONMENT_MAX_PER_DAY: u32 = 20;

/// Where an agent's reports are delivered: whichever session the agent has now, so a report
/// from a session started before the agent restarted still reaches it.
#[derive(Debug, Default)]
struct AgentSlot(Mutex<Weak<Session>>);

impl AgentSlot {
    fn set(&self, session: &Arc<Session>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(session);
    }

    fn get(&self) -> Option<Arc<Session>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade()
    }
}

/// A running environment agent.
#[derive(Debug, Clone)]
struct EnvironmentAgentRun {
    environment_id: String,
    session: Arc<Session>,
}

/// The running project agents, by canonical project folder, and environment agents, by agent
/// id.
#[derive(Debug, Default)]
pub struct ProjectAgents {
    agents: Mutex<HashMap<PathBuf, Arc<Session>>>,
    environments: Mutex<HashMap<String, EnvironmentAgentRun>>,
    /// One lock per environment, held while its agents are synced, so two syncs never start the
    /// same agent twice.
    sync_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Each environment agent's launcher, by agent id. It outlives the agent's restarts, so the
    /// limits on the sessions it starts and the reports of sessions already running carry over.
    launchers: Mutex<HashMap<String, Arc<Launcher>>>,
}

impl ProjectAgents {
    fn lock_environments(&self) -> std::sync::MutexGuard<'_, HashMap<String, EnvironmentAgentRun>> {
        self.environments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn sync_lock(&self, environment_id: &str) -> Arc<Mutex<()>> {
        Arc::clone(
            self.sync_locks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(environment_id.to_string())
                .or_default(),
        )
    }

    fn launchers(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Launcher>>> {
        self.launchers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Stops tracking an environment agent (the caller forgets its session). Its launcher is
    /// kept: the agent starts again.
    pub fn forget_agent(&self, agent_id: &str) -> Option<Arc<Session>> {
        self.lock_environments()
            .remove(agent_id)
            .map(|run| run.session)
    }

    /// A persistent agent's session, if it is running.
    pub fn agent(&self, agent_id: &str) -> Option<Arc<Session>> {
        self.lock_environments()
            .get(agent_id)
            .map(|run| Arc::clone(&run.session))
    }

    /// The running agents of one environment: agent id and session, by agent id.
    pub fn agents_of(&self, environment_id: &str) -> Vec<(String, Arc<Session>)> {
        let mut agents: Vec<_> = self
            .lock_environments()
            .iter()
            .filter(|(_, run)| run.environment_id == environment_id)
            .map(|(id, run)| (id.clone(), Arc::clone(&run.session)))
            .collect();
        agents.sort_by(|a, b| a.0.cmp(&b.0));
        agents
    }

    /// Every running persistent agent: environment id, agent id and session id, sorted.
    pub fn environment_agents(&self) -> Vec<(String, String, String)> {
        let mut agents: Vec<_> = self
            .lock_environments()
            .iter()
            .map(|(id, run)| {
                (
                    run.environment_id.clone(),
                    id.clone(),
                    run.session.id().to_string(),
                )
            })
            .collect();
        agents.sort();
        agents
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Arc<Session>>> {
        self.agents.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every project and environment agent session.
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        let mut sessions: Vec<Arc<Session>> = self.lock().values().cloned().collect();
        sessions.extend(
            self.lock_environments()
                .values()
                .map(|run| Arc::clone(&run.session)),
        );
        sessions
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
            launcher.map(|launcher| Arc::clone(&launcher.agent)),
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
        agent: Arc::new(AgentSlot::default()),
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
                // Unattended: its approvals go to a queue, so it never starts in the user's
                // `permissions.mode` (plan would stall it, ask would flood the queue).
                permission_mode: Some(imperium_api::PermissionMode::Default),
                ..SessionOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
    launcher.agent.set(&session);
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

/// Starts the persistent agents of every environment.
pub fn start_environment_agents(daemon: &Arc<DaemonState>) {
    for env in daemon.environments().list() {
        if !env.agents.is_empty() {
            sync_environment_agent(daemon, &env.id);
        }
    }
}

/// Starts an environment's persistent agents that are not running, and stops the running ones
/// that were deleted (or whose environment is gone), archiving their sessions so their history
/// stays listed with the other archived sessions. A start that fails (no folders yet, no model)
/// is logged; the next change to the environment tries again.
///
/// One sync of an environment runs at a time, so agents added or changed from several places at
/// once are started exactly once.
pub fn sync_environment_agent(daemon: &Arc<DaemonState>, environment_id: &str) {
    let lock = daemon.project_agents().sync_lock(environment_id);
    let _sync = lock.lock().unwrap_or_else(PoisonError::into_inner);
    sync_locked(daemon, environment_id);
}

/// Stops an environment's running agents (all of them, or just `only`) and starts them again
/// with the environment as it is now, resuming their conversations. Their launchers stay, so
/// what they started goes on reporting to them and their limits are not reset. The whole
/// restart is one sync.
pub fn restart_environment_agents(
    daemon: &Arc<DaemonState>,
    environment_id: &str,
    only: Option<&str>,
) {
    let lock = daemon.project_agents().sync_lock(environment_id);
    let _sync = lock.lock().unwrap_or_else(PoisonError::into_inner);
    for (agent_id, _) in daemon.project_agents().agents_of(environment_id) {
        if only.is_some_and(|only| only != agent_id) {
            continue;
        }
        if let Some(session) = daemon.project_agents().forget_agent(&agent_id) {
            daemon.forget_session(session.id());
        }
    }
    sync_locked(daemon, environment_id);
}

fn sync_locked(daemon: &Arc<DaemonState>, environment_id: &str) {
    let env = daemon.environments().get(environment_id);
    let wanted: Vec<String> = env
        .as_ref()
        .map(|env| env.agents.iter().map(|agent| agent.id.clone()).collect())
        .unwrap_or_default();
    for (agent_id, session) in daemon.project_agents().agents_of(environment_id) {
        if wanted.contains(&agent_id) {
            continue;
        }
        daemon.project_agents().forget_agent(&agent_id);
        daemon.project_agents().launchers().remove(&agent_id);
        if let Err(error) = daemon.archive_retired_agent(session.id()) {
            tracing::warn!(environment = environment_id, agent = %agent_id, %error, "could not archive the environment agent");
            daemon.forget_session(session.id());
        }
        tracing::info!(environment = environment_id, agent = %agent_id, "environment agent stopped");
    }
    let Some(env) = env else { return };
    for agent in &env.agents {
        if daemon.project_agents().agent(&agent.id).is_some() {
            continue;
        }
        if let Err(error) = start_environment_agent(daemon, &env, agent) {
            tracing::warn!(environment = %env.name, agent = %agent.name, %error, "environment agent did not start");
        }
    }
}

/// The title an agent's session carries: `<environment> · <agent>`.
pub(crate) fn agent_title(environment_name: &str, agent_name: &str) -> String {
    format!("{environment_name} · {agent_name}")
}

/// Titles the running sessions of an environment's agents after their names (after an agent or
/// the environment was renamed).
pub fn retitle_environment_agents(daemon: &Arc<DaemonState>, environment_id: &str) {
    let Some(env) = daemon.environments().get(environment_id) else {
        return;
    };
    for agent in &env.agents {
        if let Some(session) = daemon.project_agents().agent(&agent.id) {
            let _ = daemon.set_title(session.id(), &agent_title(&env.name, &agent.name), true);
        }
    }
}

/// A memory entry prefix for an agent: its name in lower case with dashes, since the
/// project-private memory is the environment's, shared by every one of its agents.
fn memory_prefix(name: &str) -> String {
    let mut prefix = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            prefix.extend(c.to_lowercase());
        } else if !prefix.ends_with('-') && !prefix.is_empty() {
            prefix.push('-');
        }
    }
    let prefix = prefix.trim_end_matches('-').to_string();
    if prefix.is_empty() {
        "agent".to_string()
    } else {
        prefix
    }
}

/// An agent's instructions: the shared ones, then who it is, how it keeps its notes apart from
/// the environment's other agents, and what it is for.
fn agent_instructions(agent: &EnvironmentAgent) -> String {
    let prefix = memory_prefix(&agent.name);
    let mut text = format!(
        "{ENVIRONMENT_INSTRUCTIONS}\n\nYour name is {}. The project-private memory is shared by \
         every agent of this environment: begin the name of each entry you save with \"{prefix}-\" \
         and keep your open-task list in \"{prefix}-open-tasks\", so you do not overwrite another \
         agent's notes.",
        agent.name
    );
    if !agent.role.is_empty() {
        text.push_str(&format!(" Your role:\n{}", agent.role));
    }
    text
}

fn new_environment_launcher(daemon: &Arc<DaemonState>, environment_id: &str) -> Arc<Launcher> {
    Arc::new(Launcher {
        daemon: Arc::downgrade(daemon),
        // An environment has no settings for deferring: its agent's sessions wait for a quiet
        // machine, up to the default deferral.
        prefer_quiet: true,
        max_defer: DEFAULT_MAX_DEFER,
        target: Target::Environment(environment_id.to_string()),
        agent: Arc::new(AgentSlot::default()),
        active: Arc::new(AtomicUsize::new(0)),
        today: Mutex::new((Local::now().date_naive(), 0)),
        max_sessions: ENVIRONMENT_MAX_SESSIONS,
        max_per_day: ENVIRONMENT_MAX_PER_DAY,
    })
}

fn start_environment_agent(
    daemon: &Arc<DaemonState>,
    env: &imperium_protocol::methods::EnvironmentInfo,
    agent: &EnvironmentAgent,
) -> Result<(), String> {
    // The agent's launcher from before a restart, with its counts of sessions running and
    // started today, and the reports still to come from sessions it started.
    let launcher = Arc::clone(
        daemon
            .project_agents()
            .launchers()
            .entry(agent.id.clone())
            .or_insert_with(|| new_environment_launcher(daemon, &env.id)),
    );
    // The same session as before when its journal is still there and it is not running as an
    // ordinary session (unarchived by hand); archived when the agent was stopped, it comes
    // back out of the archive.
    let resume_id = agent.session_id.clone().filter(|id| {
        daemon
            .data_dir()
            .join("sessions")
            .join(format!("{id}.jsonl"))
            .is_file()
            && daemon.session(id).is_none()
    });
    let resumed = resume_id.is_some();
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
                instructions: Some(agent_instructions(agent)),
                persistent: true,
                auto_compact: true,
                ..SessionOptions::default()
            },
        )
        .map_err(|error| error.to_string())?;
    launcher.agent.set(&session);
    if !resumed {
        // A new agent is listed under its name until its first exchange is worth titling.
        let _ = daemon.set_title(session.id(), &agent_title(&env.name, &agent.name), true);
    }
    if agent.session_id.as_deref() != Some(session.id()) {
        if let Err(error) = daemon.environments().set_agent_session(
            &env.id,
            &agent.id,
            Some(session.id().to_string()),
        ) {
            // The agent was deleted while it started: nothing records this session, so it must
            // not keep running. One that had a history is archived with it; a new one is dropped.
            daemon.project_agents().launchers().remove(&agent.id);
            if !resumed || daemon.archive_retired_agent(session.id()).is_err() {
                daemon.forget_session(session.id());
            }
            return Err(error.to_string());
        }
        daemon.announce_environments();
    }
    daemon.project_agents().lock_environments().insert(
        agent.id.clone(),
        EnvironmentAgentRun {
            environment_id: env.id.clone(),
            session,
        },
    );
    tracing::info!(environment = %env.name, agent = %agent.name, "environment agent started");
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
    agent: Arc<AgentSlot>,
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
                Some(Arc::clone(&self.agent)),
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
    agent: Option<Arc<AgentSlot>>,
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
        if let Some(agent) = agent.and_then(|slot| slot.get()) {
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
