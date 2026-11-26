//! Ending a terminal's processes on Unix, by session (decision D5).
//!
//! A job-control shell on a PTY puts each background job in a process group of its own, so
//! killing the shell's group would miss `sleep 60 &`. The PTY's child leads a new session (its
//! session id is its pid) and every process it starts stays in that session unless it calls
//! `setsid`. So the daemon lists the processes of that session, sends `SIGHUP` (and `SIGCONT`) to
//! each, waits 2 s, then runs the freeze-and-kill loop of [`crate::freeze`] for at most 30 s.
//!
//! The session leader (the supervisor on Linux, the shell on macOS) is never stopped, only
//! killed: stopping the supervisor would stop the very process that is ending the tree.
//!
//! On Linux the child is the supervisor, which also ends processes that left the session; this
//! sweep then only catches what survives it (for example after the supervisor itself was
//! killed with `SIGKILL`). The session id is the child's pid, so the sweep also covers
//! processes whose parent died: they keep their session. A Linux member is signalled through a
//! pidfd opened after the listing, only if its start time and session are still what the listing
//! saw, so the pid window between listing and signal is closed there; macOS uses `kill`, a
//! window of microseconds.
//!
//! Accepted window: the session id is a pid that is free once the child is reaped. Reuse of
//! that pid as the id of an unrelated new session within the seconds this takes would need the
//! pid space (4 million on Linux) to wrap, so no further check is made.

use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, getpid};

use crate::freeze::{self, Candidate, Delivery};

/// How long `SIGHUP` gets before the freeze and kill.
const HUP_WAIT: Duration = Duration::from_secs(2);
/// The most the freeze and kill goes on: the daemon's thread must not wait for ever.
const KILL_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// A process of the session. `start` is its start time on Linux (checked again before each
/// signal) and 0 on macOS.
struct Member {
    pid: Pid,
    start: u64,
    /// Live and not stopped, so it may still fork.
    running: bool,
    /// In `D` state: killed at once.
    disk_wait: bool,
}

/// The live members of session `sid`. A zombie thread-group leader with live threads is live.
#[cfg(target_os = "linux")]
fn members(sid: Pid) -> Vec<Member> {
    use crate::supervisor::{is_live, is_running, read_stat};

    let me = getpid();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let raw: i32 = entry.file_name().to_str()?.parse().ok()?;
            let pid = Pid::from_raw(raw)?;
            if pid == me {
                return None;
            }
            let stat = read_stat(raw)?;
            if stat.session != sid.as_raw_pid() || !is_live(raw, stat.state) {
                return None;
            }
            Some(Member {
                pid,
                start: stat.start,
                running: is_running(raw, stat.state),
                disk_wait: stat.state == 'D',
            })
        })
        .collect()
}

/// A process's session id; `rustix`'s `getsid` panics on the 0 some kernels report for a session
/// outside our pid namespace, hence `catch_unwind`.
#[cfg(not(target_os = "linux"))]
fn session_of(pid: Pid) -> Option<i32> {
    std::panic::catch_unwind(|| {
        rustix::process::getsid(Some(pid))
            .ok()
            .map(|session| session.as_raw_pid())
    })
    .ok()?
}

/// The live (not zombie) members of session `sid`.
#[cfg(not(target_os = "linux"))]
fn members(sid: Pid) -> Vec<Member> {
    use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};

    let me = getpid();
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    system
        .processes()
        .values()
        .filter(|process| process.status() != ProcessStatus::Zombie)
        .filter_map(|process| {
            let pid = Pid::from_raw(i32::try_from(process.pid().as_u32()).ok()?)?;
            if pid == me || session_of(pid)? != sid.as_raw_pid() {
                return None;
            }
            Some(Member {
                pid,
                start: 0,
                running: process.status() != ProcessStatus::Stop,
                disk_wait: process.status() == ProcessStatus::UninterruptibleDiskSleep,
            })
        })
        .collect()
}

/// Signals a process of the session.
#[cfg(target_os = "linux")]
fn signal_member(pid: i32, start: u64, signal: Signal, sid: Pid) -> Delivery {
    use crate::supervisor::read_stat;
    use rustix::io::Errno;
    use rustix::process::{PidfdFlags, pidfd_open, pidfd_send_signal};

    let Some(pid_value) = Pid::from_raw(pid) else {
        return Delivery::Gone;
    };
    let Ok(pidfd) = pidfd_open(pid_value, PidfdFlags::empty()) else {
        return Delivery::Gone;
    };
    if !read_stat(pid).is_some_and(|stat| stat.start == start && stat.session == sid.as_raw_pid()) {
        return Delivery::Gone;
    }
    match pidfd_send_signal(&pidfd, signal) {
        Ok(()) => Delivery::Sent,
        Err(Errno::PERM) => Delivery::Denied,
        Err(_) => Delivery::Gone,
    }
}

#[cfg(not(target_os = "linux"))]
fn signal_member(pid: i32, _start: u64, signal: Signal, _sid: Pid) -> Delivery {
    use rustix::io::Errno;

    let Some(pid) = Pid::from_raw(pid) else {
        return Delivery::Gone;
    };
    match rustix::process::kill_process(pid, signal) {
        Ok(()) => Delivery::Sent,
        Err(Errno::PERM) => Delivery::Denied,
        Err(_) => Delivery::Gone,
    }
}

/// Ends every process of session `sid`: `SIGHUP`, then freeze and `SIGKILL`. Whether none was
/// left (a process that refuses signals, or outlasts the cap, makes it `false`).
///
/// With `spare_leader` the session's leader is neither signalled nor waited for: on Linux it is
/// the supervisor, still ending its own tree inside its cap, and killing it would hand what it
/// has not ended yet to init.
pub(crate) fn sweep(sid: Pid, spare_leader: bool) -> bool {
    let members = |sid: Pid| {
        let mut found = members(sid);
        if spare_leader {
            found.retain(|member| member.pid != sid);
        }
        found
    };
    let first = members(sid);
    if first.is_empty() {
        return true;
    }
    for member in &first {
        signal_member(member.pid.as_raw_pid(), member.start, Signal::HUP, sid);
        // A stopped process gets its `SIGHUP` only once it continues.
        signal_member(member.pid.as_raw_pid(), member.start, Signal::CONT, sid);
    }
    let deadline = Instant::now() + HUP_WAIT;
    while Instant::now() < deadline {
        if members(sid).is_empty() {
            return true;
        }
        sleep(POLL);
    }
    let leader = sid.as_raw_pid();
    let outcome = freeze::run(
        || {
            members(sid)
                .into_iter()
                .map(|member| Candidate {
                    key: (member.pid.as_raw_pid(), member.start),
                    running: member.running,
                    disk_wait: member.disk_wait,
                    leader: member.pid.as_raw_pid() == leader,
                })
                .collect()
        },
        |candidate, signal| signal_member(candidate.key.0, candidate.key.1, signal, sid),
        Instant::now() + KILL_WAIT,
    );
    outcome.left == 0 && outcome.denied.is_empty()
}
