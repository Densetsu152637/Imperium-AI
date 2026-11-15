//! Whether a local connection comes from a process the daemon did not start.
//!
//! Trust in a mod and "approved at a prompt" are grants only a person at a terminal may make
//! (`mods.install` with `trusted` or `interactive`). The daemon cannot see the terminal, but on
//! the local socket the operating system tells it which process is calling (`SO_PEERCRED`, or
//! `GetNamedPipeClientProcessId`), and every command an agent runs (`run_command`, background
//! commands, hooks, the OS sandbox and container runners) is a descendant of the daemon and
//! carries [`SPAWNED_MARK`]. So a grant is honoured only when the caller is on the local socket
//! and the check below passes. It **fails closed**: anything it cannot establish is a refusal.
//!
//! * The peer must still be in the process table, not a zombie or dead, and must have started
//!   no later than the moment its connection was accepted (otherwise the id was reused by a new
//!   process).
//! * Its environment must be readable and non-empty (an unreadable environment, as of an
//!   elevated terminal, is refused: run the command from a terminal of your own privilege).
//! * Neither the peer nor any ancestor walked is the daemon, carries [`SPAWNED_MARK`], or is
//!   `sshd` (a command run over SSH, including `ssh localhost`, is refused: run it at the
//!   machine's own terminal).
//!
//! The walk follows each parent until the table shows none. On Windows a parent that has exited
//! is no longer linked, so the chain can end early; the mark in the peer's own environment,
//! which survives the parent exiting, is what covers that case.
//!
//! Not a boundary against an agent that is trying to evade it: one that detaches a process and
//! clears its environment, or reaches the daemon some other way, is not caught. It stops the
//! mistake and the plain case, an agent's shell running the CLI.

use imperium_tools::SPAWNED_MARK;
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

/// Most parents walked, in case the table is cyclic.
const MAX_DEPTH: usize = 128;
/// How much later than the accept time a peer may have started, in seconds, for clock rounding.
const START_SLACK_SECS: u64 = 2;

/// Whether the process `peer`, whose connection was accepted at `accepted_at` (Unix seconds),
/// may make grants: see the module documentation.
pub(crate) fn may_grant(peer: u32, accepted_at: u64) -> bool {
    may_grant_from(peer, std::process::id(), Some(accepted_at))
}

/// [`may_grant`] for a daemon with process id `daemon`; `accepted_at` of `None` skips the
/// check that the peer did not start after its connection.
pub(crate) fn may_grant_from(peer: u32, daemon: u32, accepted_at: Option<u64>) -> bool {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
    );
    let mark = format!("{SPAWNED_MARK}=");
    let marked = |process: &sysinfo::Process| {
        process
            .environ()
            .iter()
            .any(|entry| entry.to_string_lossy().starts_with(&mark))
    };
    let mut pid = Pid::from_u32(peer);
    // The peer itself: present, alive, started before it connected, with a readable environment.
    let Some(process) = system.process(pid) else {
        return false;
    };
    if matches!(
        process.status(),
        ProcessStatus::Zombie | ProcessStatus::Dead
    ) {
        return false;
    }
    if accepted_at.is_some_and(|at| process.start_time() > at.saturating_add(START_SLACK_SECS)) {
        return false;
    }
    if process.environ().is_empty() || marked(process) {
        return false;
    }
    for _ in 0..MAX_DEPTH {
        if pid.as_u32() == daemon {
            return false;
        }
        let Some(process) = system.process(pid) else {
            return true;
        };
        if marked(process)
            || process
                .name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with("sshd")
        {
            return false;
        }
        match process.parent() {
            Some(parent) if parent != pid => pid = parent,
            _ => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    /// A process that lives a few seconds, as a child of this one.
    fn child(marked: bool) -> std::process::Child {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("ping");
            command.args(["-n", "6", "127.0.0.1"]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("5");
            command
        };
        if marked {
            command.env(SPAWNED_MARK, "1");
        } else {
            // Whoever runs these tests (an agent's shell, say) may carry the mark itself.
            command.env_remove(SPAWNED_MARK);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn settle() {
        // Give the process table a moment to show a new process and its environment.
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    #[test]
    fn a_descendant_of_the_daemon_may_not_grant() {
        // This test process stands in for the daemon; its child is what an agent's shell is.
        let mut agent = child(false);
        settle();
        assert!(!may_grant_from(agent.id(), std::process::id(), None));
        // The daemon itself is not a person's terminal either.
        assert!(!may_grant_from(
            std::process::id(),
            std::process::id(),
            None
        ));
        let _ = agent.kill();
        let _ = agent.wait();
    }

    #[test]
    fn a_process_outside_the_daemons_tree_may_grant() {
        // The same child, judged by a "daemon" that is some other process: it is not below it.
        let mut person = child(false);
        settle();
        assert!(may_grant_from(person.id(), u32::MAX - 1, None));
        let _ = person.kill();
        let _ = person.wait();
    }

    #[test]
    fn a_marked_process_may_not_grant_even_outside_the_tree() {
        // An orphan keeps its environment, so the mark still gives it away.
        let mut orphan = child(true);
        settle();
        assert!(!may_grant_from(orphan.id(), u32::MAX - 1, None));
        let _ = orphan.kill();
        let _ = orphan.wait();
    }

    #[test]
    fn a_process_that_started_after_its_connection_is_a_reused_id() {
        let mut late = child(false);
        settle();
        // Accepted a minute before it started: the id was reused.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(!may_grant_from(late.id(), u32::MAX - 1, Some(now - 60)));
        assert!(may_grant_from(late.id(), u32::MAX - 1, Some(now + 60)));
        let _ = late.kill();
        let _ = late.wait();
    }

    #[test]
    fn a_process_that_is_gone_cannot_be_vouched_for() {
        let mut gone = child(false);
        let pid = gone.id();
        let _ = gone.kill();
        let _ = gone.wait();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!may_grant_from(pid, u32::MAX - 1, None));
    }

    /// A child that was killed and not waited for stays a zombie until reaped.
    #[cfg(unix)]
    #[test]
    fn a_zombie_cannot_be_vouched_for() {
        let mut zombie = child(false);
        let pid = zombie.id();
        settle();
        zombie.kill().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!may_grant_from(pid, u32::MAX - 1, None));
        let _ = zombie.wait();
    }
}
