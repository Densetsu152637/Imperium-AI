//! Whether a local connection comes from a process the daemon did not start.
//!
//! Trust in a mod and "approved at a prompt" are grants only a person at a terminal may make
//! (`mods.install` with `trusted` or `interactive`). The daemon cannot see the terminal, but on
//! the local socket the operating system tells it which process is calling (`SO_PEERCRED`, or
//! `GetNamedPipeClientProcessId`), and every command an agent runs (`run_command`, background
//! commands, hooks) is a descendant of the daemon. So a grant is honoured only when the caller
//! is on the local socket and
//!
//! 1. is neither the daemon nor a descendant of it (the parent chain is walked), and
//! 2. does not carry [`SPAWNED_MARK`] in its environment, which the daemon sets on every shell it
//!    starts and which survives a child that detaches from its parent (an orphan has no daemon
//!    in its chain, but keeps its environment).
//!
//! Not a boundary against an agent that is trying to evade it: an orphan that also clears its
//! environment, or an agent that talks to the daemon some other way, is not caught. It stops the
//! mistake and the plain case, an agent's own shell running `imperium mods install --yes`. A
//! connection over loopback TCP has no peer process to ask about, so it never makes a grant.

use imperium_tools::SPAWNED_MARK;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Most parents walked, in case the table is cyclic.
const MAX_DEPTH: usize = 128;

/// Whether the process `peer` may make grants: see the module documentation.
pub(crate) fn may_grant(peer: u32) -> bool {
    may_grant_from(peer, std::process::id())
}

/// [`may_grant`] for a daemon with process id `daemon`.
pub(crate) fn may_grant_from(peer: u32, daemon: u32) -> bool {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
    );
    let mark = format!("{SPAWNED_MARK}=");
    let mut pid = Pid::from_u32(peer);
    // A process the table does not show (it exited) cannot be vouched for.
    let Some(process) = system.process(pid) else {
        return false;
    };
    if process
        .environ()
        .iter()
        .any(|entry| entry.to_string_lossy().starts_with(&mark))
    {
        return false;
    }
    for _ in 0..MAX_DEPTH {
        if pid.as_u32() == daemon {
            return false;
        }
        match system.process(pid).and_then(|process| process.parent()) {
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

    #[test]
    fn a_descendant_of_the_daemon_may_not_grant() {
        // This test process stands in for the daemon; its child is what an agent's shell is.
        let mut agent = child(false);
        assert!(!may_grant_from(agent.id(), std::process::id()));
        // The daemon itself is not a person's terminal either.
        assert!(!may_grant_from(std::process::id(), std::process::id()));
        let _ = agent.kill();
        let _ = agent.wait();
    }

    #[test]
    fn a_process_outside_the_daemons_tree_may_grant() {
        // The same child, judged by a "daemon" that is some other process: it is not below it.
        let mut person = child(false);
        assert!(may_grant_from(person.id(), u32::MAX - 1));
        let _ = person.kill();
        let _ = person.wait();
    }

    #[test]
    fn a_marked_process_may_not_grant_even_outside_the_tree() {
        // An orphan keeps its environment, so the mark still gives it away.
        let mut orphan = child(true);
        // Give the process table a moment to show the new process's environment.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!may_grant_from(orphan.id(), u32::MAX - 1));
        let _ = orphan.kill();
        let _ = orphan.wait();
    }

    #[test]
    fn a_process_that_is_gone_cannot_be_vouched_for() {
        let mut gone = child(false);
        let pid = gone.id();
        let _ = gone.kill();
        let _ = gone.wait();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!may_grant_from(pid, u32::MAX - 1));
    }
}
