//! macOS has no OS memory limit for a process (Darwin does not enforce `RLIMIT_DATA`, plan C1,
//! F4), so the worker's memory is bounded only by Monty's limit and the `monty-alloc` hard limit
//! inside the worker, which hold while no escape has happened. This watchdog is the daemon-side
//! backstop the stage 1a review suggested: it samples the worker's physical footprint and kills
//! it once it passes the ceiling (twice `script_memory_mb`, as the OS limits elsewhere). The
//! program then ends as out of memory.
//!
//! The footprint (`ri_phys_footprint` from `proc_pid_rusage`) is what macOS itself charges a
//! process for (Activity Monitor's "Memory", the jetsam limit): it counts dirty memory that was
//! compressed or swapped out, which the resident size does not. A worker could otherwise keep its
//! resident size under the ceiling while the compressor holds the rest.
//!
//! Reading another process's memory and signalling it are C calls, so this module, and only this
//! one on macOS, allows `unsafe`.
#![allow(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::task::JoinHandle;

/// How often the footprint is sampled.
const INTERVAL: Duration = Duration::from_millis(50);

/// A running watchdog; it stops when dropped.
#[derive(Debug)]
pub(crate) struct Watchdog {
    tripped: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Watchdog {
    /// Watches process `pid`, a child the daemon has not reaped (so the id cannot be reused while
    /// it is watched), and kills it once its physical footprint passes `ceiling` bytes.
    pub(crate) fn start(pid: u32, ceiling: u64) -> Self {
        let tripped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&tripped);
        let task = tokio::spawn(async move {
            let Ok(pid) = libc::pid_t::try_from(pid) else {
                return;
            };
            loop {
                match footprint_bytes(pid) {
                    Some(bytes) if bytes > ceiling => {
                        flag.store(true, Ordering::SeqCst);
                        // SAFETY: a signal to the watched child, whose id is still its own.
                        unsafe { libc::kill(pid, libc::SIGKILL) };
                        return;
                    }
                    Some(_) => {}
                    // The process is gone.
                    None => return,
                }
                tokio::time::sleep(INTERVAL).await;
            }
        });
        Self { tripped, task }
    }

    /// Whether the watchdog killed the worker.
    pub(crate) fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The physical footprint of `pid` (`ri_phys_footprint`), or `None` when it cannot be read (the
/// process is gone).
pub(crate) fn footprint_bytes(pid: libc::pid_t) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `info` is a buffer for the `RUSAGE_INFO_V2` structure the call fills in; the call
    // takes it as an opaque `rusage_info_t` out-pointer.
    let status = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: the call succeeded and filled the whole structure.
    Some(unsafe { info.assume_init() }.ri_phys_footprint)
}
