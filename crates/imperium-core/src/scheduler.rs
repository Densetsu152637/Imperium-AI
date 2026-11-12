//! Runs `[[schedules]]` while the daemon runs, whether or not any window is open. Each run is a
//! new session with the schedule's prompt, or a shell command; every client hears about runs
//! through `schedule.event` notifications, and `notify = ["os"]` adds a system notification.
//!
//! A schedule with `prefer_quiet` waits, once due, until the machine is quiet (the daemon's load
//! view), for at most `max_defer`, then runs anyway; `conditions.when_quiet` waits as long as it
//! takes. A person can run a waiting schedule at once. Prompt runs are background sessions: they
//! rank below the user's sessions and pause while the machine is busy.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Local};
use imperium_automation::{History, Missed, RunRecord, Trigger, next_run, parse_duration};
use imperium_config::ScheduleSettings;
use imperium_engine::{Load, RunNow, Waited, wait_for_quiet};
use imperium_protocol::methods::{
    SCHEDULE_EVENT, ScheduleEventParams, ScheduleListResult, ScheduleRow,
};
use tokio::sync::Notify;

use crate::{DaemonState, SessionOptions};

/// Longest a run may take unless the schedule says otherwise.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3600);
/// The loop wakes at least this often, so clock changes and sleep are noticed.
const MAX_SLEEP: Duration = Duration::from_secs(30);
/// Longest summary kept for a run.
const SUMMARY_CHARS: usize = 500;
/// How long a `prefer_quiet` run waits for a quiet machine unless the schedule says otherwise.
pub const DEFAULT_MAX_DEFER: Duration = Duration::from_secs(2 * 3600);

struct Entry {
    settings: ScheduleSettings,
    trigger: Result<Trigger, String>,
    missed: Missed,
    allow_overlap: bool,
    /// Wait for a quiet machine before a run starts, and for how long (`None`: as long as it
    /// takes).
    quiet: Option<Option<Duration>>,
}

/// A run waiting for a quiet machine.
struct Deferred {
    reason: String,
    run_now: Arc<RunNow>,
}

/// What a deferred run waits on.
struct Wait {
    view: Arc<dyn imperium_engine::LoadView>,
    run_now: Arc<RunNow>,
    limit: Option<Duration>,
    reason: String,
}

pub struct Scheduler {
    entries: Vec<Entry>,
    history: Mutex<History>,
    next: Mutex<HashMap<String, Option<DateTime<Local>>>>,
    running: Mutex<HashSet<String>>,
    /// Runs waiting for a quiet machine, by schedule id.
    deferred: Mutex<HashMap<String, Deferred>>,
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
                let max_defer = match settings.max_defer.as_deref().map(parse_duration) {
                    Some(Ok(length)) => length,
                    Some(Err(error)) => {
                        tracing::warn!(schedule = %settings.id, %error, "max_defer is not a length of time; using 2 h");
                        DEFAULT_MAX_DEFER
                    }
                    None => DEFAULT_MAX_DEFER,
                };
                let quiet = if settings.conditions.when_quiet {
                    Some(None)
                } else {
                    settings.prefer_quiet.then_some(Some(max_defer))
                };
                Entry {
                    quiet,
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
            deferred: Mutex::new(HashMap::new()),
            wake: Notify::new(),
        });
        tokio::spawn(run_loop(Arc::clone(&scheduler), Arc::downgrade(daemon)));
        scheduler
    }

    pub fn list(&self) -> ScheduleListResult {
        let next = lock(&self.next);
        let running = lock(&self.running);
        let deferred = lock(&self.deferred);
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
                    action: match &entry.settings.action {
                        action if action.prompt.is_some() => "prompt".into(),
                        action if action.agent.is_some() => "agent".into(),
                        _ => "command".into(),
                    },
                    project: entry.settings.action.project.display().to_string(),
                    environment: entry.settings.action.environment.clone(),
                    next_run: next.get(id).copied().flatten().map(|at| at.to_rfc3339()),
                    last_run: last.map(|record| record.at.to_rfc3339()),
                    last_ok: last.map(|record| record.ok),
                    running: running.contains(id),
                    deferred: deferred.get(id).map(|run| run.reason.clone()),
                    problem: entry.trigger.as_ref().err().cloned(),
                }
            })
            .collect();
        ScheduleListResult { schedules }
    }

    /// Starts a schedule now, whatever the load; a run waiting for a quiet machine is released
    /// instead. False when it is already running and does not allow overlap.
    pub fn run_now(self: &Arc<Self>, daemon: &Arc<DaemonState>, id: &str) -> Result<bool, String> {
        let entry = self
            .entries
            .iter()
            .position(|entry| entry.settings.id == id)
            .ok_or_else(|| format!("no schedule named {id:?}"))?;
        if let Some(waiting) = lock(&self.deferred).get(id) {
            waiting.run_now.release();
            return Ok(true);
        }
        Ok(self.spawn_run(daemon, entry, true))
    }

    /// Marks a run of the schedule at `index` as waiting for a quiet machine, if the schedule
    /// asks for one and the machine is not quiet. Called with `deferred` locked, so a tick that
    /// comes due meanwhile sees the waiting run.
    fn defer(
        &self,
        daemon: &DaemonState,
        index: usize,
        deferred: &mut HashMap<String, Deferred>,
    ) -> Option<Wait> {
        let entry = &self.entries[index];
        let limit = entry.quiet?;
        let view = daemon.load_view();
        if view.level() == Load::Quiet {
            return None;
        }
        let run_now = Arc::new(RunNow::default());
        let reason = match limit {
            Some(limit) => format!(
                "waiting for a quiet machine (it is {}; runs anyway after {} min)",
                view.level().label(),
                limit.as_secs().div_ceil(60)
            ),
            None => format!(
                "waiting for a quiet machine (it is {})",
                view.level().label()
            ),
        };
        deferred.insert(
            entry.settings.id.clone(),
            Deferred {
                reason: reason.clone(),
                run_now: Arc::clone(&run_now),
            },
        );
        Some(Wait {
            view,
            run_now,
            limit,
            reason,
        })
    }

    /// Runs the schedule at `index`. A `manual` run (a person asked for it) does not wait for a
    /// quiet machine. False when the run is skipped: one is still going and overlap is not
    /// allowed, or (for a run that is not manual) one is already waiting for a quiet machine,
    /// which this tick then joins.
    fn spawn_run(self: &Arc<Self>, daemon: &Arc<DaemonState>, index: usize, manual: bool) -> bool {
        let entry = &self.entries[index];
        let id = entry.settings.id.clone();
        let wait = {
            let mut running = lock(&self.running);
            let mut deferred = lock(&self.deferred);
            if !manual && deferred.contains_key(&id) {
                return false;
            }
            if !running.insert(id.clone()) && !entry.allow_overlap {
                return false;
            }
            if manual {
                None
            } else {
                self.defer(daemon, index, &mut deferred)
            }
        };
        let scheduler = Arc::clone(self);
        let daemon = Arc::clone(daemon);
        tokio::spawn(async move {
            let settings = scheduler.entries[index].settings.clone();
            // After `max_defer`, or when a person runs it now, the run goes ahead whatever the
            // load, and the session it starts does not give way again.
            let run_now = match wait {
                Some(wait) => {
                    announce(&daemon, &settings.id, "deferred", None, wait.reason);
                    let waited = wait_for_quiet(&*wait.view, &wait.run_now, wait.limit).await;
                    lock(&scheduler.deferred).remove(&settings.id);
                    tracing::info!(schedule = %settings.id, ?waited, "run no longer deferred");
                    waited != Waited::Quiet
                }
                None => manual,
            };
            announce(&daemon, &settings.id, "started", None, String::new());
            let started = Local::now();
            let (ok, summary, session_id) = run(&daemon, &settings, run_now).await;
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
            if !scheduler.spawn_run(&daemon, index, false) {
                tracing::info!(schedule = %scheduler.entries[index].settings.id, "previous run still going or waiting for a quiet machine; skipped");
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
    run_now: bool,
) -> (bool, String, Option<String>) {
    let action = &settings.action;
    let timeout = action
        .timeout_s
        .map_or(DEFAULT_TIMEOUT, Duration::from_secs);
    // An environment-bound schedule works in the environment, found by id or name when it runs,
    // so renaming it or deleting it is noticed.
    let environment = match &action.environment {
        Some(reference) => match daemon.environments().find(reference) {
            Ok(env) => Some(env),
            Err(error) => return (false, error.to_string(), None),
        },
        None => None,
    };
    let project = action.project.display().to_string();
    if let Some(message) = &action.agent {
        let text = format!("Schedule {} fired: {message}", settings.id);
        if let Some(env) = &environment {
            let delivered =
                daemon.tell_environment_agents(&env.id, action.to_agent.as_deref(), &text);
            return if delivered > 0 {
                (
                    true,
                    format!("delivered to {delivered} agent(s) of {}", env.name),
                    None,
                )
            } else {
                (
                    false,
                    format!(
                        "{} has no running agent to tell (add one with `imperium env agent add`, \
                         or set agent = true on a [[projects]] entry with this environment)",
                        env.name
                    ),
                    None,
                )
            };
        }
        return if daemon.tell_project_agent(&action.project, text) {
            (true, "delivered to the project agent".into(), None)
        } else {
            (
                false,
                format!(
                    "{project} has no running project agent (set agent = true under [[projects]])"
                ),
                None,
            )
        };
    }
    if let Some(command) = &action.command {
        let dir = match &environment {
            Some(env) => match env.folders.first() {
                Some(folder) if !env.runtime.is_remote() => PathBuf::from(folder),
                _ => {
                    return (
                        false,
                        format!(
                            "{} has no folder on this machine to run a command in",
                            env.name
                        ),
                        None,
                    );
                }
            },
            None => action.project.clone(),
        };
        return match run_command(command, &dir, timeout).await {
            Ok((ok, output)) => (ok, output, None),
            Err(error) => (false, error, None),
        };
    }
    let prompt = action.prompt.clone().unwrap_or_default();
    let options = SessionOptions {
        background: true,
        run_now,
        ..SessionOptions::default()
    };
    let (ok, summary, session_id) = match &environment {
        Some(env) => {
            daemon
                .run_prompt_in_environment(
                    &env.id,
                    action.model.as_deref(),
                    prompt,
                    options,
                    timeout,
                )
                .await
        }
        None => {
            daemon
                .run_prompt(&project, action.model.as_deref(), prompt, options, timeout)
                .await
        }
    };
    (ok, truncate(&summary), session_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualLoad;
    use imperium_engine::Load;

    /// A daemon whose one schedule only tells a project agent that does not exist, so a run
    /// ends at once without touching a model or a process.
    fn start_daemon(
        schedule: &str,
        load: &Arc<ManualLoad>,
    ) -> (Arc<DaemonState>, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().display().to_string().replace('\\', "/");
        let config = format!(
            r#"
            [models]
            default = "fake:model"
            [endpoints.fake]
            base_url = "http://127.0.0.1:9/v1"
            [memory]
            enabled = false

            [[schedules]]
            id = "report"
            trigger = {{ at = "2099-01-01 00:00" }}
            action = {{ project = "{project}", agent = "ping" }}
            {schedule}
            "#
        );
        let settings = imperium_config::Config::from_layers([
            imperium_config::Layer::defaults(),
            imperium_config::Layer::new(
                imperium_config::Source::User("test".into()),
                config.parse().unwrap(),
            ),
        ])
        .settings()
        .unwrap();
        let daemon = DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
        daemon.use_load_view(load.clone());
        (daemon, home)
    }

    fn row(scheduler: &Scheduler) -> ScheduleRow {
        scheduler.list().schedules.remove(0)
    }

    async fn minutes(count: u64) {
        tokio::time::sleep(Duration::from_secs(count * 60)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_deferral_waits_while_busy_and_runs_when_quiet() {
        let load = ManualLoad::new(Load::Busy);
        let (daemon, _home) = start_daemon("prefer_quiet = true", &load);
        let scheduler = Scheduler::start(&daemon);
        assert!(scheduler.spawn_run(&daemon, 0, false));
        minutes(20).await;
        let waiting = row(&scheduler);
        assert!(waiting.running && waiting.last_run.is_none());
        assert!(
            waiting
                .deferred
                .as_deref()
                .is_some_and(|reason| reason.contains("quiet") && reason.contains("busy")),
            "{waiting:?}"
        );
        // Saturated is no better than busy.
        load.set(Load::Saturated);
        minutes(20).await;
        assert!(row(&scheduler).deferred.is_some());
        load.set(Load::Quiet);
        minutes(1).await;
        let ran = row(&scheduler);
        assert!(ran.deferred.is_none() && !ran.running);
        assert!(ran.last_run.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_deferral_runs_anyway_after_max_defer() {
        let load = ManualLoad::new(Load::Busy);
        let (daemon, _home) = start_daemon("prefer_quiet = true\nmax_defer = \"30m\"", &load);
        let scheduler = Scheduler::start(&daemon);
        assert!(scheduler.spawn_run(&daemon, 0, false));
        minutes(29).await;
        assert!(row(&scheduler).deferred.is_some(), "still within max_defer");
        minutes(2).await;
        let ran = row(&scheduler);
        assert!(ran.deferred.is_none() && ran.last_run.is_some(), "{ran:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_deferral_for_a_condition_never_gives_up_but_a_person_can_run_it() {
        let load = ManualLoad::new(Load::Busy);
        let (daemon, _home) = start_daemon("conditions = { when_quiet = true }", &load);
        let scheduler = Scheduler::start(&daemon);
        assert!(scheduler.spawn_run(&daemon, 0, false));
        minutes(10 * 60).await;
        assert!(row(&scheduler).deferred.is_some(), "no time limit");
        // "Run now" releases the waiting run instead of starting a second one.
        assert_eq!(scheduler.run_now(&daemon, "report"), Ok(true));
        minutes(1).await;
        let ran = row(&scheduler);
        assert!(ran.deferred.is_none() && ran.last_run.is_some(), "{ran:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_deferral_with_overlap_allowed_joins_ticks_to_the_waiting_run() {
        let load = ManualLoad::new(Load::Busy);
        let (daemon, _home) = start_daemon(
            "prefer_quiet = true
overlap = \"allow\"",
            &load,
        );
        let mut announcements = daemon.announcements();
        let scheduler = Scheduler::start(&daemon);
        assert!(scheduler.spawn_run(&daemon, 0, false));
        for _ in 0..3 {
            minutes(5).await;
            assert!(
                !scheduler.spawn_run(&daemon, 0, false),
                "joins the waiting run"
            );
        }
        assert!(row(&scheduler).deferred.is_some());
        load.set(Load::Quiet);
        minutes(1).await;
        let ran = row(&scheduler);
        assert!(
            ran.deferred.is_none() && !ran.running && ran.last_run.is_some(),
            "{ran:?}"
        );
        let mut started = 0;
        while let Ok(notification) = announcements.try_recv() {
            if notification.method == SCHEDULE_EVENT && notification.params["status"] == "started" {
                started += 1;
            }
        }
        assert_eq!(started, 1, "one run, not one per tick");
        // Once nothing waits, the next tick starts a run again.
        assert!(scheduler.spawn_run(&daemon, 0, false));
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_deferral_is_off_unless_asked_for_and_skipped_when_already_quiet() {
        let load = ManualLoad::new(Load::Busy);
        let (daemon, _home) = start_daemon("", &load);
        let scheduler = Scheduler::start(&daemon);
        assert!(scheduler.spawn_run(&daemon, 0, false));
        minutes(1).await;
        assert!(row(&scheduler).last_run.is_some(), "no preference: it runs");

        let load = ManualLoad::new(Load::Quiet);
        let (daemon, _home) = start_daemon("prefer_quiet = true", &load);
        let scheduler = Scheduler::start(&daemon);
        assert!(scheduler.spawn_run(&daemon, 0, false));
        minutes(1).await;
        assert!(row(&scheduler).last_run.is_some());
    }
}
