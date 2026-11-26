//! Ending a terminal's processes on Unix, by session (decision D5).
//!
//! A job-control shell on a PTY puts each background job in a process group of its own, so
//! killing the shell's group would miss `sleep 60 &`. The PTY's child leads a new session (its
//! session id is its pid) and every process it starts stays in that session unless it calls
//! `setsid`. So the daemon lists the processes of that session, sends `SIGHUP` (and `SIGCONT`) to
//! each, waits 2 s, then freezes (`SIGSTOP`, repeated until none runs: a stopped process cannot
//! fork) and `SIGKILL`s them, for at most 30 s.
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

/// How long `SIGHUP` gets before the freeze and kill.
const HUP_WAIT: Duration = Duration::from_secs(2);
/// The most the freeze and kill repeats: the daemon's thread must not wait for ever.
const KILL_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// A process of the session.
struct Member {
    pid: Pid,
    /// Live and not stopped, so it may still fork.
    running: bool,
    /// The start time the listing saw (Linux: checked again before each signal).
    #[cfg(target_os = "linux")]
    start: u64,
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
                running: is_running(raw, stat.state),
                start: stat.start,
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
                running: process.status() != ProcessStatus::Stop,
            })
        })
        .collect()
}

/// Signals a member.
#[cfg(target_os = "linux")]
fn signal_member(member: &Member, signal: Signal, sid: Pid) {
    use crate::supervisor::read_stat;
    use rustix::process::{PidfdFlags, pidfd_open, pidfd_send_signal};

    let Ok(pidfd) = pidfd_open(member.pid, PidfdFlags::empty()) else {
        return;
    };
    if read_stat(member.pid.as_raw_pid())
        .is_some_and(|stat| stat.start == member.start && stat.session == sid.as_raw_pid())
    {
        let _ = pidfd_send_signal(&pidfd, signal);
    }
}

#[cfg(not(target_os = "linux"))]
fn signal_member(member: &Member, signal: Signal, _sid: Pid) {
    let _ = rustix::process::kill_process(member.pid, signal);
}

/// Ends every process of session `sid`: `SIGHUP`, then freeze and `SIGKILL`.
pub(crate) fn sweep(sid: Pid) {
    let first = members(sid);
    if first.is_empty() {
        return;
    }
    for member in &first {
        signal_member(member, Signal::HUP, sid);
        // A stopped process gets its `SIGHUP` only once it continues.
        signal_member(member, Signal::CONT, sid);
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
        // Freeze what runs and look again at once; kill only when nothing can fork.
        let running: Vec<&Member> = left.iter().filter(|member| member.running).collect();
        if !running.is_empty() {
            for member in running {
                signal_member(member, Signal::STOP, sid);
            }
            continue;
        }
        for member in &left {
            signal_member(member, Signal::KILL, sid);
        }
        sleep(Duration::from_millis(1));
    }
}
