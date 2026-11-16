//! Background work giving way to the user: waiting for a quiet machine.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use imperium_api::{Load, LoadView};
use tokio::sync::Notify;
use tokio::time::Instant;

/// How often a waiting task looks at the load again.
const POLL: Duration = Duration::from_secs(2);

/// What a session's background work waits on: the load view of the daemon that started it.
#[derive(Debug, Clone)]
pub struct Background {
    pub load: std::sync::Arc<dyn LoadView>,
}

/// A person's "run it now": once released, the work stops giving way to the load.
#[derive(Debug, Default)]
pub struct RunNow {
    released: AtomicBool,
    wake: Notify,
}

impl RunNow {
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    pub fn released(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }
}

/// Why [`wait_for_quiet`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waited {
    /// The machine is quiet.
    Quiet,
    /// A person released the wait.
    RunNow,
    /// The limit passed first.
    TimedOut,
}

/// Waits until `view` reports `Quiet`, `run_now` is released, or `limit` passes (no limit if
/// `None`). Returns at once when none of that needs waiting for.
pub async fn wait_for_quiet(
    view: &dyn LoadView,
    run_now: &RunNow,
    limit: Option<Duration>,
) -> Waited {
    let deadline = limit.map(|limit| Instant::now() + limit);
    loop {
        // Listen before looking, so a release between the two is not missed.
        let released = run_now.wake.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        if run_now.released() {
            return Waited::RunNow;
        }
        if view.level() == Load::Quiet {
            return Waited::Quiet;
        }
        let now = Instant::now();
        let wait = match deadline {
            Some(deadline) if now >= deadline => return Waited::TimedOut,
            Some(deadline) => POLL.min(deadline - now),
            None => POLL,
        };
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = released => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug)]
    struct Fixed(Mutex<Load>);

    impl LoadView for Fixed {
        fn level(&self) -> Load {
            *self.0.lock().unwrap()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn waits_for_quiet_for_a_release_or_for_the_limit() {
        let view = Arc::new(Fixed(Mutex::new(Load::Busy)));
        let run_now = Arc::new(RunNow::default());
        // The limit passes first.
        let waited = wait_for_quiet(&*view, &run_now, Some(Duration::from_secs(60))).await;
        assert_eq!(waited, Waited::TimedOut);
        // The machine turns quiet.
        let quiet = Arc::clone(&view);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            *quiet.0.lock().unwrap() = Load::Quiet;
        });
        assert_eq!(wait_for_quiet(&*view, &run_now, None).await, Waited::Quiet);
        // A person releases it while the machine is busy.
        *view.0.lock().unwrap() = Load::Saturated;
        let release = Arc::clone(&run_now);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            release.release();
        });
        assert_eq!(wait_for_quiet(&*view, &run_now, None).await, Waited::RunNow);
        // Once released it stays released.
        assert_eq!(wait_for_quiet(&*view, &run_now, None).await, Waited::RunNow);
    }
}
