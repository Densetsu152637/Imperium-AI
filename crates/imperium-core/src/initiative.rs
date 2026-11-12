//! Initiative: a persistent agent looks for work on its own (see
//! docs/14-projects-and-automation.md#initiative-implemented).
//!
//! Each persistent agent of an environment sets `off` (the default), `suggest` or `act`. Every
//! `initiative.review_every_min` the daemon checks whether a review is due for each agent that
//! has initiative. A review is the daemon sending the agent a message built from a template
//! ([`prompt`]) with facts only the daemon knows; the agent reads the environment's documents
//! itself and answers with tools: `suggest_work` and `work_digest` ([`tools`]) in `suggest`, and
//! `start_session` as well in `act`. It runs only while the machine is quiet and the user has not
//! been active in the environment, so it never competes with the user.
//!
//! What an agent suggests is kept in `initiative.json` in the data folder, per agent, with the
//! sessions it started (for the daily limit). A suggestion becomes a session only when the user
//! starts it (`initiative.start`), or, in `act` mode, when the agent calls `start_session` during
//! a review, within `initiative.max_sessions_per_day` and `initiative.max_concurrent` (counted per
//! agent). Those sessions are tagged with who started them and why ([`StartedBy`]).

mod prompt;
mod tools;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use imperium_engine::Load;
use imperium_protocol::agents::EnvironmentAgent;
use imperium_protocol::initiative::{
    DigestItem, INITIATIVE_CHANGED, INITIATIVE_NOTICE, Initiative, InitiativeNotice, InitiativeRef,
    InitiativeStartResult, InitiativeState, InitiativeSuggestionRef, NoticeKind, StartedBy,
    SuggestionStatus, WorkDigest, WorkSuggestion,
};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::EnvironmentInfo;
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent, SessionStatus};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub(crate) use tools::InitiativeTools;

use crate::{DaemonState, SessionOptions};

const DAY_MS: u64 = 86_400_000;
/// Suggestions waiting for the user, and handled ones kept to avoid repeating them.
const KEEP_OPEN: usize = 12;
const KEEP_CLOSED: usize = 20;
/// A handled suggestion is not suggested again for this long.
const REPEAT_AFTER_MS: u64 = 7 * DAY_MS;

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What is kept for one agent.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct AgentBook {
    #[serde(default)]
    suggestions: Vec<WorkSuggestion>,
    #[serde(default)]
    last_review_at: Option<u64>,
    #[serde(default)]
    last_digest_at: Option<u64>,
    #[serde(default)]
    digest: Option<WorkDigest>,
    #[serde(default)]
    last_error: Option<String>,
    /// Sessions the agent started on its own initiative, for the daily limit.
    #[serde(default)]
    started: Vec<Started>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Started {
    at: u64,
    session_id: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    agents: BTreeMap<String, AgentBook>,
}

/// A review in progress: what the agent may do in it.
#[derive(Debug, Clone)]
pub(crate) struct Review {
    /// `suggest` or `act`: `act` only while the limits leave room.
    pub mode: Initiative,
    pub max_suggestions: u32,
    pub added: u32,
    pub digest_given: bool,
}

#[derive(Debug, Default)]
struct Live {
    reviews: HashMap<String, Review>,
    /// Agents that are working on the report of a session it started on its own
    /// initiative: it follows the same limits as a review.
    follow_ups: HashSet<String>,
    running: HashMap<String, Arc<AtomicUsize>>,
    /// Sessions started on the agent's own initiative, until the daemon stops.
    sessions: HashSet<String>,
    next_pending: u64,
}

#[derive(Debug)]
struct Inner {
    path: Option<PathBuf>,
    book: Mutex<File>,
    live: Mutex<Live>,
}

/// The suggestions, digests and started sessions of every persistent agent, and what is running.
#[derive(Debug, Clone)]
pub struct InitiativeBook {
    inner: Arc<Inner>,
}

/// Frees a running-session slot when dropped.
#[derive(Debug)]
pub(crate) struct RunningGuard(Arc<AtomicUsize>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Permission to start one session on the agent's own initiative; dropped without
/// [`Ticket::commit`], it gives the slot back.
#[derive(Debug)]
pub(crate) struct Ticket {
    inner: Arc<Inner>,
    agent_id: String,
    pending: String,
    counter: Arc<AtomicUsize>,
    committed: bool,
}

impl Ticket {
    /// Records the session that used the permission.
    pub(crate) fn commit(mut self, session_id: &str) -> RunningGuard {
        {
            let mut book = lock(&self.inner.book);
            if let Some(started) = book.agents.get_mut(&self.agent_id).and_then(|env| {
                env.started
                    .iter_mut()
                    .find(|s| s.session_id == self.pending)
            }) {
                started.session_id = session_id.to_string();
            }
            InitiativeBook::save(&self.inner, &book);
        }
        lock(&self.inner.live)
            .sessions
            .insert(session_id.to_string());
        self.committed = true;
        RunningGuard(Arc::clone(&self.counter))
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        {
            let mut book = lock(&self.inner.book);
            if let Some(env) = book.agents.get_mut(&self.agent_id) {
                env.started.retain(|s| s.session_id != self.pending);
            }
            InitiativeBook::save(&self.inner, &book);
        }
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

fn local_day(at_ms: u64) -> Option<chrono::NaiveDate> {
    i64::try_from(at_ms)
        .ok()
        .and_then(|ms| Local.timestamp_millis_opt(ms).single())
        .map(|time| time.date_naive())
}

fn normal(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

impl InitiativeBook {
    /// Reads `path`; a missing or unreadable file starts empty.
    pub fn load(path: PathBuf) -> Self {
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "the initiative file is unreadable; starting a new one");
                File::default()
            }),
            Err(_) => File::default(),
        };
        InitiativeBook {
            inner: Arc::new(Inner {
                path: Some(path),
                book: Mutex::new(file),
                live: Mutex::new(Live::default()),
            }),
        }
    }

    fn save(inner: &Inner, file: &File) {
        let Some(path) = &inner.path else { return };
        let written = serde_json::to_string_pretty(file)
            .map_err(std::io::Error::other)
            .and_then(|text| std::fs::write(path, text));
        if let Err(error) = written {
            tracing::warn!(path = %path.display(), %error, "could not save the initiative file");
        }
    }

    fn change<R>(&self, agent_id: &str, change: impl FnOnce(&mut AgentBook) -> R) -> R {
        let mut book = lock(&self.inner.book);
        let result = change(book.agents.entry(agent_id.to_string()).or_default());
        Self::save(&self.inner, &book);
        result
    }

    fn read<R>(&self, agent_id: &str, read: impl FnOnce(&AgentBook) -> R) -> R {
        let book = lock(&self.inner.book);
        match book.agents.get(agent_id) {
            Some(env) => read(env),
            None => read(&AgentBook::default()),
        }
    }

    /// Forgets an agent that was deleted.
    pub(crate) fn forget_agent(&self, agent_id: &str) {
        let mut book = lock(&self.inner.book);
        if book.agents.remove(agent_id).is_some() {
            Self::save(&self.inner, &book);
        }
        let mut live = lock(&self.inner.live);
        live.reviews.remove(agent_id);
        live.follow_ups.remove(agent_id);
    }

    // --- reviews -----------------------------------------------------------------------

    /// Starts a review; false when one is already running.
    pub(crate) fn begin_review(&self, agent_id: &str, review: Review) -> bool {
        let mut live = lock(&self.inner.live);
        if live.reviews.contains_key(agent_id) {
            return false;
        }
        live.reviews.insert(agent_id.to_string(), review);
        true
    }

    pub(crate) fn end_review(&self, agent_id: &str) -> Option<Review> {
        lock(&self.inner.live).reviews.remove(agent_id)
    }

    pub(crate) fn review(&self, agent_id: &str) -> Option<Review> {
        lock(&self.inner.live).reviews.get(agent_id).cloned()
    }

    pub(crate) fn is_reviewing(&self, agent_id: &str) -> bool {
        lock(&self.inner.live).reviews.contains_key(agent_id)
    }

    fn set_follow_up(&self, agent_id: &str, on: bool) {
        let mut live = lock(&self.inner.live);
        if on {
            live.follow_ups.insert(agent_id.to_string());
        } else {
            live.follow_ups.remove(agent_id);
        }
    }

    /// Whether the agent's current turn is a review, or the follow-up to a session it started
    /// on its own: what it starts then follows the initiative limits.
    pub(crate) fn initiative_turn(&self, agent_id: &str) -> bool {
        let live = lock(&self.inner.live);
        live.reviews.contains_key(agent_id) || live.follow_ups.contains(agent_id)
    }

    /// Whether `text` is the report of a session the agent started on its own initiative.
    fn is_report_of_initiative_session(&self, text: &str) -> bool {
        let Some(rest) = text.strip_prefix("Session ") else {
            return false;
        };
        let id = rest.split_whitespace().next().unwrap_or_default();
        lock(&self.inner.live).sessions.contains(id)
    }

    pub(crate) fn finish_review(
        &self,
        agent_id: &str,
        at: u64,
        digest_attempted: bool,
        error: Option<String>,
    ) {
        self.change(agent_id, |env| {
            env.last_review_at = Some(at);
            if digest_attempted {
                env.last_digest_at = Some(at);
            }
            env.last_error = error;
        });
    }

    // --- suggestions and digests ------------------------------------------------------

    /// Adds a suggestion during a review. Refused outside one, when the review has made its
    /// share, or when the same work was suggested lately.
    pub(crate) fn add_suggestion(
        &self,
        agent_id: &str,
        title: String,
        prompt: String,
        reason: String,
        source: String,
    ) -> Result<WorkSuggestion, String> {
        {
            let mut live = lock(&self.inner.live);
            let review = live.reviews.get_mut(agent_id).ok_or_else(|| {
                "no initiative review is running; suggest work only when the daemon asks for a review"
                    .to_string()
            })?;
            if review.added >= review.max_suggestions {
                return Err(format!(
                    "this review may make {} suggestions and has; stop here",
                    review.max_suggestions
                ));
            }
            review.added += 1;
        }
        let now = now_ms();
        let result = self.change(agent_id, |env| {
            let key = normal(&title);
            let repeated = env.suggestions.iter().any(|known| {
                normal(&known.title) == key
                    && (known.status == SuggestionStatus::Open
                        || now.saturating_sub(known.created_at) < REPEAT_AFTER_MS)
            });
            if repeated {
                return Err(format!("{title:?} was suggested already; do not repeat it"));
            }
            if env
                .suggestions
                .iter()
                .filter(|s| s.status == SuggestionStatus::Open)
                .count()
                >= KEEP_OPEN
            {
                return Err(format!(
                    "{KEEP_OPEN} suggestions are waiting for the user; add no more until some are handled"
                ));
            }
            let suggestion = WorkSuggestion {
                id: format!("s{now}-{}", env.suggestions.len()),
                title,
                prompt,
                reason,
                source,
                created_at: now,
                status: SuggestionStatus::Open,
                session_id: None,
            };
            env.suggestions.push(suggestion.clone());
            prune(&mut env.suggestions, now);
            Ok(suggestion)
        });
        if result.is_err() {
            // It was not added after all: give the review its share back.
            if let Some(review) = lock(&self.inner.live).reviews.get_mut(agent_id) {
                review.added = review.added.saturating_sub(1);
            }
        }
        result
    }

    pub(crate) fn set_digest(
        &self,
        agent_id: &str,
        summary: String,
        items: Vec<DigestItem>,
    ) -> Result<(), String> {
        {
            let mut live = lock(&self.inner.live);
            let review = live.reviews.get_mut(agent_id).ok_or_else(|| {
                "no initiative review is running; send a reminder only when the daemon asks for one"
                    .to_string()
            })?;
            review.digest_given = true;
        }
        self.change(agent_id, |env| {
            env.digest = Some(WorkDigest {
                created_at: now_ms(),
                summary,
                items,
            });
        });
        Ok(())
    }

    pub(crate) fn suggestion(&self, agent_id: &str, id: &str) -> Option<WorkSuggestion> {
        self.read(agent_id, |env| {
            env.suggestions.iter().find(|s| s.id == id).cloned()
        })
    }

    /// Marks an open suggestion started or dismissed; false when it is not open.
    pub(crate) fn close_suggestion(
        &self,
        agent_id: &str,
        id: &str,
        status: SuggestionStatus,
        session_id: Option<String>,
    ) -> bool {
        self.change(agent_id, |env| {
            match env
                .suggestions
                .iter_mut()
                .find(|s| s.id == id && s.status == SuggestionStatus::Open)
            {
                Some(suggestion) => {
                    suggestion.status = status;
                    suggestion.session_id = session_id;
                    true
                }
                None => false,
            }
        })
    }

    // --- sessions the agent starts ----------------------------------------------------

    pub(crate) fn started_today(&self, agent_id: &str) -> u32 {
        let today = Local::now().date_naive();
        self.read(agent_id, |env| {
            let count = env
                .started
                .iter()
                .filter(|started| local_day(started.at) == Some(today))
                .count();
            u32::try_from(count).unwrap_or(u32::MAX)
        })
    }

    pub(crate) fn running(&self, agent_id: &str) -> u32 {
        let live = lock(&self.inner.live);
        live.running.get(agent_id).map_or(0, |counter| {
            u32::try_from(counter.load(Ordering::SeqCst)).unwrap_or(u32::MAX)
        })
    }

    /// Takes one of the day's and the moment's session slots, or says which limit is reached.
    pub(crate) fn begin_start(
        &self,
        agent_id: &str,
        max_per_day: u32,
        max_concurrent: u32,
    ) -> Result<Ticket, String> {
        let today = Local::now().date_naive();
        let mut book = lock(&self.inner.book);
        let mut live = lock(&self.inner.live);
        let counter = Arc::clone(live.running.entry(agent_id.to_string()).or_default());
        if counter.load(Ordering::SeqCst) >= usize::try_from(max_concurrent).unwrap_or(usize::MAX) {
            return Err(format!(
                "{max_concurrent} session(s) you started are still running (initiative.max_concurrent); wait for a report"
            ));
        }
        let env = book.agents.entry(agent_id.to_string()).or_default();
        let started_today = env
            .started
            .iter()
            .filter(|started| local_day(started.at) == Some(today))
            .count();
        if started_today >= usize::try_from(max_per_day).unwrap_or(usize::MAX) {
            return Err(format!(
                "{max_per_day} session(s) were started on your initiative today (initiative.max_sessions_per_day); suggest the rest with suggest_work"
            ));
        }
        live.next_pending += 1;
        let pending = format!("pending-{}", live.next_pending);
        env.started.push(Started {
            at: now_ms(),
            session_id: pending.clone(),
        });
        // Records older than a few days are of no use.
        let cutoff = now_ms().saturating_sub(3 * DAY_MS);
        env.started.retain(|started| started.at >= cutoff);
        counter.fetch_add(1, Ordering::SeqCst);
        Self::save(&self.inner, &book);
        Ok(Ticket {
            inner: Arc::clone(&self.inner),
            agent_id: agent_id.to_string(),
            pending,
            counter,
            committed: false,
        })
    }

    // --- views -------------------------------------------------------------------------

    fn snapshot(&self, agent_id: &str) -> AgentBook {
        self.read(agent_id, Clone::clone)
    }
}

/// Keeps the open suggestions and the latest handled ones.
fn prune(suggestions: &mut Vec<WorkSuggestion>, now: u64) {
    let mut closed = 0;
    let mut keep = Vec::new();
    for suggestion in suggestions.iter().rev() {
        if suggestion.status != SuggestionStatus::Open {
            closed += 1;
            if closed > KEEP_CLOSED || now.saturating_sub(suggestion.created_at) > 30 * DAY_MS {
                continue;
            }
        }
        keep.push(suggestion.clone());
    }
    keep.reverse();
    *suggestions = keep;
}

// --- the daemon's side ---------------------------------------------------------------------

/// Why a review runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// The daemon's own timetable: waits for a quiet machine and an idle environment.
    Schedule,
    /// A person asked for it: does not wait.
    User,
}

fn review_interval(daemon: &DaemonState) -> u64 {
    daemon
        .settings()
        .initiative
        .review_every_min
        .max(1)
        .saturating_mul(60_000)
}

fn digest_interval(daemon: &DaemonState) -> Option<u64> {
    match daemon.settings().initiative.digest_every_hours {
        0 => None,
        hours => Some(hours.saturating_mul(3_600_000)),
    }
}

/// What is due for an agent now, by its own timetable (not whether it may run).
fn due(daemon: &DaemonState, agent_id: &str, now: u64) -> prompt::Tasks {
    let book = daemon.initiative().snapshot(agent_id);
    let review = book
        .last_review_at
        .is_none_or(|at| now.saturating_sub(at) >= review_interval(daemon));
    let digest = digest_interval(daemon).is_some_and(|every| {
        book.last_digest_at
            .is_none_or(|at| now.saturating_sub(at) >= every)
    });
    prompt::Tasks { review, digest }
}

/// Why a review cannot run now, or the agent's session that will do it.
fn gate(
    daemon: &Arc<DaemonState>,
    env: &EnvironmentInfo,
    agent: &EnvironmentAgent,
    trigger: Trigger,
) -> Result<Arc<imperium_engine::Session>, String> {
    if agent.initiative == Initiative::Off {
        return Err("initiative is off for this agent".into());
    }
    let session = daemon
        .project_agents()
        .agent(&agent.id)
        .ok_or_else(|| "the agent is not running".to_string())?;
    if daemon.initiative().is_reviewing(&agent.id) {
        return Err("a review is running".into());
    }
    if session.status().0 != SessionStatus::Idle {
        return Err("the agent is busy".into());
    }
    if trigger == Trigger::Schedule {
        if daemon.load_view().level() != Load::Quiet {
            return Err("the machine is busy".into());
        }
        if user_active(daemon, &env.id) {
            return Err("you are working in this environment".into());
        }
    }
    Ok(session)
}

/// Whether the user's sessions in the environment are active now, or were lately.
pub(crate) fn user_active(daemon: &DaemonState, environment_id: &str) -> bool {
    let idle = Duration::from_secs(daemon.settings().initiative.idle_min.saturating_mul(60));
    daemon.environment_user_active(environment_id, idle)
}

/// The mode a review runs in: `act` only while the agent's limits leave room to start a
/// session.
fn effective_mode(daemon: &DaemonState, agent: &EnvironmentAgent) -> Initiative {
    let settings = daemon.settings();
    let book = daemon.initiative();
    if agent.initiative == Initiative::Act
        && book.started_today(&agent.id) < settings.initiative.max_sessions_per_day
        && book.running(&agent.id) < settings.initiative.max_concurrent
    {
        Initiative::Act
    } else {
        Initiative::Suggest
    }
}

/// An agent's initiative as its panel shows it.
pub(crate) fn state(
    daemon: &Arc<DaemonState>,
    env: &EnvironmentInfo,
    agent: &EnvironmentAgent,
) -> InitiativeState {
    let settings = daemon.settings();
    let book = daemon.initiative();
    let data = book.snapshot(&agent.id);
    let mut suggestions: Vec<WorkSuggestion> = data.suggestions.clone();
    // Open first, then the latest handled; newest first within each.
    suggestions.sort_by(|a, b| {
        (a.status != SuggestionStatus::Open)
            .cmp(&(b.status != SuggestionStatus::Open))
            .then(b.created_at.cmp(&a.created_at))
    });
    let waiting = gate(daemon, env, agent, Trigger::Schedule).err();
    let next = data
        .last_review_at
        .map(|at| at.saturating_add(review_interval(daemon)));
    InitiativeState {
        environment_id: env.id.clone(),
        agent_id: agent.id.clone(),
        mode: agent.initiative,
        reviewing: book.is_reviewing(&agent.id),
        waiting: waiting.filter(|reason| reason != "a review is running"),
        last_review_at: data.last_review_at,
        next_review_at: next,
        last_error: data.last_error,
        suggestions,
        digest: data.digest,
        started_today: book.started_today(&agent.id),
        max_per_day: settings.initiative.max_sessions_per_day,
        running: book.running(&agent.id),
        max_concurrent: settings.initiative.max_concurrent,
    }
}

/// Tells every client where an agent's initiative stands.
pub(crate) fn announce(daemon: &Arc<DaemonState>, agent_id: &str) {
    if let Some((env, agent)) = daemon.environments().find_agent_anywhere(agent_id) {
        daemon.announce(INITIATIVE_CHANGED, state(daemon, &env, &agent));
    }
}

fn notice(
    daemon: &DaemonState,
    env: &EnvironmentInfo,
    agent: &EnvironmentAgent,
    kind: NoticeKind,
    title: String,
    body: String,
    session_id: Option<String>,
) {
    daemon.announce(
        INITIATIVE_NOTICE,
        InitiativeNotice {
            environment_id: env.id.clone(),
            environment_name: env.name.clone(),
            agent_id: agent.id.clone(),
            agent_name: agent.name.clone(),
            kind,
            title,
            body,
            session_id,
        },
    );
}

/// Starts looking for due reviews: every `initiative.poll_s` the daemon checks each persistent
/// agent that has initiative. Between looks it does nothing.
pub fn start(daemon: &Arc<DaemonState>) {
    let weak = Arc::downgrade(daemon);
    tokio::spawn(async move {
        loop {
            let Some(daemon) = weak.upgrade() else {
                return;
            };
            let poll = daemon.settings().initiative.poll_s.max(1);
            drop(daemon);
            tokio::time::sleep(Duration::from_secs(poll)).await;
            let Some(daemon) = weak.upgrade() else {
                return;
            };
            tick(&daemon).await;
        }
    });
}

/// One look at every agent: reviews those that have initiative, are due and may run now.
/// Returns the agents reviewed, by agent id.
pub async fn tick(daemon: &Arc<DaemonState>) -> Vec<String> {
    let now = now_ms();
    let mut reviewed = Vec::new();
    for env in daemon.environments().list() {
        for agent in &env.agents {
            if agent.initiative == Initiative::Off {
                continue;
            }
            let tasks = due(daemon, &agent.id, now);
            if !tasks.review && !tasks.digest {
                continue;
            }
            match run_review(daemon, &agent.id, Trigger::Schedule).await {
                Ok(_) => reviewed.push(agent.id.clone()),
                Err(reason) => {
                    tracing::debug!(environment = %env.name, agent = %agent.name, %reason, "no initiative review now");
                }
            }
        }
    }
    reviewed
}

/// What a review did.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub suggestions: u32,
    pub digest: bool,
    pub reply: String,
}

/// Runs one review with an agent now, if it may: the agent gets the review message and the
/// permission mode that fits the mode (plan only for `suggest`, so it can read but change
/// nothing and start nothing; `default` for `act`), and the mode it had is put back after.
/// An `Err` says why nothing ran (nothing is recorded then) or how the turn failed.
pub(crate) async fn run_review(
    daemon: &Arc<DaemonState>,
    agent_id: &str,
    trigger: Trigger,
) -> Result<Outcome, String> {
    let (env, agent) = daemon
        .environments()
        .find_agent_anywhere(agent_id)
        .ok_or_else(|| "no such agent".to_string())?;
    let session = gate(daemon, &env, &agent, trigger)?;
    if let Err(stop) = crate::budgets::check(daemon, session.id(), false) {
        return Err(stop.message);
    }
    let settings = daemon.settings();
    let now = now_ms();
    let mut tasks = due(daemon, &agent.id, now);
    if trigger == Trigger::User {
        tasks.review = true;
    }
    if !tasks.review && !tasks.digest {
        return Err("nothing is due".into());
    }
    let mode = effective_mode(daemon, &agent);
    let book = daemon.initiative();
    let begun = book.begin_review(
        &agent.id,
        Review {
            mode,
            max_suggestions: settings.initiative.max_suggestions,
            added: 0,
            digest_given: false,
        },
    );
    if !begun {
        return Err("a review is running".into());
    }
    announce(daemon, &agent.id);

    let message = prompt::render(&facts(daemon, &env, &agent, &settings), mode, tasks);
    let permission = if mode == Initiative::Act {
        imperium_api::PermissionMode::Default
    } else {
        imperium_api::PermissionMode::Plan
    };
    let before = session.permission_mode();
    daemon.set_permission_mode(&session, permission);
    let events = session.subscribe();
    let timeout = Duration::from_secs(settings.initiative.review_timeout_min.max(1) * 60);
    let result = if session.send(message).is_err() {
        Err("the agent's session closed".to_string())
    } else {
        daemon.wait_for_turn(&session, events, timeout).await
    };
    // Put the mode back unless the user changed it meanwhile.
    if session.permission_mode() == permission {
        daemon.set_permission_mode(&session, before);
    }
    let finished = book.end_review(&agent.id).unwrap_or(Review {
        mode,
        max_suggestions: 0,
        added: 0,
        digest_given: false,
    });
    let (reply, error) = match result {
        Ok(reply) => (reply, None),
        Err(message) => (String::new(), Some(message)),
    };
    // A failed review waits out the interval like a finished one, so it does not retry in a
    // loop.
    book.finish_review(&agent.id, now_ms(), tasks.digest, error.clone());
    announce(daemon, &agent.id);
    if finished.added > 0 {
        notice(
            daemon,
            &env,
            &agent,
            NoticeKind::Suggestions,
            format!("{} has {} suggestion(s)", agent.name, finished.added),
            format!("Open {} to start one.", env.name),
            None,
        );
    }
    if finished.digest_given {
        let summary = book
            .snapshot(&agent.id)
            .digest
            .map(|digest| digest.summary)
            .unwrap_or_default();
        notice(
            daemon,
            &env,
            &agent,
            NoticeKind::Digest,
            format!("Work left in {}", env.name),
            summary,
            None,
        );
    }
    match error {
        Some(message) => Err(message),
        None => Ok(Outcome {
            suggestions: finished.added,
            digest: finished.digest_given,
            reply,
        }),
    }
}

/// What the daemon tells the agent about its environment.
fn facts(
    daemon: &Arc<DaemonState>,
    env: &EnvironmentInfo,
    agent: &EnvironmentAgent,
    settings: &imperium_config::Settings,
) -> prompt::Facts {
    let book = daemon.initiative();
    let data = book.snapshot(&agent.id);
    let now = now_ms();
    let mut sessions: Vec<(u64, String)> = Vec::new();
    for session in daemon.sessions() {
        let summary = daemon.summary(&session);
        if summary.environment_id.as_deref() != Some(env.id.as_str()) || summary.persistent {
            continue;
        }
        let active = daemon
            .session_activity_ms(session.id())
            .unwrap_or_else(|| summary.created_at.unwrap_or(0));
        let by = match &summary.started_by {
            Some(by) if by.agent_id == agent.id => ", started by you",
            Some(_) => ", started by an agent",
            None => "",
        };
        sessions.push((
            active,
            format!(
                "\"{}\" ({:?}, {}{by})",
                imperium_api::truncate_chars(summary.title.trim(), 80),
                summary.status,
                ago(now, active),
            ),
        ));
    }
    sessions.sort_by(|a, b| b.0.cmp(&a.0));
    sessions.truncate(12);
    let titles = |status: &dyn Fn(SuggestionStatus) -> bool| -> Vec<String> {
        data.suggestions
            .iter()
            .filter(|s| status(s.status))
            .map(|s| s.title.clone())
            .collect()
    };
    let local = Local::now();
    prompt::Facts {
        environment: env.name.clone(),
        agent: agent.name.clone(),
        folders: env.folders.clone(),
        when: local.format("%A %Y-%m-%d %H:%M").to_string(),
        sessions: sessions.into_iter().map(|(_, line)| line).collect(),
        open: titles(&|s| s == SuggestionStatus::Open),
        closed: titles(&|s| s != SuggestionStatus::Open),
        last_digest: data.digest.map(|digest| digest.summary),
        running: book.running(&agent.id),
        max_concurrent: settings.initiative.max_concurrent,
        started_today: book.started_today(&agent.id),
        max_per_day: settings.initiative.max_sessions_per_day,
        max_suggestions: settings.initiative.max_suggestions,
    }
}

fn ago(now: u64, then: u64) -> String {
    let minutes = now.saturating_sub(then) / 60_000;
    match minutes {
        0..=1 => "just now".to_string(),
        2..=119 => format!("{minutes} min ago"),
        120..=2879 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

/// Watches an agent for the turns that report a session it started on its own initiative: what
/// it starts in them follows the initiative limits too.
pub(crate) fn watch_agent(
    daemon: &Arc<DaemonState>,
    agent_id: String,
    mut events: broadcast::Receiver<AgentEvent>,
) {
    let weak = Arc::downgrade(daemon);
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if event.agent_id != ROOT_AGENT {
                continue;
            }
            let Some(daemon) = weak.upgrade() else {
                return;
            };
            match event.event {
                SessionEvent::TurnStarted { text, .. } => {
                    let follow = daemon.initiative().is_report_of_initiative_session(&text);
                    daemon.initiative().set_follow_up(&agent_id, follow);
                }
                SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. } => {
                    daemon.initiative().set_follow_up(&agent_id, false);
                }
                _ => {}
            }
        }
    });
}

/// Checks a `start_session` the agent makes during an initiative turn and takes a slot for it.
/// Outside `act`, without a reason and a source, while the user is active, or past a limit, it
/// is refused with a message the agent reads.
pub(crate) fn permit_start(
    daemon: &Arc<DaemonState>,
    env: &EnvironmentInfo,
    agent: &EnvironmentAgent,
    reason: Option<&str>,
    source: Option<&str>,
) -> Result<Ticket, String> {
    let settings = daemon.settings();
    let book = daemon.initiative();
    let mode = book
        .review(&agent.id)
        .map_or(agent.initiative, |review| review.mode);
    if agent.initiative != Initiative::Act || mode != Initiative::Act {
        return Err(
            "this is an initiative turn and you are not in act mode (or today's limit is reached): do not start sessions; record the work with suggest_work and the user starts it".to_string(),
        );
    }
    if reason.is_none_or(|text| text.trim().is_empty())
        || source.is_none_or(|text| text.trim().is_empty())
    {
        return Err(
            "during an initiative turn start_session needs `reason` (why now) and `source` (what you read: a file and heading, an issue, a session); without a source, do not start it".to_string(),
        );
    }
    if user_active(daemon, &env.id) {
        return Err(
            "the user is active in this environment, so you do not start work now; record it with suggest_work".to_string(),
        );
    }
    book.begin_start(
        &agent.id,
        settings.initiative.max_sessions_per_day,
        settings.initiative.max_concurrent,
    )
}

/// A brief with its source named, when the agent's wording does not already.
pub(crate) fn with_source(brief: &str, source: Option<&str>) -> String {
    match source.map(str::trim).filter(|source| !source.is_empty()) {
        Some(source) if !brief.to_lowercase().contains(&source.to_lowercase()) => {
            format!("{}\n\n(From {source}.)", brief.trim_end())
        }
        _ => brief.to_string(),
    }
}

/// Tells the user the agent started a session.
pub(crate) fn announce_started(
    daemon: &Arc<DaemonState>,
    env: &EnvironmentInfo,
    agent: &EnvironmentAgent,
    session_id: &str,
    reason: &str,
) {
    notice(
        daemon,
        env,
        agent,
        NoticeKind::Started,
        format!("{} started a session", agent.name),
        reason.to_string(),
        Some(session_id.to_string()),
    );
    announce(daemon, &agent.id);
}

// --- methods -------------------------------------------------------------------------------

fn invalid(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message)
}

/// The environment and agent a request names; the agent must be one of that environment's.
fn subject(
    daemon: &DaemonState,
    environment_id: &str,
    agent_id: &str,
) -> Result<(EnvironmentInfo, EnvironmentAgent), ErrorObject> {
    let env = daemon
        .environments()
        .get(environment_id)
        .ok_or_else(|| invalid(format!("no environment {environment_id:?}")))?;
    let agent = env
        .agents
        .iter()
        .find(|agent| agent.id == agent_id)
        .cloned()
        .ok_or_else(|| invalid(format!("no agent {agent_id:?} in {}", env.name)))?;
    Ok((env, agent))
}

pub(crate) fn get(
    daemon: &Arc<DaemonState>,
    params: InitiativeRef,
) -> Result<InitiativeState, ErrorObject> {
    let (env, agent) = subject(daemon, &params.environment_id, &params.agent_id)?;
    Ok(state(daemon, &env, &agent))
}

pub(crate) fn dismiss(
    daemon: &Arc<DaemonState>,
    params: InitiativeSuggestionRef,
) -> Result<(), ErrorObject> {
    subject(daemon, &params.environment_id, &params.agent_id)?;
    if !daemon.initiative().close_suggestion(
        &params.agent_id,
        &params.suggestion_id,
        SuggestionStatus::Dismissed,
        None,
    ) {
        return Err(invalid("that suggestion is not open"));
    }
    announce(daemon, &params.agent_id);
    Ok(())
}

/// Starts an open suggestion as the user's own session, with its prompt as the first message.
pub(crate) fn start_suggestion(
    daemon: &Arc<DaemonState>,
    params: InitiativeSuggestionRef,
) -> Result<InitiativeStartResult, ErrorObject> {
    let (env, agent) = subject(daemon, &params.environment_id, &params.agent_id)?;
    let suggestion = daemon
        .initiative()
        .suggestion(&agent.id, &params.suggestion_id)
        .filter(|suggestion| suggestion.status == SuggestionStatus::Open)
        .ok_or_else(|| invalid("that suggestion is not open"))?;
    let (session, _events) = daemon
        .create_in_environment(
            &env.id,
            None,
            SessionOptions {
                started_by: Some(StartedBy {
                    environment_id: env.id.clone(),
                    environment_name: env.name.clone(),
                    agent_id: agent.id.clone(),
                    agent_name: agent.name.clone(),
                    kind: "suggestion".into(),
                    reason: suggestion.reason.clone(),
                    source: Some(suggestion.source.clone()),
                }),
                ..SessionOptions::default()
            },
        )
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?;
    let id = session.id().to_string();
    if let Err(stop) = crate::budgets::check(daemon, &id, false) {
        let _ = daemon.delete_session(&id, false);
        return Err(stop);
    }
    if session
        .send(with_source(&suggestion.prompt, Some(&suggestion.source)))
        .is_err()
    {
        let _ = daemon.delete_session(&id, false);
        return Err(ErrorObject::new(
            codes::INTERNAL_ERROR,
            "the session closed before it started",
        ));
    }
    daemon.initiative().close_suggestion(
        &agent.id,
        &suggestion.id,
        SuggestionStatus::Started,
        Some(id.clone()),
    );
    announce(daemon, &agent.id);
    Ok(InitiativeStartResult { session_id: id })
}

/// Reviews now, at a person's request; the result arrives as `initiative.changed`.
pub(crate) fn review_now(
    daemon: &Arc<DaemonState>,
    params: InitiativeRef,
) -> Result<(), ErrorObject> {
    let (env, agent) = subject(daemon, &params.environment_id, &params.agent_id)?;
    // The checks that fail at once are reported to the caller.
    gate(daemon, &env, &agent, Trigger::User).map_err(invalid)?;
    let daemon = Arc::clone(daemon);
    tokio::spawn(async move {
        if let Err(reason) = run_review(&daemon, &agent.id, Trigger::User).await {
            tracing::info!(agent = %agent.name, %reason, "initiative review did not finish");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> InitiativeBook {
        InitiativeBook {
            inner: Arc::new(Inner {
                path: None,
                book: Mutex::new(File::default()),
                live: Mutex::new(Live::default()),
            }),
        }
    }

    fn review(book: &InitiativeBook, max: u32) {
        assert!(book.begin_review(
            "e",
            Review {
                mode: Initiative::Suggest,
                max_suggestions: max,
                added: 0,
                digest_given: false,
            }
        ));
    }

    fn suggest(book: &InitiativeBook, title: &str) -> Result<WorkSuggestion, String> {
        book.add_suggestion(
            "e",
            title.into(),
            "do it".into(),
            "why".into(),
            "PLAN.md".into(),
        )
    }

    #[test]
    fn suggestions_exist_only_during_a_review_and_not_twice() {
        let book = book();
        assert!(
            suggest(&book, "Tidy")
                .unwrap_err()
                .contains("no initiative review")
        );
        review(&book, 2);
        let first = suggest(&book, "Tidy the roadmap").unwrap();
        assert!(
            suggest(&book, "tidy  the roadmap!")
                .unwrap_err()
                .contains("already")
        );
        // The refused repeat did not use up the review's share.
        suggest(&book, "Critique the memory design").unwrap();
        assert!(
            suggest(&book, "A third")
                .unwrap_err()
                .contains("may make 2")
        );
        // Handled work is not suggested again for a week.
        assert!(book.close_suggestion("e", &first.id, SuggestionStatus::Dismissed, None));
        book.end_review("e");
        review(&book, 2);
        assert!(
            suggest(&book, "Tidy the roadmap")
                .unwrap_err()
                .contains("already")
        );
        assert_eq!(book.snapshot("e").suggestions.len(), 2);
    }

    #[test]
    fn started_sessions_are_limited_per_day_and_at_once() {
        let book = book();
        let first = book.begin_start("e", 2, 1).unwrap();
        assert_eq!(book.running("e"), 1);
        assert!(
            book.begin_start("e", 2, 1)
                .unwrap_err()
                .contains("still running")
        );
        let guard = first.commit("s1");
        drop(guard);
        assert_eq!(book.running("e"), 0);
        // A ticket dropped without a session gives its slot back and does not count.
        drop(book.begin_start("e", 2, 1).unwrap());
        assert_eq!(book.started_today("e"), 1);
        let second = book.begin_start("e", 2, 1).unwrap();
        drop(second.commit("s2"));
        assert_eq!(book.started_today("e"), 2);
        assert!(book.begin_start("e", 2, 1).unwrap_err().contains("today"));
        // Another environment has its own allowance.
        assert!(book.begin_start("other", 2, 1).is_ok());
    }

    #[test]
    fn a_report_of_a_session_the_agent_started_is_recognised() {
        let book = book();
        book.begin_start("e", 2, 1).unwrap().commit("abc123");
        assert!(book.is_report_of_initiative_session("Session abc123 finished. Its report:\n\nok"));
        assert!(book.is_report_of_initiative_session("Session abc123 did not finish: x"));
        assert!(!book.is_report_of_initiative_session("Session zzz finished."));
        assert!(!book.is_report_of_initiative_session("please start a session"));
    }

    #[test]
    fn a_source_is_named_in_the_brief_once() {
        assert_eq!(
            with_source("Improve my timeline", Some("docs/12-roadmap.md#next")),
            "Improve my timeline\n\n(From docs/12-roadmap.md#next.)"
        );
        let cited = "Do item 3 of docs/12-roadmap.md";
        assert_eq!(with_source(cited, Some("docs/12-roadmap.md")), cited);
        assert_eq!(with_source("x", None), "x");
    }

    #[test]
    fn only_open_and_the_latest_handled_suggestions_are_kept() {
        let mut list: Vec<WorkSuggestion> = (0..30)
            .map(|n| WorkSuggestion {
                id: format!("s{n}"),
                title: format!("t{n}"),
                prompt: String::new(),
                reason: String::new(),
                source: String::new(),
                created_at: 1000 + n,
                status: if n < 25 {
                    SuggestionStatus::Dismissed
                } else {
                    SuggestionStatus::Open
                },
                session_id: None,
            })
            .collect();
        prune(&mut list, 2000);
        assert_eq!(
            list.iter()
                .filter(|s| s.status == SuggestionStatus::Open)
                .count(),
            5
        );
        assert_eq!(list.len(), 5 + KEEP_CLOSED);
    }
}
