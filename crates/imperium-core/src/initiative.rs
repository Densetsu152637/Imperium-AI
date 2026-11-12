//! Initiative: a persistent agent looks for work on its own (see
//! docs/14-projects-and-automation.md#initiative-implemented).
//!
//! Each persistent agent of an environment sets `off` (the default), `suggest` or `act`. Every
//! `initiative.review_every_min` the daemon checks whether a review is due for each agent that
//! has initiative. A review is the daemon sending the agent a message built from a template
//! ([`prompt`]) with facts only the daemon knows; the agent reads the environment's documents
//! itself and answers with tools: `suggest_work` and `work_digest` ([`tools`]) in `suggest`, and
//! `start_session` as well in `act`. It runs only while the machine is quiet and the user has not
//! been active in the environment (their sessions, or their chat with any agent of it), so it
//! never competes with the user.
//!
//! While a review's turn runs, the agent is in plan mode and the launcher refuses every tool
//! but reading, searching and recording ([`REVIEW_TOOLS`]), for the root agent and its subagents.
//! The restrictions end when the turn does, not when the daemon stops waiting for it.
//!
//! What an agent suggests is kept in `initiative.json` in the data folder, per agent, with the
//! sessions it started (for the daily limit). A suggestion becomes a session only when the user
//! starts it (`initiative.start`), or, in `act` mode, when the agent calls `start_session` during
//! a review, within `initiative.max_sessions_per_day` and `initiative.max_concurrent` (counted per
//! agent). Those sessions are tagged with who started them and why ([`StartedBy`]).

mod prompt;
mod tools;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
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
use tokio::sync::{broadcast, oneshot};

pub(crate) use tools::InitiativeTools;

use crate::{DaemonState, SessionOptions};

const DAY_MS: u64 = 86_400_000;
/// Suggestions waiting for the user, and handled ones kept to avoid repeating them.
const KEEP_OPEN: usize = 12;
const KEEP_CLOSED: usize = 20;
/// A handled suggestion is not suggested again for this long.
const REPEAT_AFTER_MS: u64 = 7 * DAY_MS;

/// The tools an agent may use during an initiative review, whoever calls them (the root agent or
/// a subagent it spawned): read and search the environment and the notes, plan, delegate reading,
/// and record suggestions. Everything else (writing, commands, the web, MCP tools, plugins,
/// memory writes, the app's own config) is refused for the length of the turn. `start_session`
/// is listed because `act` reviews use it; the launcher allows it only there.
pub(crate) const REVIEW_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "glob",
    "grep",
    "update_plan",
    "context_load",
    "memory_search",
    "memory_read",
    "search_sessions",
    "spawn_agent",
    "await_tasks",
    "list_agents",
    "suggest_work",
    "work_digest",
    "start_session",
];

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

/// What a review did.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub suggestions: u32,
    pub digest: bool,
    pub reply: String,
}

/// A review in progress: what the agent may do in it, and what is needed to end it.
#[derive(Debug)]
pub(crate) struct Review {
    /// `suggest` or `act`: `act` only while the limits leave room.
    pub mode: Initiative,
    pub max_suggestions: u32,
    pub added: u32,
    pub digest_given: bool,
    /// Whether this review also asked for the work-left reminder.
    pub digest_attempted: bool,
    /// The agent's permission mode before the review (put back when its turn ends), and the
    /// session it was set on.
    pub before: imperium_api::PermissionMode,
    pub session_id: String,
    /// The review's own turn has started (not some other turn of the agent).
    pub started: bool,
    pub reply: String,
    /// Told how the review ended.
    pub done: Option<oneshot::Sender<Result<Outcome, String>>>,
}

#[derive(Debug, Default)]
struct Live {
    reviews: HashMap<String, Review>,
    /// Agents that are working on the report of a session they started on their own initiative:
    /// they follow the same limits as a review.
    follow_ups: HashSet<String>,
    running: HashMap<String, Arc<AtomicUsize>>,
    /// Starts allowed but not yet recorded with a session, by agent.
    pending: HashMap<String, u32>,
    /// Sessions started on an agent's own initiative, until the daemon stops.
    sessions: HashSet<String>,
    /// Agents whose next turn is the report of a session they started on their own initiative,
    /// with the time the flag lapses. Set by the daemon right before it delivers the report to
    /// an idle agent; the agent's next turn takes it.
    follow_up_due: HashMap<String, u64>,
    /// When the user last talked to an agent (a message that was not the daemon's).
    chats: HashMap<String, u64>,
    /// Agents that were deleted: nothing is written for them again.
    forgotten: HashSet<String>,
}

#[derive(Debug)]
struct Inner {
    path: Option<PathBuf>,
    book: Mutex<File>,
    live: Mutex<Live>,
    /// Held while the file is written, so writes do not overtake each other. Never held with
    /// `book` or `live` while writing.
    writing: Mutex<()>,
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
    counter: Arc<AtomicUsize>,
    committed: bool,
}

impl Ticket {
    /// Records the session that used the permission.
    pub(crate) fn commit(mut self, session_id: &str) -> RunningGuard {
        {
            let mut book = lock(&self.inner.book);
            let mut live = lock(&self.inner.live);
            if !live.forgotten.contains(&self.agent_id) {
                book.agents
                    .entry(self.agent_id.clone())
                    .or_default()
                    .started
                    .push(Started {
                        at: now_ms(),
                        session_id: session_id.to_string(),
                    });
            }
            let pending = live.pending.entry(self.agent_id.clone()).or_default();
            *pending = pending.saturating_sub(1);
            live.sessions.insert(session_id.to_string());
        }
        InitiativeBook::save(&self.inner);
        self.committed = true;
        RunningGuard(Arc::clone(&self.counter))
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut live = lock(&self.inner.live);
        let pending = live.pending.entry(self.agent_id.clone()).or_default();
        *pending = pending.saturating_sub(1);
        drop(live);
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
    /// Reads `path`; a missing file starts empty, and an unreadable one is set aside (not
    /// overwritten) with a warning.
    pub fn load(path: PathBuf) -> Self {
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
                let aside = path.with_extension("json.unreadable");
                tracing::warn!(path = %path.display(), %error, "the initiative file is unreadable; setting it aside");
                let _ = std::fs::rename(&path, aside);
                File::default()
            }),
            Err(_) => File::default(),
        };
        InitiativeBook {
            inner: Arc::new(Inner {
                path: Some(path),
                book: Mutex::new(file),
                live: Mutex::new(Live::default()),
                writing: Mutex::new(()),
            }),
        }
    }

    /// Writes the file: a temporary file renamed over it. Call with no lock on `book` or `live`
    /// held; the state is read for the write after taking the write lock, so the last write has
    /// the latest state.
    fn save(inner: &Inner) {
        let Some(path) = &inner.path else { return };
        let _writing = lock(&inner.writing);
        let text = {
            let book = lock(&inner.book);
            serde_json::to_string_pretty(&*book)
        };
        let written = text.map_err(std::io::Error::other).and_then(|text| {
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, text)?;
            std::fs::rename(&temporary, path)
        });
        if let Err(error) = written {
            tracing::warn!(path = %path.display(), %error, "could not save the initiative file");
        }
    }

    fn change<R>(&self, agent_id: &str, change: impl FnOnce(&mut AgentBook) -> R) -> R {
        let result = {
            let mut book = lock(&self.inner.book);
            change(book.agents.entry(agent_id.to_string()).or_default())
        };
        Self::save(&self.inner);
        result
    }

    fn read<R>(&self, agent_id: &str, read: impl FnOnce(&AgentBook) -> R) -> R {
        let book = lock(&self.inner.book);
        match book.agents.get(agent_id) {
            Some(agent) => read(agent),
            None => read(&AgentBook::default()),
        }
    }

    fn gone(&self, agent_id: &str) -> bool {
        lock(&self.inner.live).forgotten.contains(agent_id)
    }

    /// Forgets an agent that was deleted: its records go, and nothing is written for it again.
    pub(crate) fn forget_agent(&self, agent_id: &str) {
        let removed = {
            let mut book = lock(&self.inner.book);
            let mut live = lock(&self.inner.live);
            live.forgotten.insert(agent_id.to_string());
            live.reviews.remove(agent_id);
            live.follow_ups.remove(agent_id);
            live.follow_up_due.remove(agent_id);
            live.chats.remove(agent_id);
            book.agents.remove(agent_id).is_some()
        };
        if removed {
            Self::save(&self.inner);
        }
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

    /// Ends a review, handing it back to say how.
    pub(crate) fn take_review(&self, agent_id: &str) -> Option<Review> {
        lock(&self.inner.live).reviews.remove(agent_id)
    }

    /// The mode a running review has (`suggest` or `act`).
    pub(crate) fn review_mode(&self, agent_id: &str) -> Option<Initiative> {
        lock(&self.inner.live)
            .reviews
            .get(agent_id)
            .map(|review| review.mode)
    }

    pub(crate) fn is_reviewing(&self, agent_id: &str) -> bool {
        lock(&self.inner.live).reviews.contains_key(agent_id)
    }

    /// Whether the review's own turn has started.
    pub(crate) fn review_started(&self, agent_id: &str) -> bool {
        lock(&self.inner.live)
            .reviews
            .get(agent_id)
            .is_some_and(|review| review.started)
    }

    fn mark_review_started(&self, agent_id: &str) {
        if let Some(review) = lock(&self.inner.live).reviews.get_mut(agent_id) {
            review.started = true;
        }
    }

    fn push_review_text(&self, agent_id: &str, text: &str) {
        if let Some(review) = lock(&self.inner.live).reviews.get_mut(agent_id)
            && review.started
        {
            review.reply.push_str(text);
        }
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
    /// on its own initiative: what it starts then follows the initiative limits.
    pub(crate) fn initiative_turn(&self, agent_id: &str) -> bool {
        let live = lock(&self.inner.live);
        live.reviews.contains_key(agent_id) || live.follow_ups.contains(agent_id)
    }

    /// Whether `id` is a session started on an agent's own initiative.
    pub(crate) fn is_initiative_session(&self, id: &str) -> bool {
        lock(&self.inner.live).sessions.contains(id)
    }

    /// Notes that the daemon is about to deliver a report to an idle agent, whose next turn is
    /// then a follow-up. (The turn is told by this flag, not by its text: the engine may replace
    /// a long message with a stub before the turn starts.)
    pub(crate) fn expect_follow_up(&self, agent_id: &str) {
        lock(&self.inner.live)
            .follow_up_due
            .insert(agent_id.to_string(), now_ms().saturating_add(30_000));
    }

    /// Whether the agent's turn that just started is the report the daemon announced (and
    /// forgets the announcement).
    fn take_follow_up(&self, agent_id: &str) -> bool {
        lock(&self.inner.live)
            .follow_up_due
            .remove(agent_id)
            .is_some_and(|until| now_ms() <= until)
    }

    /// Forgets an announcement that did not lead to a turn.
    fn forget_follow_up(&self, agent_id: &str) {
        lock(&self.inner.live).follow_up_due.remove(agent_id);
    }

    /// Notes that the user talked to the agent.
    fn note_user_chat(&self, agent_id: &str, at: u64) {
        lock(&self.inner.live)
            .chats
            .insert(agent_id.to_string(), at);
    }

    /// Whether the user talked to the agent in the last `within_ms`.
    pub(crate) fn chatted_within(&self, agent_id: &str, within_ms: u64) -> bool {
        lock(&self.inner.live)
            .chats
            .get(agent_id)
            .is_some_and(|at| now_ms().saturating_sub(*at) < within_ms)
    }

    /// Records how a review ended. Nothing is written for an agent that was deleted.
    pub(crate) fn finish_review(
        &self,
        agent_id: &str,
        at: u64,
        digest_attempted: bool,
        error: Option<String>,
    ) {
        if self.gone(agent_id) {
            return;
        }
        self.change(agent_id, |agent| {
            agent.last_review_at = Some(at);
            if digest_attempted {
                agent.last_digest_at = Some(at);
            }
            agent.last_error = error;
        });
    }

    /// Makes the agent's next review (and reminder) a full interval away from `at` (after `act` is
    /// turned on).
    pub(crate) fn postpone_review(&self, agent_id: &str, at: u64) {
        if self.gone(agent_id) {
            return;
        }
        self.change(agent_id, |agent| {
            agent.last_review_at = Some(at);
            agent.last_digest_at = Some(at);
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
            if live.forgotten.contains(agent_id) {
                return Err("this agent was deleted".to_string());
            }
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
        let result = self.change(agent_id, |agent| {
            let key = normal(&title);
            let repeated = agent.suggestions.iter().any(|known| {
                normal(&known.title) == key
                    && (known.status == SuggestionStatus::Open
                        || now.saturating_sub(known.created_at) < REPEAT_AFTER_MS)
            });
            if repeated {
                return Err(format!("{title:?} was suggested already; do not repeat it"));
            }
            if agent
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
                id: format!("s{now}-{}", agent.suggestions.len()),
                title,
                prompt,
                reason,
                source,
                created_at: now,
                status: SuggestionStatus::Open,
                session_id: None,
            };
            agent.suggestions.push(suggestion.clone());
            prune(&mut agent.suggestions, now);
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
            if live.forgotten.contains(agent_id) {
                return Err("this agent was deleted".to_string());
            }
            let review = live.reviews.get_mut(agent_id).ok_or_else(|| {
                "no initiative review is running; send a reminder only when the daemon asks for one"
                    .to_string()
            })?;
            review.digest_given = true;
        }
        self.change(agent_id, |agent| {
            agent.digest = Some(WorkDigest {
                created_at: now_ms(),
                summary,
                items,
            });
        });
        Ok(())
    }

    pub(crate) fn suggestion(&self, agent_id: &str, id: &str) -> Option<WorkSuggestion> {
        self.read(agent_id, |agent| {
            agent.suggestions.iter().find(|s| s.id == id).cloned()
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
        self.change(agent_id, |agent| {
            match agent
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

    /// Sessions started today, recorded and about to be.
    pub fn started_today(&self, agent_id: &str) -> u32 {
        let today = Local::now().date_naive();
        let recorded = self.read(agent_id, |agent| {
            agent
                .started
                .iter()
                .filter(|started| local_day(started.at) == Some(today))
                .count()
        });
        let pending = lock(&self.inner.live)
            .pending
            .get(agent_id)
            .copied()
            .unwrap_or(0);
        u32::try_from(recorded)
            .unwrap_or(u32::MAX)
            .saturating_add(pending)
    }

    /// Sessions the agent started that are still running (or about to).
    pub fn running(&self, agent_id: &str) -> u32 {
        let live = lock(&self.inner.live);
        live.running.get(agent_id).map_or(0, |counter| {
            u32::try_from(counter.load(Ordering::SeqCst)).unwrap_or(u32::MAX)
        })
    }

    /// Takes one of the day's and the moment's session slots, or says which limit is reached.
    /// Nothing is written until a session uses the slot ([`Ticket::commit`]), so a crash between
    /// leaves no record.
    pub(crate) fn begin_start(
        &self,
        agent_id: &str,
        max_per_day: u32,
        max_concurrent: u32,
    ) -> Result<Ticket, String> {
        let today = Local::now().date_naive();
        let book = lock(&self.inner.book);
        let mut live = lock(&self.inner.live);
        if live.forgotten.contains(agent_id) {
            return Err("this agent was deleted".to_string());
        }
        let counter = Arc::clone(live.running.entry(agent_id.to_string()).or_default());
        if counter.load(Ordering::SeqCst) >= usize::try_from(max_concurrent).unwrap_or(usize::MAX) {
            return Err(format!(
                "{max_concurrent} session(s) you started are still running (initiative.max_concurrent); wait for a report"
            ));
        }
        let recorded = book.agents.get(agent_id).map_or(0, |agent| {
            agent
                .started
                .iter()
                .filter(|started| local_day(started.at) == Some(today))
                .count()
        });
        let pending = live.pending.get(agent_id).copied().unwrap_or(0);
        let started_today = recorded.saturating_add(usize::try_from(pending).unwrap_or(0));
        if started_today >= usize::try_from(max_per_day).unwrap_or(usize::MAX) {
            return Err(format!(
                "{max_per_day} session(s) were started on your initiative today (initiative.max_sessions_per_day); suggest the rest with suggest_work"
            ));
        }
        *live.pending.entry(agent_id.to_string()).or_default() += 1;
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Ticket {
            inner: Arc::clone(&self.inner),
            agent_id: agent_id.to_string(),
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

/// Whether the user is active in the environment now, or was lately: one of their sessions has
/// a turn running, waits on them, or wrote its journal, or they talked to any of the
/// environment's persistent agents.
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
    // A review whose agent was stopped has not ended; it is paused until the agent is resumed.
    let paused = book.is_reviewing(&agent.id)
        && daemon
            .project_agents()
            .agent(&agent.id)
            .is_some_and(|session| session.is_stopped());
    let waiting = if paused {
        Some("paused: the review was stopped; resume the agent to let its turn end".to_string())
    } else {
        gate(daemon, env, agent, Trigger::Schedule).err()
    };
    let next = data
        .last_review_at
        .map(|at| at.saturating_add(review_interval(daemon)));
    InitiativeState {
        environment_id: env.id.clone(),
        agent_id: agent.id.clone(),
        mode: agent.initiative,
        reviewing: book.is_reviewing(&agent.id) && !paused,
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

/// Runs one review with an agent now, if it may. The agent gets the review message in **plan
/// mode**, and for the length of the turn the launcher refuses every tool but [`REVIEW_TOOLS`]
/// (`start_session` only in `act` and within the limits). The restrictions, and the mode the
/// agent had, are undone by the watcher when the review's own turn ends ([`conclude`]), not when
/// this function stops waiting: a review that times out is stopped, and stays restricted until
/// its turn ends.
///
/// An `Err` says why nothing ran (nothing is recorded then), how the turn failed, or why the
/// wait ended early.
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
    let (done, finished) = oneshot::channel();
    let begun = book.begin_review(
        &agent.id,
        Review {
            mode,
            max_suggestions: settings.initiative.max_suggestions,
            added: 0,
            digest_given: false,
            digest_attempted: tasks.digest,
            before: session.permission_mode(),
            session_id: session.id().to_string(),
            started: false,
            reply: String::new(),
            done: Some(done),
        },
    );
    if !begun {
        return Err("a review is running".into());
    }
    announce(daemon, &agent.id);

    let message = prompt::render(&facts(daemon, &env, &agent, &settings), mode, tasks);
    // Every review is in plan mode: it reads and records, and changes nothing, whatever the
    // user's rules and approvals allow elsewhere.
    set_review_mode(daemon, &session, imperium_api::PermissionMode::Plan);
    if session.send(message).is_err() {
        conclude(daemon, &agent.id, Err("the agent's session closed".into()));
        return Err("the agent's session closed".into());
    }
    let timeout = Duration::from_secs(settings.initiative.review_timeout_s.max(1));
    wait_for_review(
        daemon, &session, &env.id, &agent.id, trigger, timeout, finished,
    )
    .await
}

/// Sets the session's permission mode for the review (or puts the user's back), in memory only:
/// nothing is written to the session's record, so a restart can never leave the agent in the
/// review's mode. Clients are told.
fn set_review_mode(
    daemon: &Arc<DaemonState>,
    session: &Arc<imperium_engine::Session>,
    mode: imperium_api::PermissionMode,
) {
    session.set_permission_mode(mode);
    daemon.announce(
        imperium_protocol::methods::SESSION_UPDATED,
        daemon.summary(session),
    );
}

/// Waits for the review to end, checking now and then that it should go on.
async fn wait_for_review(
    daemon: &Arc<DaemonState>,
    session: &Arc<imperium_engine::Session>,
    environment_id: &str,
    agent_id: &str,
    trigger: Trigger,
    timeout: Duration,
    mut finished: oneshot::Receiver<Result<Outcome, String>>,
) -> Result<Outcome, String> {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut look = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            result = &mut finished => {
                return result.unwrap_or_else(|_| Err("the review ended without a result".into()));
            }
            () = &mut deadline => {
                // Nothing more runs; the turn ends when the agent is resumed, still restricted.
                session.stop();
                announce(daemon, agent_id);
                return Err(format!(
                    "the review timed out after {} s and the agent was stopped; resume it to let the turn end",
                    timeout.as_secs()
                ));
            }
            _ = look.tick() => {
                let current = daemon.environments().find_agent_anywhere(agent_id);
                if current.is_none_or(|(_, agent)| agent.initiative == Initiative::Off) {
                    session.stop();
                    announce(daemon, agent_id);
                    return Err("initiative was turned off during the review, so the agent was stopped; resume it to let the turn end".into());
                }
                let book = daemon.initiative();
                if trigger == Trigger::Schedule && book.review_started(agent_id) {
                    let queued = session.queue(None).is_ok_and(|queued| !queued.is_empty());
                    if queued || user_active(daemon, environment_id) {
                        return Err("you became active in the environment; the review finishes in the background and your messages wait behind it".into());
                    }
                }
            }
        }
    }
}

/// Ends the review whose turn ended: puts the agent's permission mode back (on the session the
/// review set it on), records how it went, tells the user, and tells whoever waits.
fn conclude(daemon: &Arc<DaemonState>, agent_id: &str, result: Result<(), String>) {
    let book = daemon.initiative();
    let Some(mut review) = book.take_review(agent_id) else {
        return;
    };
    if let Some(session) = daemon.project_agents().agent(agent_id)
        && session.id() == review.session_id
        && session.permission_mode() == imperium_api::PermissionMode::Plan
    {
        set_review_mode(daemon, &session, review.before);
    }
    let (reply, error) = match result {
        Ok(()) => (std::mem::take(&mut review.reply).trim().to_string(), None),
        Err(message) => (String::new(), Some(message)),
    };
    // A deleted agent leaves nothing behind.
    if let Some((env, agent)) = daemon.environments().find_agent_anywhere(agent_id) {
        // A failed review waits out the interval like a finished one, so it does not retry in
        // a loop.
        book.finish_review(agent_id, now_ms(), review.digest_attempted, error.clone());
        announce(daemon, agent_id);
        if review.added > 0 {
            notice(
                daemon,
                &env,
                &agent,
                NoticeKind::Suggestions,
                format!("{} has {} suggestion(s)", agent.name, review.added),
                format!("Open {} to start one.", env.name),
                None,
            );
        }
        if review.digest_given {
            let summary = book
                .snapshot(agent_id)
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
    }
    if let Some(done) = review.done.take() {
        let _ = done.send(match error {
            Some(message) => Err(message),
            None => Ok(Outcome {
                suggestions: review.added,
                digest: review.digest_given,
                reply,
            }),
        });
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
                "\"{}\" ({:?}, {}{by}; id {})",
                imperium_api::truncate_chars(summary.title.trim(), 80),
                summary.status,
                ago(now, active),
                summary.session_id,
            ),
        ));
    }
    sessions.sort_by_key(|entry| std::cmp::Reverse(entry.0));
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

/// Watches an agent for as long as its session lives: it ends reviews when their turn ends,
/// answers (through the approval queue) what a review's agents ask, tells the daemon which turns
/// are follow-ups to reports of sessions it started on its own initiative (they follow the same
/// limits) and which are the user talking to the agent (that counts as the user being active).
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
                Err(broadcast::error::RecvError::Closed) => {
                    // The session is gone (the agent restarted, or was deleted): a review that
                    // was running on it cannot end any other way.
                    if let Some(daemon) = weak.upgrade() {
                        daemon.initiative().set_follow_up(&agent_id, false);
                        conclude(
                            &daemon,
                            &agent_id,
                            Err("the agent's session ended during the review".into()),
                        );
                    }
                    return;
                }
            };
            let Some(daemon) = weak.upgrade() else {
                return;
            };
            let book = daemon.initiative();
            let root = event.agent_id == ROOT_AGENT;
            match event.event {
                // Nobody watches a review: what its agents ask goes to the approval queue.
                SessionEvent::PermissionRequested {
                    request_id,
                    tool,
                    summary,
                    ..
                } if book.initiative_turn(&agent_id) => {
                    if let Some(session) = daemon.project_agents().agent(&agent_id) {
                        daemon.bridge_permission(&session, request_id, &tool, summary);
                    }
                }
                SessionEvent::QuestionsAsked {
                    request_id,
                    questions,
                } if book.initiative_turn(&agent_id) => {
                    if let Some(session) = daemon.project_agents().agent(&agent_id) {
                        crate::state::answer_with_recommendations(&session, request_id, &questions);
                    }
                }
                SessionEvent::TurnStarted { text, .. } if root => {
                    if text.starts_with(prompt::MARKER) {
                        book.forget_follow_up(&agent_id);
                        book.mark_review_started(&agent_id);
                    } else if book.take_follow_up(&agent_id) {
                        book.set_follow_up(&agent_id, true);
                    } else {
                        book.set_follow_up(&agent_id, false);
                        book.note_user_chat(&agent_id, now_ms());
                    }
                }
                SessionEvent::TextDelta { text } if root => book.push_review_text(&agent_id, &text),
                SessionEvent::TurnCompleted { .. } if root => {
                    book.set_follow_up(&agent_id, false);
                    if book.review_started(&agent_id) {
                        conclude(&daemon, &agent_id, Ok(()));
                    }
                }
                SessionEvent::TurnFailed { message } if root => {
                    book.set_follow_up(&agent_id, false);
                    if book.review_started(&agent_id) {
                        conclude(&daemon, &agent_id, Err(message));
                    }
                }
                _ => {}
            }
        }
    });
}

/// Checks that `source` names something the agent could have read: a file or folder inside the
/// environment's folders (optionally with a `#heading` or `:line`), or a session of the
/// environment's it was shown. The daemon cannot check that it says what the agent claims.
pub(crate) fn check_source(
    daemon: &DaemonState,
    env: &EnvironmentInfo,
    source: &str,
) -> Result<(), String> {
    let source = source.trim();
    let refused = || {
        format!(
            "source {source:?} is not something you read in this environment: name a file or folder inside its folders (for example docs/12-roadmap.md#next) or a session id from the list"
        )
    };
    if source.is_empty() {
        return Err(refused());
    }
    let path_part = source.split('#').next().unwrap_or_default();
    let path_part = match path_part.rsplit_once(':') {
        Some((path, line)) if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) => path,
        _ => path_part,
    };
    if daemon.session(path_part).is_some_and(|session| {
        daemon.summary(&session).environment_id.as_deref() == Some(env.id.as_str())
    }) || daemon.archived_sessions().iter().any(|archived| {
        archived.session_id == path_part
            && archived.environment_id.as_deref() == Some(env.id.as_str())
    }) {
        return Ok(());
    }
    let candidate = Path::new(path_part);
    if path_part.is_empty()
        || candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(refused());
    }
    // Folders on another machine cannot be looked at from here: a clean relative path will do.
    if env.runtime.is_remote() {
        return if candidate.is_relative() {
            Ok(())
        } else {
            Err(refused())
        };
    }
    for folder in &env.folders {
        let folder = Path::new(folder);
        let full = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            folder.join(candidate)
        };
        if full.starts_with(folder) && full.exists() {
            return Ok(());
        }
    }
    Err(refused())
}

/// Checks a `start_session` the agent makes during an initiative turn and takes a slot for it.
/// Outside `act`, without a reason and a source, with a source that is not a file or session of
/// the environment, while the user is active, or past a limit, it is refused with a message the
/// agent reads.
pub(crate) fn permit_start(
    daemon: &Arc<DaemonState>,
    env: &EnvironmentInfo,
    agent: &EnvironmentAgent,
    reason: Option<&str>,
    source: Option<&str>,
) -> Result<Ticket, String> {
    let settings = daemon.settings();
    let book = daemon.initiative();
    let mode = book.review_mode(&agent.id).unwrap_or(agent.initiative);
    if agent.initiative != Initiative::Act || mode != Initiative::Act {
        return Err(
            "this is an initiative turn and you are not in act mode (or today's limit is reached): do not start sessions; record the work with suggest_work and the user starts it".to_string(),
        );
    }
    if reason.is_none_or(|text| text.trim().is_empty())
        || source.is_none_or(|text| text.trim().is_empty())
    {
        return Err(
            "during an initiative turn start_session needs `reason` (why now) and `source` (what you read: a file and heading, or a session id); without a source, do not start it".to_string(),
        );
    }
    check_source(daemon, env, source.unwrap_or_default())?;
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

/// The refusal the launcher gives a tool call during a review, or none when it is allowed.
pub(crate) fn review_restriction(
    daemon: &DaemonState,
    agent_id: &str,
    tool: &str,
) -> Option<String> {
    let book = daemon.initiative();
    let review = book.is_reviewing(agent_id);
    if !review && !book.initiative_turn(agent_id) {
        return None;
    }
    // The follow-up to a report is held to the same tools, except that it records nothing:
    // `start_session` is its only way to act, through the same checks and limits.
    let recording = matches!(tool, "suggest_work" | "work_digest");
    if REVIEW_TOOLS.contains(&tool) && (review || !recording) {
        return None;
    }
    Some(if review {
        format!(
            "{tool} is not available during an initiative review: only reading, searching and recording suggestions (suggest_work, work_digest) are allowed"
        )
    } else {
        format!(
            "{tool} is not available while you follow up on a session you started on your own initiative: only reading and searching, and starting a session within your limits, are allowed"
        )
    })
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
/// A used-up budget that asks or blocks refuses it with the budget's own message.
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

/// Reviews now, at a person's request; the result arrives as `initiative.changed`. What stops it
/// at once (initiative off, the agent busy, a used-up budget) is the caller's error.
pub(crate) fn review_now(
    daemon: &Arc<DaemonState>,
    params: InitiativeRef,
) -> Result<(), ErrorObject> {
    let (env, agent) = subject(daemon, &params.environment_id, &params.agent_id)?;
    let session = gate(daemon, &env, &agent, Trigger::User).map_err(invalid)?;
    crate::budgets::check(daemon, session.id(), false)?;
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
                writing: Mutex::new(()),
            }),
        }
    }

    fn a_review(max: u32) -> Review {
        Review {
            mode: Initiative::Suggest,
            max_suggestions: max,
            added: 0,
            digest_given: false,
            digest_attempted: false,
            before: imperium_api::PermissionMode::Default,
            session_id: "s".into(),
            started: false,
            reply: String::new(),
            done: None,
        }
    }

    fn review(book: &InitiativeBook, max: u32) {
        assert!(book.begin_review("e", a_review(max)));
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
        assert!(book.take_review("e").is_some());
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
        // A slot taken counts toward the day before it has a session.
        assert_eq!(book.started_today("e"), 1);
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
        // Another agent has its own allowance.
        assert!(book.begin_start("other", 2, 1).is_ok());
    }

    #[test]
    fn the_daemons_report_flag_is_taken_once_and_lapses() {
        let book = book();
        assert!(!book.take_follow_up("e"), "nothing announced");
        book.expect_follow_up("e");
        assert!(book.take_follow_up("e"));
        assert!(!book.take_follow_up("e"), "taken once");
        book.expect_follow_up("e");
        book.forget_follow_up("e");
        assert!(!book.take_follow_up("e"));
        // A flag that was never taken lapses.
        lock(&book.inner.live)
            .follow_up_due
            .insert("e".into(), now_ms().saturating_sub(1));
        assert!(!book.take_follow_up("e"));
    }

    #[test]
    fn a_deleted_agent_is_not_recreated_by_a_late_write() {
        let book = book();
        review(&book, 2);
        suggest(&book, "Tidy").unwrap();
        book.forget_agent("e");
        book.finish_review("e", 5, true, Some("late".into()));
        book.postpone_review("e", 7);
        let snapshot = book.snapshot("e");
        assert!(snapshot.last_review_at.is_none() && snapshot.suggestions.is_empty());
        assert!(suggest(&book, "Again").is_err());
        assert!(book.begin_start("e", 2, 1).is_err());
        assert!(!book.is_reviewing("e"));
    }

    #[test]
    fn the_file_is_written_whole_and_an_unreadable_one_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("initiative.json");
        let first = InitiativeBook::load(path.clone());
        assert!(first.begin_review("e", a_review(2)));
        suggest(&first, "Tidy the roadmap").unwrap();
        drop(first);
        // No temporary file is left, and the suggestion survives a reload.
        assert!(!dir.path().join("initiative.json.tmp").exists());
        let second = InitiativeBook::load(path.clone());
        assert_eq!(second.snapshot("e").suggestions.len(), 1);
        drop(second);

        std::fs::write(&path, "{ not json").unwrap();
        let third = InitiativeBook::load(path.clone());
        assert!(third.snapshot("e").suggestions.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("initiative.json.unreadable")).unwrap(),
            "{ not json",
            "the damaged file is kept, not overwritten"
        );
    }

    #[test]
    fn user_chat_counts_for_a_while() {
        let book = book();
        assert!(!book.chatted_within("e", 60_000));
        book.note_user_chat("e", now_ms());
        assert!(book.chatted_within("e", 60_000));
        assert!(!book.chatted_within("e", 0));
        book.note_user_chat("e", now_ms().saturating_sub(120_000));
        assert!(!book.chatted_within("e", 60_000));
    }

    #[test]
    fn a_review_may_only_read_search_and_record() {
        for tool in [
            "read_file",
            "grep",
            "suggest_work",
            "work_digest",
            "start_session",
        ] {
            assert!(REVIEW_TOOLS.contains(&tool), "{tool}");
        }
        for tool in [
            "write_file",
            "edit_file",
            "run_command",
            "web_fetch",
            "web_search",
            "memory_remember",
            "install_extension",
            "set_model_card",
            "activate_plugin",
            "browser_navigate",
            "generate_image",
        ] {
            assert!(!REVIEW_TOOLS.contains(&tool), "{tool}");
        }
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
