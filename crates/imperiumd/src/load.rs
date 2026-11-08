//! The daemon's load view: whether the machine is `quiet`, `busy`, or `saturated`, so work that
//! schedules and project agents start gives way to the user's own sessions.
//!
//! A monitor samples what the daemon already knows (the user's sessions with a turn running, the
//! slots and queue of the local models, recent swaps) plus the machine's CPU use and available
//! memory every few seconds. The level rises at once and falls only after the machine has looked
//! calmer for `load.settle_s`, so the pause between two of the user's messages is not "quiet".
//!
//! - **Saturated**: the user's model calls queue for a slot, models are being swapped out, or
//!   memory is nearly gone, or the CPU is pegged. Background model calls wait.
//! - **Busy**: a session of the user's has a turn running, or the CPU is heavily used or memory
//!   is low. Background sessions pause at their next step boundary and scheduled background
//!   work waits to start.
//! - **Quiet**: none of that.
//!
//! The CPU is not counted while a background session is working, since the daemon cannot tell
//! its own load from the user's. The user's keyboard and mouse idle time is not used (no safe OS
//! interface in this codebase, which forbids `unsafe`); the user's sessions stand in for it.

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use imperium_config::LoadSettings;
use imperium_core::{Load, LoadView};

use crate::DaemonState;

/// What the daemon saw at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    /// The user's sessions with a turn running.
    pub interactive_sessions: u32,
    /// Background sessions with a turn running.
    pub background_sessions: u32,
    /// Local model slots held by the user's calls.
    pub interactive_calls: u32,
    /// The user's calls waiting for a local model slot.
    pub interactive_queued: u32,
    /// Local models swapped out in the last minute.
    pub swaps_last_min: u32,
    /// Overall CPU use, if measured.
    pub cpu_pct: Option<f32>,
    /// Available system memory, if measured.
    pub free_ram_mb: Option<u64>,
}

/// The level a sample calls for, before any settling.
pub fn classify(sample: &Sample, settings: &LoadSettings) -> Load {
    // A background session's own tools and local model load the CPU too.
    let cpu = sample
        .cpu_pct
        .filter(|_| sample.background_sessions == 0)
        .unwrap_or(0.0);
    let free_ram_mb = sample.free_ram_mb.unwrap_or(u64::MAX);
    if sample.interactive_queued > 0
        || (settings.saturated_swaps_per_min > 0
            && sample.swaps_last_min >= settings.saturated_swaps_per_min)
        || cpu >= settings.saturated_cpu_pct
        || free_ram_mb < settings.saturated_free_ram_mb
    {
        Load::Saturated
    } else if sample.interactive_sessions > 0
        || sample.interactive_calls > 0
        || cpu >= settings.busy_cpu_pct
        || free_ram_mb < settings.busy_free_ram_mb
    {
        Load::Busy
    } else {
        Load::Quiet
    }
}

#[derive(Debug)]
struct Tracked {
    level: Load,
    /// Since when samples have called for a lower level than `level`.
    calmer_since: Option<Instant>,
}

/// Turns samples into the level background work sees.
#[derive(Debug)]
pub struct LoadMonitor {
    settings: LoadSettings,
    tracked: Mutex<Tracked>,
}

impl LoadMonitor {
    /// Starts quiet.
    pub fn new(settings: LoadSettings) -> Self {
        LoadMonitor {
            settings,
            tracked: Mutex::new(Tracked {
                level: Load::Quiet,
                calmer_since: None,
            }),
        }
    }

    /// Takes a sample at `now`; returns the level after it.
    pub fn observe(&self, sample: &Sample, now: Instant) -> Load {
        let wanted = classify(sample, &self.settings);
        let mut tracked = self.tracked.lock().unwrap_or_else(PoisonError::into_inner);
        if wanted >= tracked.level {
            tracked.level = wanted;
            tracked.calmer_since = None;
        } else {
            let since = *tracked.calmer_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= Duration::from_secs(self.settings.settle_s) {
                tracked.level = wanted;
                tracked.calmer_since = None;
            }
        }
        tracked.level
    }
}

impl LoadView for LoadMonitor {
    fn level(&self) -> Load {
        self.tracked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .level
    }
}

/// A load view set by hand, for tests.
#[derive(Debug, Default)]
pub struct ManualLoad(Mutex<Load>);

impl ManualLoad {
    pub fn new(level: Load) -> Arc<Self> {
        Arc::new(ManualLoad(Mutex::new(level)))
    }

    pub fn set(&self, level: Load) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = level;
    }
}

impl LoadView for ManualLoad {
    fn level(&self) -> Load {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Reads the machine and the daemon's own activity every `load.sample_s` and feeds the monitor,
/// for as long as the daemon lives.
pub fn start_sampling(daemon: &Arc<DaemonState>) {
    let monitor = daemon.load_monitor();
    let every = Duration::from_secs(daemon.settings().load.sample_s.max(1));
    let daemon: Weak<DaemonState> = Arc::downgrade(daemon);
    tokio::spawn(async move {
        let mut system = sysinfo::System::new();
        let mut ticks = tokio::time::interval(every);
        loop {
            ticks.tick().await;
            let Some(daemon) = daemon.upgrade() else {
                return;
            };
            system.refresh_cpu_usage();
            system.refresh_memory();
            let (interactive_sessions, background_sessions) = daemon.working_sessions();
            let activity = daemon.local_activity();
            let sample = Sample {
                interactive_sessions,
                background_sessions,
                interactive_calls: activity.interactive_in_use,
                interactive_queued: activity.interactive_queued,
                swaps_last_min: daemon.recent_swaps(),
                cpu_pct: Some(system.global_cpu_usage()),
                free_ram_mb: Some(system.available_memory() / (1024 * 1024)),
            };
            let before = monitor.level();
            let after = monitor.observe(&sample, Instant::now());
            if before != after {
                tracing::info!(
                    from = before.label(),
                    to = after.label(),
                    ?sample,
                    "load level changed"
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> LoadSettings {
        LoadSettings::default()
    }

    #[test]
    fn the_users_sessions_make_the_machine_busy_and_a_queue_saturates_it() {
        let quiet = Sample::default();
        assert_eq!(classify(&quiet, &settings()), Load::Quiet);
        let working = Sample {
            interactive_sessions: 1,
            ..quiet
        };
        assert_eq!(classify(&working, &settings()), Load::Busy);
        let queued = Sample {
            interactive_queued: 1,
            ..working
        };
        assert_eq!(classify(&queued, &settings()), Load::Saturated);
        let swapping = Sample {
            swaps_last_min: 3,
            ..quiet
        };
        assert_eq!(classify(&swapping, &settings()), Load::Saturated);
    }

    #[test]
    fn cpu_and_memory_count_unless_background_work_is_the_load() {
        let hot = Sample {
            cpu_pct: Some(80.0),
            free_ram_mb: Some(16_000),
            ..Sample::default()
        };
        assert_eq!(classify(&hot, &settings()), Load::Busy);
        let pegged = Sample {
            cpu_pct: Some(99.0),
            ..hot
        };
        assert_eq!(classify(&pegged, &settings()), Load::Saturated);
        // Our own background session may be what loads the CPU.
        let ours = Sample {
            background_sessions: 1,
            ..pegged
        };
        assert_eq!(classify(&ours, &settings()), Load::Quiet);
        let low_memory = Sample {
            free_ram_mb: Some(1000),
            ..Sample::default()
        };
        assert_eq!(classify(&low_memory, &settings()), Load::Busy);
        let no_memory = Sample {
            free_ram_mb: Some(100),
            ..Sample::default()
        };
        assert_eq!(classify(&no_memory, &settings()), Load::Saturated);
    }

    #[test]
    fn the_level_rises_at_once_and_falls_after_the_machine_settles() {
        let monitor = LoadMonitor::new(settings());
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let busy = Sample {
            interactive_sessions: 1,
            ..Sample::default()
        };
        let calm = Sample::default();
        assert_eq!(monitor.observe(&calm, at(0)), Load::Quiet);
        assert_eq!(monitor.observe(&busy, at(5)), Load::Busy);
        // The turn ended, but a pause between messages is not quiet yet.
        assert_eq!(monitor.observe(&calm, at(10)), Load::Busy);
        assert_eq!(monitor.observe(&calm, at(60)), Load::Busy);
        // Busy again before settling: the clock starts over.
        assert_eq!(monitor.observe(&busy, at(65)), Load::Busy);
        assert_eq!(monitor.observe(&calm, at(70)), Load::Busy);
        assert_eq!(monitor.observe(&calm, at(129)), Load::Busy);
        assert_eq!(monitor.observe(&calm, at(131)), Load::Quiet);
        assert_eq!(monitor.level(), Load::Quiet);
    }
}
