//! Ending a terminal's processes on Unix, by session (decision D5).
//!
//! A job-control shell on a PTY puts each background job in a process group of its own, so
//! killing the shell's group would miss `sleep 60 &`. The PTY's child leads a new session (its
//! session id is its pid) and every process it starts stays in that session unless it calls
//! `setsid`. So the daemon lists the processes of that session, sends `SIGHUP` to each of their
//! groups, waits 2 s, and `SIGKILL`s what is left.
//!
//! On Linux the child is the supervisor, which also ends processes that left the session; this
//! sweep then only catches what survives it (for example after the supervisor itself was
//! killed with `SIGKILL`). The session id is the child's pid, so the sweep also covers
//! processes whose parent died: they keep their session.
//!
//! Accepted window: the session id is a pid that is free once the child is reaped. Reuse of
//! that pid as the id of an unrelated new session within the seconds this takes would need the
//! pid space (4 million on Linux) to wrap, so no further check is made.

use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, getpgid, getpid, getsid, kill_process, kill_process_group};
use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

/// How long `SIGHUP` gets before `SIGKILL`.
const HUP_WAIT: Duration = Duration::from_secs(2);
/// How long `SIGKILL` is repeated for a process that keeps forking.
const KILL_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

/// The live (not zombie) processes whose session is `sid`.
fn members(sid: Pid) -> Vec<Pid> {
    let me = getpid();
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    system
        .processes()
        .values()
        .filter(|process| process.status() != ProcessStatus::Zombie)
        .filter_map(|process| i32::try_from(process.pid().as_u32()).ok())
        .filter_map(Pid::from_raw)
        .filter(|&pid| pid != me && getsid(Some(pid)).is_ok_and(|found| found == sid))
        .collect()
}

/// Ends every process of session `sid`: `SIGHUP` to their groups, then `SIGKILL`.
pub(crate) fn sweep(sid: Pid) {
    let first = members(sid);
    if first.is_empty() {
        return;
    }
    let mut groups: Vec<Pid> = first
        .iter()
        .filter_map(|&pid| getpgid(Some(pid)).ok())
        .collect();
    groups.sort_by_key(|group| group.as_raw_pid());
    groups.dedup();
    for group in groups {
        let _ = kill_process_group(group, Signal::HUP);
        // A stopped process gets its `SIGHUP` only once it continues.
        let _ = kill_process_group(group, Signal::CONT);
    }
    let deadline = Instant::now() + HUP_WAIT;
    while Instant::now() < deadline {
        if members(sid).is_empty() {
            return;
        }
        sleep(POLL);
    }
    let deadline = Instant::now() + KILL_WAIT;
    while Instant::now() < deadline {
        let left = members(sid);
        if left.is_empty() {
            return;
        }
        for pid in left {
            let _ = kill_process(pid, Signal::KILL);
        }
        sleep(POLL);
    }
}
