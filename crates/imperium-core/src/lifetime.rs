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
/// How much later than a lease was taken its process may have started, in seconds, for clock
/// rounding; a process that started after that is a new one under a reused process id.
const START_SLACK_SECS: u64 = 2;
const LABEL_CHARS: usize = 120;

/// Whether the process `pid` is running and is the one that took a lease at `since_unix`
/// (Unix seconds): one that started later reuses the id of a process that is gone.
pub(crate) fn process_alive(pid: u32, since_unix: u64) -> bool {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).is_some_and(|process| {
        !matches!(
            process.status(),
            sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
        ) && process.start_time() <= since_unix.saturating_add(START_SLACK_SECS)
    })
}

type Probe = Arc<dyn Fn(u32, u64) -> bool + Send + Sync>;

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
    since_unix: u64,
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
                pid: pid.filter(|pid| *pid != 0),
                label: label
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(LABEL_CHARS)
                    .collect(),
                since: Instant::now(),
                since_unix: imperium_api::time::unix_secs(),
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
        let candidates: Vec<(u64, u32, u64)> = self
            .lock()
            .leases
            .iter()
            .filter_map(|(id, lease)| Some((*id, lease.pid?, lease.since_unix)))
            .collect();
        let probe = self.probe.get().map(|probe| Arc::clone(&probe.0));
        let dead: Vec<u64> = candidates
            .into_iter()
            .filter(|(_, pid, since)| match &probe {
                Some(probe) => !probe(*pid, *since),
                None => !process_alive(*pid, *since),
            })
            .map(|(id, ..)| id)
            .collect();
        let mut inner = self.lock();
        dead.into_iter()
            .filter_map(|id| inner.leases.remove(&id))
            .map(|lease| lease.label)
            .collect()
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
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let seen = Arc::clone(&alive);
        lifetime.use_probe(Arc::new(move |_, _| {
            seen.load(std::sync::atomic::Ordering::SeqCst)
        }));
        let _with_pid = lease(&lifetime, Some(7));
        let _without = lease(&lifetime, None);
        assert!(lifetime.reap().is_empty());
        alive.store(false, std::sync::atomic::Ordering::SeqCst);
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
    fn the_process_table_tells_a_live_process_from_a_dead_one() {
        let now = imperium_api::time::unix_secs();
        assert!(process_alive(std::process::id(), now));
        let mut child = child();
        let pid = child.id();
        // Give the process table a moment to show a new process.
        std::thread::sleep(Duration::from_millis(300));
        assert!(process_alive(pid, now));
        assert!(
            !process_alive(pid, now.saturating_sub(3600)),
            "a process that started after the lease is another one with a reused id"
        );
        child.kill().expect("end the child");
        child.wait().expect("reap the child");
        std::thread::sleep(Duration::from_millis(200));
        assert!(!process_alive(pid, now));
    }
}
