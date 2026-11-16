//! How long the daemon lives (docs/02-architecture.md#shared-daemon-lifetime-implemented).
//!
//! A daemon started on its own (`imperium-core`, autostart, `imperium daemon start`) is
//! *persistent*: it runs until told to stop. A daemon a client started for itself
//! (`imperium-core --exit-when-idle`) is *shared*: it is kept up by **leases** and by **work**,
//! and exits, through the same path as `system.shutdown`, once it has had neither for a grace
//! period.
//!
//! * A lease is a connection that sent `system.lease`. It ends when the connection closes, and
//!   also when the process it names is gone (a crash or a hang can leave a socket open), which
//!   [`Lifetime::reap`] checks.
//! * Work is what the daemon does on its own: running turns, enabled schedules with a run to
//!   come, persistent agents with initiative, GitHub responders (see
//!   `DaemonState::work_references`).
//!
//! The decision to exit is made under the same lock that grants leases, so a client either gets
//! its lease before the daemon decides, or is told `DAEMON_STOPPING` and starts a new daemon.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use imperium_protocol::methods::{LeaseInfo, LeaseKind, LifetimeMode, LifetimeStatus, WorkInfo};

/// How long a shared daemon waits with no lease and no work unless told otherwise. Long enough
/// that one command after another reuses the daemon.
pub(crate) const DEFAULT_GRACE: Duration = Duration::from_secs(10);
/// How far a process's start time may differ from the one recorded when its lease was taken, in
/// seconds, for rounding; a process that differs by more is another one under a reused id.
const START_SLACK_SECS: u64 = 2;
const LABEL_CHARS: usize = 120;

/// When the process `pid` started (Unix seconds, as the process table says), or `None` when it
/// is gone or a zombie. Only ever compared with an earlier reading of the same table, never
/// with this machine's clock.
pub(crate) fn process_start(pid: u32) -> Option<u64> {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .filter(|process| {
            !matches!(
                process.status(),
                sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
            )
        })
        .map(sysinfo::Process::start_time)
}

type Probe = Arc<dyn Fn(u32) -> Option<u64> + Send + Sync>;

#[derive(Debug)]
pub(crate) struct Lifetime {
    inner: Mutex<Inner>,
    /// Decides whether a lease's process is alive; a fake in tests.
    probe: OnceLock<ProbeFn>,
}

#[cfg_attr(not(any(test, feature = "testing")), allow(dead_code))]
struct ProbeFn(Probe);

impl std::fmt::Debug for ProbeFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Probe")
    }
}

#[derive(Debug)]
struct Inner {
    persistent: bool,
    grace: Duration,
    leases: BTreeMap<u64, Lease>,
    /// Long jobs in progress, by id (see [`Lifetime::begin_job`]).
    jobs: BTreeMap<u64, String>,
    next_id: u64,
    /// When the daemon last found itself with no lease and no work.
    idle_since: Option<Instant>,
    /// The daemon has decided to exit; it takes no more leases.
    exiting: bool,
}

#[derive(Debug, Clone)]
struct Lease {
    kind: LeaseKind,
    pid: Option<u32>,
    label: String,
    since: Instant,
    /// The process's start time when the lease was taken (`None`: the table did not show it).
    started: Option<u64>,
}

/// Why a lease was refused: the daemon is exiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exiting;

/// A lease held by a connection; ends when dropped.
#[derive(Debug)]
pub(crate) struct LeaseGuard {
    lifetime: Arc<Lifetime>,
    id: u64,
}

impl LeaseGuard {
    pub(crate) fn id(&self) -> String {
        format!("lease-{}", self.id)
    }
}

/// A long job in progress; ends when dropped.
#[derive(Debug)]
pub(crate) struct JobGuard {
    lifetime: Arc<Lifetime>,
    id: u64,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.lifetime.lock().jobs.remove(&self.id);
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.lifetime.lock().leases.remove(&self.id);
    }
}

impl Lifetime {
    /// A persistent lifetime: nothing makes the daemon exit until it is made shared.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Lifetime {
            inner: Mutex::new(Inner {
                persistent: true,
                grace: DEFAULT_GRACE,
                leases: BTreeMap::new(),
                jobs: BTreeMap::new(),
                next_id: 1,
                idle_since: None,
                exiting: false,
            }),
            probe: OnceLock::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes the daemon shared (exits after `grace` of idleness) or persistent.
    pub(crate) fn configure(&self, shared: Option<Duration>) {
        let mut inner = self.lock();
        match shared {
            Some(grace) => {
                inner.persistent = false;
                inner.grace = grace;
            }
            None => inner.persistent = true,
        }
    }

    /// Chooses persistent or shared, keeping the grace period. Once the daemon is exiting,
    /// nothing changes.
    pub(crate) fn set_persistent(&self, persistent: bool) {
        let mut inner = self.lock();
        if !inner.exiting {
            inner.persistent = persistent;
        }
    }

    /// Uses `probe` instead of looking at the process table (for tests). Call once.
    #[cfg_attr(not(any(test, feature = "testing")), allow(dead_code))]
    pub(crate) fn use_probe(&self, probe: Probe) {
        let _ = self.probe.set(ProbeFn(probe));
    }

    /// How often the monitor looks: often enough to notice the end of the grace period.
    pub(crate) fn tick_interval(&self) -> Duration {
        (self.lock().grace / 4).clamp(Duration::from_millis(25), Duration::from_secs(1))
    }

    /// Takes a lease for a connection, unless the daemon has decided to exit.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        kind: LeaseKind,
        pid: Option<u32>,
        label: &str,
    ) -> Result<LeaseGuard, Exiting> {
        let pid = pid.filter(|pid| *pid != 0);
        // Read before the lock is taken: it asks the operating system.
        let started = pid.and_then(|pid| self.start_of(pid));
        let mut inner = self.lock();
        if inner.exiting {
            return Err(Exiting);
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner.leases.insert(
            id,
            Lease {
                kind,
                pid,
                label: label
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(LABEL_CHARS)
                    .collect(),
                since: Instant::now(),
                started,
            },
        );
        inner.idle_since = None;
        Ok(LeaseGuard {
            lifetime: Arc::clone(self),
            id,
        })
    }

    /// Ends the leases whose process is gone; returns what they were doing. The process table
    /// is read without the lock held.
    pub(crate) fn reap(&self) -> Vec<String> {
        let candidates: Vec<(u64, u32, Option<u64>)> = self
            .lock()
            .leases
            .iter()
            .filter_map(|(id, lease)| Some((*id, lease.pid?, lease.started)))
            .collect();
        let mut dead = Vec::new();
        let mut learned = Vec::new();
        for (id, pid, recorded) in candidates {
            match (self.start_of(pid), recorded) {
                // Gone.
                (None, _) => dead.push(id),
                // Another process under the same id.
                (Some(now), Some(then)) if now.abs_diff(then) > START_SLACK_SECS => dead.push(id),
                // The table did not show it when the lease was taken; it does now.
                (Some(now), None) => learned.push((id, now)),
                (Some(_), Some(_)) => {}
            }
        }
        let mut inner = self.lock();
        for (id, started) in learned {
            if let Some(lease) = inner.leases.get_mut(&id) {
                lease.started = Some(started);
            }
        }
        dead.into_iter()
            .filter_map(|id| inner.leases.remove(&id))
            .map(|lease| lease.label)
            .collect()
    }

    /// When `pid` started, from the process table or the fake in tests.
    fn start_of(&self, pid: u32) -> Option<u64> {
        match self.probe.get() {
            Some(probe) => (probe.0)(pid),
            None => process_start(pid),
        }
    }

    /// Ends the daemon's willingness to take leases: it is being stopped.
    pub(crate) fn mark_exiting(&self) {
        self.lock().exiting = true;
    }

    /// Records a long job the daemon runs on its own (a folder move); it holds the daemon up
    /// until the guard is dropped.
    pub(crate) fn begin_job(self: &Arc<Self>, label: &str) -> JobGuard {
        let mut inner = self.lock();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.jobs.insert(id, label.to_string());
        inner.idle_since = None;
        JobGuard {
            lifetime: Arc::clone(self),
            id,
        }
    }

    /// The jobs `begin_job` recorded.
    pub(crate) fn jobs(&self) -> Vec<String> {
        self.lock().jobs.values().cloned().collect()
    }

    /// One look at the daemon: returns true when it should exit now, once. `work` says whether
    /// anything the daemon does keeps it up; it is asked only when no lease does.
    pub(crate) fn tick(&self, now: Instant, work: impl FnOnce() -> bool) -> bool {
        {
            let mut inner = self.lock();
            if !inner.leases.is_empty() {
                inner.idle_since = None;
                return false;
            }
        }
        let working = work();
        let mut inner = self.lock();
        if working || !inner.leases.is_empty() {
            inner.idle_since = None;
            return false;
        }
        let since = *inner.idle_since.get_or_insert(now);
        if inner.persistent || inner.exiting || now.saturating_duration_since(since) < inner.grace {
            return false;
        }
        inner.exiting = true;
        true
    }

    /// The daemon as `system.lifetime` reports it, with the work `work` found.
    pub(crate) fn snapshot(&self, now: Instant, work: Vec<WorkInfo>) -> LifetimeStatus {
        let inner = self.lock();
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let idle = (inner.leases.is_empty() && work.is_empty()).then(|| {
            inner
                .idle_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
        });
        LifetimeStatus {
            mode: if inner.persistent {
                LifetimeMode::Persistent
            } else {
                LifetimeMode::Shared
            },
            grace_ms: millis(inner.grace),
            leases: inner
                .leases
                .iter()
                .map(|(id, lease)| LeaseInfo {
                    id: format!("lease-{id}"),
                    kind: lease.kind,
                    pid: lease.pid,
                    label: lease.label.clone(),
                    held_ms: millis(now.saturating_duration_since(lease.since)),
                })
                .collect(),
            work,
            idle_ms: idle.map(millis),
            exits_in_ms: idle
                .filter(|_| !inner.persistent)
                .map(|idle| millis(inner.grace.saturating_sub(idle))),
            exiting: inner.exiting,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    const GRACE: Duration = Duration::from_secs(10);

    fn shared() -> Arc<Lifetime> {
        let lifetime = Lifetime::new();
        lifetime.configure(Some(GRACE));
        lifetime
    }

    fn lease(lifetime: &Arc<Lifetime>, pid: Option<u32>) -> LeaseGuard {
        lifetime
            .acquire(LeaseKind::Cli, pid, "imperium chat")
            .expect("a lease")
    }

    #[test]
    fn a_persistent_daemon_never_exits() {
        let lifetime = Lifetime::new();
        let start = Instant::now();
        assert!(!lifetime.tick(start, || false));
        assert!(!lifetime.tick(start + GRACE * 100, || false));
        let status = lifetime.snapshot(start, Vec::new());
        assert_eq!(status.mode, LifetimeMode::Persistent);
        assert_eq!(status.exits_in_ms, None);
    }

    #[test]
    fn a_shared_daemon_exits_after_the_grace_period_with_no_lease_and_no_work() {
        let lifetime = shared();
        let start = Instant::now();
        assert!(!lifetime.tick(start, || false), "the clock starts here");
        assert!(!lifetime.tick(start + GRACE - Duration::from_millis(1), || false));
        assert!(lifetime.tick(start + GRACE, || false));
        assert!(!lifetime.tick(start + GRACE * 2, || false), "it exits once");
    }

    #[test]
    fn a_lease_holds_the_daemon_and_the_last_one_to_go_starts_the_clock() {
        let lifetime = shared();
        let start = Instant::now();
        let first = lease(&lifetime, None);
        let second = lease(&lifetime, None);
        assert!(!lifetime.tick(start + GRACE * 5, || false));
        drop(first);
        assert!(!lifetime.tick(start + GRACE * 6, || false), "one is left");
        drop(second);
        assert!(
            !lifetime.tick(start + GRACE * 7, || false),
            "the clock starts"
        );
        assert!(lifetime.tick(start + GRACE * 8, || false));
    }

    #[test]
    fn work_holds_the_daemon_without_a_lease_and_restarts_the_clock() {
        let lifetime = shared();
        let start = Instant::now();
        assert!(!lifetime.tick(start, || false));
        assert!(
            !lifetime.tick(start + GRACE * 3, || true),
            "a turn is running"
        );
        assert!(
            !lifetime.tick(start + GRACE * 4, || false),
            "the clock restarts"
        );
        assert!(lifetime.tick(start + GRACE * 5, || false));
    }

    #[test]
    fn work_is_not_asked_while_a_lease_holds_the_daemon() {
        let lifetime = shared();
        let _held = lease(&lifetime, None);
        assert!(!lifetime.tick(Instant::now(), || unreachable!("a lease already holds it")));
    }

    #[test]
    fn once_the_daemon_decides_to_exit_it_takes_no_more_leases() {
        let lifetime = shared();
        let start = Instant::now();
        lifetime.tick(start, || false);
        assert!(lifetime.tick(start + GRACE, || false));
        assert_eq!(
            lifetime.acquire(LeaseKind::Cli, None, "late").unwrap_err(),
            Exiting
        );
        lifetime.set_persistent(true);
        assert!(lifetime.snapshot(start, Vec::new()).exiting);
        assert_eq!(
            lifetime.snapshot(start, Vec::new()).mode,
            LifetimeMode::Shared,
            "an exiting daemon cannot be made persistent"
        );
    }

    #[test]
    fn a_lease_taken_before_the_decision_keeps_the_daemon() {
        let lifetime = shared();
        let start = Instant::now();
        lifetime.tick(start, || false);
        // The lease arrives while the daemon was looking for work.
        let held = std::cell::RefCell::new(None);
        assert!(!lifetime.tick(start + GRACE, || {
            *held.borrow_mut() = Some(lease(&lifetime, None));
            false
        }));
        assert!(held.borrow().is_some());
    }

    #[test]
    fn the_status_lists_leases_and_counts_down() {
        let lifetime = shared();
        let start = Instant::now();
        let held = lifetime
            .acquire(LeaseKind::Desktop, Some(4242), "Imperium\u{7}desktop")
            .unwrap();
        let status = lifetime.snapshot(start, Vec::new());
        assert_eq!(status.leases.len(), 1);
        assert_eq!(status.leases[0].id, held.id());
        assert_eq!(status.leases[0].kind, LeaseKind::Desktop);
        assert_eq!(status.leases[0].pid, Some(4242));
        assert_eq!(
            status.leases[0].label, "Imperiumdesktop",
            "controls are dropped"
        );
        assert_eq!((status.idle_ms, status.exits_in_ms), (None, None));
        drop(held);

        lifetime.tick(start, || false);
        let status = lifetime.snapshot(start + Duration::from_secs(4), Vec::new());
        assert_eq!(status.idle_ms, Some(4000));
        assert_eq!(status.exits_in_ms, Some(6000));
        assert_eq!(status.grace_ms, 10_000);
    }

    #[test]
    fn a_lease_whose_process_is_gone_is_reaped() {
        let lifetime = shared();
        // The fake process table: the start time of process 7, or none once it is gone.
        let start = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let seen = Arc::clone(&start);
        lifetime.use_probe(Arc::new(move |_| {
            match seen.load(std::sync::atomic::Ordering::SeqCst) {
                0 => None,
                started => Some(started),
            }
        }));
        let _with_pid = lease(&lifetime, Some(7));
        let _without = lease(&lifetime, None);
        assert!(lifetime.reap().is_empty());
        start.store(0, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(lifetime.reap(), ["imperium chat"]);
        let status = lifetime.snapshot(Instant::now(), Vec::new());
        assert_eq!(
            status.leases.len(),
            1,
            "a lease with no pid is left to its connection"
        );
    }

    /// A process that lives a few seconds, as a child of this one.
    fn child() -> std::process::Child {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("ping");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a child process")
    }

    #[test]
    fn a_reused_process_id_is_told_by_its_start_time_not_the_wall_clock() {
        let lifetime = shared();
        let start = Arc::new(std::sync::atomic::AtomicU64::new(5_000));
        let seen = Arc::clone(&start);
        lifetime.use_probe(Arc::new(move |_| {
            Some(seen.load(std::sync::atomic::Ordering::SeqCst))
        }));
        let _held = lease(&lifetime, Some(7));
        // A table clock that disagrees with this machine's by years changes nothing, and a
        // reading within the slack is the same process.
        start.store(5_002, std::sync::atomic::Ordering::SeqCst);
        assert!(lifetime.reap().is_empty());
        // Another process under the id: its start time is shifted past the slack.
        start.store(9_000, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(lifetime.reap(), ["imperium chat"]);
    }

    #[test]
    fn the_process_table_tells_a_live_process_from_a_dead_one() {
        let own = process_start(std::process::id()).expect("this process is in the table");
        assert!(own > 0);
        let mut child = child();
        let pid = child.id();
        // Give the process table a moment to show a new process.
        std::thread::sleep(Duration::from_millis(300));
        assert!(process_start(pid).is_some());
        child.kill().expect("end the child");
        child.wait().expect("reap the child");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(process_start(pid), None);
    }
}
