//! A session's explicit status (idle, working, needs attention, paused, watching, merged) and
//! its controls for usage limits: how to wait when a provider says usage has run out, and a
//! way to resume early.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveTime, TimeZone};
use imperium_protocol::methods::SessionHold;
use imperium_protocol::session::{
    AgentEvent, PauseCause, ResumePolicy, SessionEvent, SessionStatus,
};
use tokio::sync::{Notify, watch};

use crate::quiet::RunNow;
use crate::session::lock;

/// What a session is doing, as clients show it.
#[derive(Debug, Default)]
struct Tracker {
    /// From the root's turns: idle, working, or needs attention (a failed turn).
    base: Option<SessionStatus>,
    /// Agents waiting for a permission answer.
    asking: HashMap<String, usize>,
    paused: bool,
    /// The user stopped the session; it stays paused until they resume it.
    stopped: bool,
    /// Background work waiting for a quiet machine: before it started or between steps.
    hold: Option<SessionHold>,
    /// A message waits for the session to be given a model.
    needs_model: bool,
    /// Set by the daemon: the session is watching a pull request, or it merged.
    overlay: Option<(SessionStatus, String)>,
    shown: Option<(SessionStatus, Option<String>)>,
}

impl Tracker {
    fn current(&self) -> (SessionStatus, Option<String>) {
        if self.asking.values().any(|count| *count > 0) {
            return (SessionStatus::NeedsAttention, None);
        }
        if self.stopped {
            return (SessionStatus::Paused, Some("stopped".into()));
        }
        if self.paused {
            return (SessionStatus::Paused, None);
        }
        if self.needs_model {
            return (SessionStatus::NeedsModel, None);
        }
        let base = self.base.unwrap_or(SessionStatus::Idle);
        match (&base, &self.overlay) {
            (SessionStatus::Idle, Some((status, detail))) => (*status, Some(detail.clone())),
            _ => (base, None),
        }
    }
}

/// Shared between a session's handle and its agents.
#[derive(Debug)]
pub struct Controls {
    tracker: Mutex<Tracker>,
    default_policy: ResumePolicy,
    policy: Mutex<Option<ResumePolicy>>,
    wake: Notify,
    /// Released by "run now" while the session waits for a quiet machine.
    run_now: RunNow,
    /// Whether the user stopped the session: agents halt at their next safe point while set.
    /// Follows `work_stopped` and `work_resumed`, so a replayed journal restores it.
    stop: watch::Sender<bool>,
}

impl Controls {
    pub fn new(default_policy: ResumePolicy) -> Self {
        Controls {
            tracker: Mutex::new(Tracker::default()),
            default_policy,
            policy: Mutex::new(None),
            wake: Notify::new(),
            run_now: RunNow::default(),
            stop: watch::Sender::new(false),
        }
    }

    /// Whether the user stopped the session.
    pub fn is_stopped(&self) -> bool {
        *self.stop.borrow()
    }

    /// Follows the stop: `true` while the session is stopped.
    pub(crate) fn stop_signal(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }

    /// Why background work is waiting for a quiet machine, if it is.
    pub fn hold(&self) -> Option<SessionHold> {
        lock(&self.tracker).hold.clone()
    }

    pub(crate) fn run_now(&self) -> &RunNow {
        &self.run_now
    }

    /// Forgets a wait for a quiet machine that a restart ended.
    pub(crate) fn clear_hold(&self) {
        let mut tracker = lock(&self.tracker);
        tracker.hold = None;
        tracker.paused = false;
    }

    pub fn status(&self) -> (SessionStatus, Option<String>) {
        lock(&self.tracker).current()
    }

    /// Updates the status from an event; returns a `status_changed` event when it changed.
    pub(crate) fn observe(&self, event: &AgentEvent) -> Option<SessionEvent> {
        let mut tracker = lock(&self.tracker);
        let root = event.agent_id == imperium_protocol::session::ROOT_AGENT;
        match &event.event {
            SessionEvent::TurnStarted { .. } if root => tracker.base = Some(SessionStatus::Working),
            SessionEvent::TurnCompleted { .. } if root => {
                tracker.base = Some(SessionStatus::Idle);
                tracker.asking.clear();
            }
            SessionEvent::TurnFailed { .. } if root => {
                tracker.base = Some(SessionStatus::NeedsAttention);
                tracker.asking.clear();
                tracker.paused = false;
                tracker.hold = None;
            }
            SessionEvent::PermissionRequested { .. } | SessionEvent::QuestionsAsked { .. } => {
                *tracker.asking.entry(event.agent_id.clone()).or_default() += 1;
            }
            // An agent moving on means its question was answered.
            SessionEvent::ToolStarted { .. } | SessionEvent::ToolFinished { .. } => {
                tracker.asking.remove(&event.agent_id);
            }
            SessionEvent::Paused { reason, cause, .. } => {
                tracker.paused = true;
                if *cause != PauseCause::UsageLimit {
                    tracker.hold = Some(SessionHold {
                        cause: *cause,
                        reason: reason.clone(),
                    });
                }
            }
            SessionEvent::Resumed {} => {
                tracker.paused = false;
                tracker.hold = None;
            }
            SessionEvent::WorkStopped {} if root => {
                tracker.stopped = true;
                self.stop.send_replace(true);
            }
            SessionEvent::WorkResumed {} if root => {
                tracker.stopped = false;
                self.stop.send_replace(false);
            }
            _ => {}
        }
        Self::changed(&mut tracker)
    }

    /// Marks whether a message waits for the session to be given a model.
    pub(crate) fn set_needs_model(&self, needs: bool) -> Option<SessionEvent> {
        let mut tracker = lock(&self.tracker);
        tracker.needs_model = needs;
        Self::changed(&mut tracker)
    }

    /// Sets or clears the daemon's overlay (watching a pull request, or merged).
    pub(crate) fn set_overlay(
        &self,
        overlay: Option<(SessionStatus, String)>,
    ) -> Option<SessionEvent> {
        let mut tracker = lock(&self.tracker);
        tracker.overlay = overlay;
        Self::changed(&mut tracker)
    }

    fn changed(tracker: &mut Tracker) -> Option<SessionEvent> {
        let current = tracker.current();
        if tracker.shown.as_ref() == Some(&current) {
            return None;
        }
        tracker.shown = Some(current.clone());
        Some(SessionEvent::StatusChanged {
            status: current.0,
            detail: current.1,
        })
    }

    pub fn policy(&self) -> ResumePolicy {
        lock(&self.policy)
            .clone()
            .unwrap_or_else(|| self.default_policy.clone())
    }

    /// Overrides the session's policy; `None` returns to the configured default.
    pub fn set_policy(&self, policy: Option<ResumePolicy>) {
        *lock(&self.policy) = policy;
    }

    /// Ends a usage-limit wait now, or a background session's wait for a quiet machine, which
    /// then stops giving way to the load.
    pub fn resume_now(&self) {
        if lock(&self.tracker).hold.is_some() {
            self.run_now.release();
        }
        self.wake.notify_waiters();
    }

    /// Waits until `until`, or until resumed early.
    pub(crate) async fn pause_until(&self, until: DateTime<Local>) {
        let wait = (until - Local::now()).to_std().unwrap_or(Duration::ZERO);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = self.wake.notified() => {}
        }
    }
}

/// When to try again after a usage limit, or `None` to stop. `attempt` counts the waits so
/// far in this turn (0 for the first); `waited` is how long they took.
pub fn next_attempt(
    policy: &ResumePolicy,
    retry_after_s: Option<u64>,
    attempt: u32,
    waited: Duration,
    now: DateTime<Local>,
) -> Option<DateTime<Local>> {
    let every = chrono::Duration::minutes(i64::from(policy.retry_every_minutes.max(1)));
    let until = match policy.mode.as_str() {
        "stop" => return None,
        "retry_every" => now + every,
        // The set time first; if usage has still not reset then, keep retrying.
        "at_time" if attempt == 0 => policy
            .at_time
            .as_deref()
            .and_then(|text| NaiveTime::parse_from_str(text, "%H:%M").ok())
            .and_then(|time| next_local(now, time))
            .unwrap_or(now + every),
        "at_time" => now + every,
        // wait_for_reset: the provider's hint, else the retry interval.
        _ => retry_after_s.map_or(now + every, |seconds| {
            now + chrono::Duration::seconds(i64::try_from(seconds).unwrap_or(i64::MAX / 2) + 1)
        }),
    };
    let limit = Duration::from_secs(u64::from(policy.max_wait_hours) * 3600);
    let next = (until - now).to_std().unwrap_or(Duration::ZERO);
    (waited + next <= limit).then_some(until)
}

/// The next time the clock reads `time`, today or tomorrow.
fn next_local(now: DateTime<Local>, time: NaiveTime) -> Option<DateTime<Local>> {
    let today = now.date_naive().and_time(time);
    let candidate = Local.from_local_datetime(&today).earliest()?;
    if candidate > now {
        Some(candidate)
    } else {
        Local
            .from_local_datetime(&(today + chrono::Duration::days(1)))
            .earliest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: &str) -> ResumePolicy {
        ResumePolicy {
            mode: mode.into(),
            at_time: Some("03:00".into()),
            retry_every_minutes: 5,
            max_wait_hours: 12,
        }
    }

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(2026, 11, 7)
                    .unwrap()
                    .and_hms_opt(hour, minute, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
    }

    #[test]
    fn policies_choose_when_to_retry() {
        let now = at(22, 41);
        assert_eq!(
            next_attempt(&policy("stop"), Some(30), 0, Duration::ZERO, now),
            None
        );
        assert_eq!(
            next_attempt(&policy("wait_for_reset"), Some(120), 0, Duration::ZERO, now),
            Some(now + chrono::Duration::seconds(121))
        );
        assert_eq!(
            next_attempt(&policy("wait_for_reset"), None, 0, Duration::ZERO, now),
            Some(now + chrono::Duration::minutes(5))
        );
        // 03:00 tomorrow first, then every five minutes.
        let first = next_attempt(&policy("at_time"), None, 0, Duration::ZERO, now).unwrap();
        assert_eq!(first.format("%d %H:%M").to_string(), "08 03:00");
        assert_eq!(
            next_attempt(&policy("at_time"), None, 1, Duration::ZERO, first),
            Some(first + chrono::Duration::minutes(5))
        );
        // Past the longest wait, give up.
        let long = Duration::from_secs(12 * 3600);
        assert_eq!(
            next_attempt(&policy("retry_every"), None, 9, long, now),
            None
        );
    }

    #[test]
    fn status_follows_turns_questions_pauses_and_watches() {
        let controls = Controls::new(policy("wait_for_reset"));
        let root = |event| AgentEvent {
            agent_id: "root".into(),
            event,
        };
        assert!(
            controls
                .observe(&root(SessionEvent::TurnStarted { text: "x".into() }))
                .is_some()
        );
        assert_eq!(controls.status().0, SessionStatus::Working);
        controls.observe(&AgentEvent {
            agent_id: "t1".into(),
            event: SessionEvent::PermissionRequested {
                request_id: 1,
                tool: "w".into(),
                summary: "s".into(),
                suggested_rule: None,
            },
        });
        assert_eq!(controls.status().0, SessionStatus::NeedsAttention);
        controls.observe(&AgentEvent {
            agent_id: "t1".into(),
            event: SessionEvent::ToolStarted {
                call_id: "c".into(),
                tool: "w".into(),
                summary: "s".into(),
            },
        });
        assert_eq!(controls.status().0, SessionStatus::Working);
        controls.observe(&root(SessionEvent::Paused {
            until_unix_ms: 0,
            reason: "limit".into(),
            cause: PauseCause::UsageLimit,
        }));
        assert_eq!(controls.status().0, SessionStatus::Paused);
        controls.observe(&root(SessionEvent::Resumed {}));
        controls.observe(&root(SessionEvent::TurnCompleted {
            usage: Default::default(),
            cost_usd: None,
            steps: 1,
        }));
        assert_eq!(controls.status().0, SessionStatus::Idle);
        let watching = controls.set_overlay(Some((SessionStatus::Watching, "o/r#7".into())));
        assert!(matches!(
            watching,
            Some(SessionEvent::StatusChanged {
                status: SessionStatus::Watching,
                ..
            })
        ));
        // Working again hides the overlay; idle shows it.
        controls.observe(&root(SessionEvent::TurnStarted { text: "y".into() }));
        assert_eq!(controls.status().0, SessionStatus::Working);
        controls.observe(&root(SessionEvent::TurnFailed {
            message: "x".into(),
        }));
        assert_eq!(controls.status().0, SessionStatus::NeedsAttention);
    }
}
