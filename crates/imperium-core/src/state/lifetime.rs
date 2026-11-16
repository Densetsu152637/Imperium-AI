//! The daemon's side of its lifetime: what counts as work, and the monitor that ends a shared
//! daemon nobody uses (the rules are in [`crate::lifetime`]).

use imperium_protocol::initiative::Initiative;
use imperium_protocol::methods::{LeaseKind, LifetimeStatus, WorkInfo, WorkKind};

use super::*;
use crate::lifetime::{Exiting, LeaseGuard};

/// Most names listed in a work reference's detail.
const DETAIL_NAMES: usize = 3;

fn names(all: &[String]) -> String {
    let mut shown = all
        .iter()
        .take(DETAIL_NAMES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if all.len() > DETAIL_NAMES {
        shown.push_str(&format!(" and {} more", all.len() - DETAIL_NAMES));
    }
    shown
}

fn work(kind: WorkKind, all: &[String]) -> Option<WorkInfo> {
    (!all.is_empty()).then(|| WorkInfo {
        kind,
        count: u32::try_from(all.len()).unwrap_or(u32::MAX),
        detail: names(all),
    })
}

impl DaemonState {
    /// Makes the daemon shared, so it exits after `grace` with no lease and no work (`Some`),
    /// or leaves it persistent (`None`), and starts the monitor that decides. Call once, inside
    /// the Tokio runtime, after the background work has started.
    pub fn start_lifetime(self: &Arc<Self>, shared: Option<Duration>) {
        self.lifetime.configure(shared);
        let weak = Arc::downgrade(self);
        let lifetime = Arc::clone(&self.lifetime);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(lifetime.tick_interval()).await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                let reaping = Arc::clone(&state.lifetime);
                if let Ok(gone) = tokio::task::spawn_blocking(move || reaping.reap()).await {
                    for label in gone {
                        tracing::info!(%label, "its process is gone; the lease ended");
                    }
                }
                if state
                    .lifetime
                    .tick(Instant::now(), || !state.work_references().is_empty())
                {
                    tracing::info!("no client and no work is using imperium-core; exiting");
                    state.request_shutdown();
                    return;
                }
            }
        });
    }

    /// Registers a lease held by one connection; it ends when the guard is dropped. Refused
    /// once the daemon has decided to exit.
    pub(crate) fn acquire_lease(
        &self,
        kind: LeaseKind,
        pid: Option<u32>,
        label: &str,
    ) -> Result<LeaseGuard, Exiting> {
        self.lifetime.acquire(kind, pid, label)
    }

    /// Chooses whether the daemon runs until stopped (`true`) or exits when idle (`false`).
    pub fn set_persistent(&self, persistent: bool) {
        self.lifetime.set_persistent(persistent);
    }

    /// Who holds the daemon open and what work does, for `system.lifetime`.
    pub fn lifetime_status(&self) -> LifetimeStatus {
        self.lifetime
            .snapshot(Instant::now(), self.work_references())
    }

    /// Uses `probe(pid)` (the process's start time, or none when it is gone) instead of the
    /// process table (for tests). Call once.
    #[cfg(any(test, feature = "testing"))]
    pub fn use_pid_probe(&self, probe: Arc<dyn Fn(u32) -> Option<u64> + Send + Sync>) {
        self.lifetime.use_probe(probe);
    }

    /// What the daemon does on its own that keeps it up: turns running, schedules with a run to
    /// come, persistent agents with initiative, and GitHub responders.
    ///
    /// A session waiting on a permission or a question is not counted: with no client to answer
    /// it, nothing runs, and its journal is picked up again when a daemon starts.
    pub(crate) fn work_references(&self) -> Vec<WorkInfo> {
        let mut found = Vec::new();

        let turns: Vec<String> = self
            .sessions()
            .iter()
            .filter(|session| {
                matches!(
                    session.status().0,
                    SessionStatus::Working | SessionStatus::Paused
                )
            })
            .map(|session| session.id().to_string())
            .collect();
        found.extend(work(WorkKind::Turn, &turns));

        if let Some(scheduler) = self.scheduler() {
            let schedules: Vec<String> = scheduler
                .list()
                .schedules
                .into_iter()
                .filter(|row| row.enabled && (row.running || row.next_run.is_some()))
                .map(|row| row.id)
                .collect();
            found.extend(work(WorkKind::Schedule, &schedules));
        }

        let agents: Vec<String> = self
            .environments()
            .list()
            .iter()
            .flat_map(|env| {
                env.agents
                    .iter()
                    .filter(|agent| agent.initiative != Initiative::Off)
                    .map(move |agent| format!("{}/{}", env.name, agent.name))
            })
            .collect();
        found.extend(work(WorkKind::Initiative, &agents));

        let settings = self.settings();
        let mut github: Vec<String> = Vec::new();
        if settings.github.autofix.enabled {
            github.extend(
                settings
                    .github
                    .watch
                    .iter()
                    .map(|watch| format!("auto-fix {}", watch.repo)),
            );
        }
        github.extend(
            self.watched()
                .into_iter()
                .map(|(_, repo, number)| format!("{repo}#{number}")),
        );
        found.extend(work(WorkKind::Github, &github));
        found
    }
}
