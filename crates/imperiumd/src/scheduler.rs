//! Runs `[[schedules]]` while the daemon runs, whether or not any window is open. Each run is a
//! new session with the schedule's prompt, or a shell command; every client hears about runs
//! through `schedule.event` notifications, and `notify = ["os"]` adds a system notification.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Local};
use imperium_automation::{History, Missed, RunRecord, Trigger, next_run};
use imperium_config::ScheduleSettings;
use imperium_protocol::methods::{
    SCHEDULE_EVENT, ScheduleEventParams, ScheduleListResult, ScheduleRow,
};
use imperium_protocol::session::{ROOT_AGENT, SessionEvent};
use tokio::sync::{Notify, broadcast};

use crate::DaemonState;

/// Longest a run may take unless the schedule says otherwise.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3600);
/// The loop wakes at least this often, so clock changes and sleep are noticed.
const MAX_SLEEP: Duration = Duration::from_secs(30);
/// Longest summary kept for a run.
const SUMMARY_CHARS: usize = 500;

struct Entry {
    settings: ScheduleSettings,
    trigger: Result<Trigger, String>,
    missed: Missed,
    allow_overlap: bool,
}

pub struct Scheduler {
    entries: Vec<Entry>,
    history: Mutex<History>,
    next: Mutex<HashMap<String, Option<DateTime<Local>>>>,
    running: Mutex<HashSet<String>>,
    wake: Notify,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("schedules", &self.entries.len())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Scheduler {
    /// Builds the scheduler for `daemon`'s schedules and starts its loop. Run history is kept
    /// in `<data>/schedules/history.json`.
    pub fn start(daemon: &Arc<DaemonState>) -> Arc<Self> {
        let now = Local::now();
        let history = History::load(&daemon.data_dir().join("schedules").join("history.json"));
        let entries: Vec<Entry> = daemon
            .settings()
            .schedules
            .iter()
            .map(|settings| {
                let trigger = Trigger::parse(
                    settings.trigger.cron.as_deref(),
                    settings.trigger.every.as_deref(),
                    settings.trigger.at.as_deref(),
                );
                if let Err(error) = &trigger {
                    tracing::warn!(schedule = %settings.id, %error, "schedule cannot run");
                }
                Entry {
                    missed: match settings.missed.as_deref() {
                        Some("skip") => Missed::Skip,
                        _ => Missed::RunOnceOnWake,
                    },
                    allow_overlap: settings.overlap.as_deref() == Some("allow"),
                    trigger,
                    settings: settings.clone(),
                }
            })
            .collect();
        let next = entries
            .iter()
            .filter(|entry| entry.settings.enabled)
            .filter_map(|entry| {
                let trigger = entry.trigger.as_ref().ok()?;
                let last = history.last(&entry.settings.id).map(|record| record.at);
                Some((
                    entry.settings.id.clone(),
                    next_run(trigger, entry.missed, last, now),
                ))
            })
            .collect();
        let scheduler = Arc::new(Scheduler {
            entries,
            history: Mutex::new(history),
            next: Mutex::new(next),
            running: Mutex::new(HashSet::new()),
            wake: Notify::new(),
        });
        tokio::spawn(run_loop(Arc::clone(&scheduler), Arc::downgrade(daemon)));
        scheduler
    }

    pub fn list(&self) -> ScheduleListResult {
        let next = lock(&self.next);
        let running = lock(&self.running);
        let history = lock(&self.history);
        let schedules = self
            .entries
            .iter()
            .map(|entry| {
                let id = &entry.settings.id;
                let trigger = &entry.settings.trigger;
                let (kind, value) = match (&trigger.cron, &trigger.every, &trigger.at) {
                    (Some(cron), ..) => ("cron", cron.clone()),
                    (_, Some(every), _) => ("every", every.clone()),
                    (.., Some(at)) => ("at", at.clone()),
                    _ => ("none", String::new()),
                };
                let last = history.last(id);
                ScheduleRow {
                    id: id.clone(),
                    enabled: entry.settings.enabled,
                    trigger: format!("{kind} {value}"),
                    action: if entry.settings.action.prompt.is_some() {
                        "prompt".into()
                    } else {
                        "command".into()
                    },
                    project: entry.settings.action.project.display().to_string(),
                    next_run: next.get(id).copied().flatten().map(|at| at.to_rfc3339()),
                    last_run: last.map(|record| record.at.to_rfc3339()),
                    last_ok: last.map(|record| record.ok),
                    running: running.contains(id),
                    problem: entry.trigger.as_ref().err().cloned(),
                }
            })
            .collect();
        ScheduleListResult { schedules }
    }

    /// Starts a schedule now; false when it is already running and does not allow overlap.
    pub fn run_now(self: &Arc<Self>, daemon: &Arc<DaemonState>, id: &str) -> Result<bool, String> {
        let entry = self
            .entries
            .iter()
            .position(|entry| entry.settings.id == id)
            .ok_or_else(|| format!("no schedule named {id:?}"))?;
        Ok(self.spawn_run(daemon, entry))
    }

    fn spawn_run(self: &Arc<Self>, daemon: &Arc<DaemonState>, index: usize) -> bool {
        let entry = &self.entries[index];
        let id = entry.settings.id.clone();
        if !lock(&self.running).insert(id.clone()) && !entry.allow_overlap {
            return false;
        }
        let scheduler = Arc::clone(self);
        let daemon = Arc::clone(daemon);
        tokio::spawn(async move {
            let settings = scheduler.entries[index].settings.clone();
            announce(&daemon, &settings.id, "started", None, String::new());
            let started = Local::now();
            let (ok, summary, session_id) = run(&daemon, &settings).await;
            let record = RunRecord {
                at: started,
                ok,
                summary: summary.clone(),
                session_id: session_id.clone(),
            };
            if let Err(error) = lock(&scheduler.history).record(&settings.id, record) {
                tracing::warn!(schedule = %settings.id, %error, "could not record the run");
            }
            lock(&scheduler.running).remove(&settings.id);
            let status = if ok { "completed" } else { "failed" };
            announce(&daemon, &settings.id, status, session_id, summary.clone());
            if settings.notify.iter().any(|target| target == "os") {
                notify_os(&settings.id, status, &summary);
            }
            scheduler.wake.notify_one();
        });
        true
    }
}

/// Fires schedules as they come due.
async fn run_loop(scheduler: Arc<Scheduler>, daemon: std::sync::Weak<DaemonState>) {
    loop {
        let Some(daemon) = daemon.upgrade() else {
            return;
        };
        let now = Local::now();
        let mut soonest = now + chrono::Duration::from_std(MAX_SLEEP).unwrap_or_default();
        let due: Vec<usize> = {
            let mut next = lock(&scheduler.next);
            scheduler
                .entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    let slot = next.get_mut(&entry.settings.id)?;
                    let at = (*slot)?;
                    if at > now {
                        soonest = soonest.min(at);
                        return None;
                    }
                    *slot = entry.trigger.as_ref().ok()?.next_after(now);
                    Some(index)
                })
                .collect()
        };
        for index in due {
            if !scheduler.spawn_run(&daemon, index) {
                tracing::info!(schedule = %scheduler.entries[index].settings.id, "previous run still going; skipped");
            }
        }
        drop(daemon);
        let wait = (soonest - Local::now()).to_std().unwrap_or(Duration::ZERO);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = scheduler.wake.notified() => {}
        }
    }
}

/// Runs a schedule's action: `(ok, summary, session id)`.
async fn run(
    daemon: &Arc<DaemonState>,
    settings: &ScheduleSettings,
) -> (bool, String, Option<String>) {
    let action = &settings.action;
    let timeout = action
        .timeout_s
        .map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    let project = action.project.display().to_string();
    if let Some(command) = &action.command {
        return match run_command(command, &action.project, timeout).await {
            Ok((ok, output)) => (ok, output, None),
            Err(error) => (false, error, None),
        };
    }
    let prompt = action.prompt.clone().unwrap_or_default();
    let (session, mut events) = match daemon.create_session(&project, action.model.as_deref()) {
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
    let mut reply = String::new();
    let outcome = tokio::time::timeout(timeout, async {
        loop {
            match events.recv().await {
                Ok(event) if event.agent_id == ROOT_AGENT => match event.event {
                    SessionEvent::TextDelta { text } => reply.push_str(&text),
                    SessionEvent::TurnCompleted { .. } => return Ok(()),
                    SessionEvent::TurnFailed { message } => return Err(message),
                    _ => {}
                },
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("the session ended".to_string());
                }
            }
        }
    })
    .await;
    match outcome {
        Ok(Ok(())) => (true, truncate(reply.trim()), session_id),
        Ok(Err(message)) => (false, truncate(&message), session_id),
        Err(_) => (
            false,
            format!(
                "timed out after {} s (a permission request may be waiting)",
                timeout.as_secs()
            ),
            session_id,
        ),
    }
}

async fn run_command(
    command: &str,
    project: &Path,
    timeout: Duration,
) -> Result<(bool, String), String> {
    let child = imperium_tools::shell(command)
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("could not start the command: {error}"))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("timed out after {} s", timeout.as_secs()))?
        .map_err(|error| error.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // Keep the end, where results and errors usually are.
    let tail: String = text
        .trim()
        .chars()
        .rev()
        .take(SUMMARY_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok((output.status.success(), tail))
}

fn truncate(text: &str) -> String {
    text.chars().take(SUMMARY_CHARS).collect()
}

fn announce(
    daemon: &DaemonState,
    id: &str,
    status: &str,
    session_id: Option<String>,
    summary: String,
) {
    daemon.announce(
        SCHEDULE_EVENT,
        ScheduleEventParams {
            schedule_id: id.to_string(),
            status: status.to_string(),
            session_id,
            summary,
        },
    );
}

/// A system notification; failures (no notification service) are logged, not fatal.
fn notify_os(id: &str, status: &str, summary: &str) {
    let title = format!("Imperium: {id} {status}");
    let body = summary.chars().take(200).collect::<String>();
    tokio::task::spawn_blocking(move || {
        if let Err(error) = notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .show()
        {
            tracing::warn!(%error, "could not show a system notification");
        }
    });
}
