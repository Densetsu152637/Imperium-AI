//! The one long-lived thread every terminal is started from.
//!
//! A child that a thread forked is tied to that *thread* by some lifetimes (a parent-death
//! signal), and a tokio blocking-pool thread is retired at will. So every spawn, on every OS,
//! runs on this plain `std::thread`, which lives as long as the process.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread;

use crate::SpawnError;

type Job = Box<dyn FnOnce() + Send>;

struct Spawner {
    jobs: Mutex<Sender<Job>>,
}

static SPAWNER: OnceLock<Result<Spawner, String>> = OnceLock::new();

fn start() -> Result<Spawner, String> {
    let (jobs, queue) = channel::<Job>();
    thread::Builder::new()
        .name("imperium-pty-spawner".into())
        .spawn(move || {
            for job in queue {
                // A panicking spawn must not end the thread every later terminal needs.
                let _ = catch_unwind(AssertUnwindSafe(job));
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(Spawner {
        jobs: Mutex::new(jobs),
    })
}

/// Runs `work` on the spawner thread and returns what it returns.
pub(crate) fn run<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SpawnError> {
    let spawner = SPAWNER.get_or_init(start).as_ref().map_err(|error| {
        SpawnError::Start(format!("could not start the spawner thread: {error}"))
    })?;
    let (reply, answer) = channel();
    let job: Job = Box::new(move || {
        let _ = reply.send(work());
    });
    spawner
        .jobs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .send(job)
        .map_err(|_| SpawnError::SpawnerGone)?;
    answer.recv().map_err(|_| SpawnError::SpawnerGone)
}
