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
//!
//! **The nested-PID-namespace rule.** Before any of this, a daemon that runs in a nested PID
//! namespace (a container, distrobox, toolbox: its own `NSpid` line in `/proc/self/status` has
//! more than one entry) refuses every grant. There a process can double-fork so that it is
//! re-parented to the namespace's PID 1, which carries no mark and has no `sshd` above it, and
//! the walk would pass. The rule reads only the kernel's answer: no setting or environment
//! variable changes it, and a status it cannot read refuses (fails closed). It applies to making
//! grants and approvals, not to reading ones recorded earlier. The app-identity check of plugin
//! approvals and the auto-merge challenge applies the same rule ([`refuse_nested`]).

use imperium_tools::SPAWNED_MARK;
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

/// Most parents walked, in case the table is cyclic.
const MAX_DEPTH: usize = 128;
/// How much later than the accept time a peer may have started, in seconds, for clock rounding.
const START_SLACK_SECS: u64 = 2;

/// Why a grant or an approval is refused in a nested PID namespace.
pub(crate) const NESTED_NAMESPACE: &str = "the daemon runs in a nested PID namespace (a \
     container, distrobox or toolbox), where the process that asks cannot be traced to a person: \
     grants and approvals are refused there; run the daemon on the host to make them";

/// The start-up warning of a server-mode daemon in the host's PID namespace.
pub(crate) const HOST_NAMESPACE_WARNING: &str = "running in the host's PID namespace: the structural \
     refusal of grants does not apply; only server mode refuses them";

/// Whether the text of `/proc/self/status` says the process is in a nested PID namespace: its
/// `NSpid` line lists more than one id. A missing line, or one with no id, counts as nested
/// (fail closed).
pub(crate) fn nested_pid_namespace(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("NSpid:"))
        .is_none_or(|ids| ids.split_whitespace().count() != 1)
}

/// Reads the daemon's own process status for the nested-namespace rule. The grant paths take
/// one, so tests can inject the kernel's answer.
pub trait NamespaceReader: Send + Sync {
    /// The text of `/proc/self/status`; `Ok(None)` on a platform without PID namespaces.
    fn status(&self) -> std::io::Result<Option<String>>;
}

/// The real reader: `/proc/self/status` on Linux, nothing elsewhere.
pub struct ProcSelf;

impl NamespaceReader for ProcSelf {
    fn status(&self) -> std::io::Result<Option<String>> {
        if cfg!(target_os = "linux") {
            std::fs::read_to_string("/proc/self/status").map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Where the daemon runs, as far as PID namespaces go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Namespace {
    /// A platform without PID namespaces.
    NotApplicable,
    /// The `NSpid` line has one entry: the host's namespace, or no container.
    Single,
    /// More than one entry, or a status that could not be read or has no `NSpid` line.
    Nested,
}

pub(crate) fn namespace(reader: &dyn NamespaceReader) -> Namespace {
    match reader.status() {
        Ok(None) => Namespace::NotApplicable,
        Ok(Some(status)) if !nested_pid_namespace(&status) => Namespace::Single,
        Ok(Some(_)) | Err(_) => Namespace::Nested,
    }
}

/// The guard at the top of every grant and app-identity approval: refused in a nested PID
/// namespace.
pub(crate) fn refuse_nested(reader: &dyn NamespaceReader) -> Result<(), String> {
    match namespace(reader) {
        Namespace::Nested => Err(NESTED_NAMESPACE.to_string()),
        Namespace::NotApplicable | Namespace::Single => Ok(()),
    }
}

/// The warning a daemon logs once at start: server mode in a single-entry PID namespace (a
/// container run with `pid: host`, or no container), where only server mode refuses grants.
pub fn host_namespace_warning(server: bool, reader: &dyn NamespaceReader) -> Option<&'static str> {
    (server && namespace(reader) == Namespace::Single).then_some(HOST_NAMESPACE_WARNING)
}

/// Whether the process `peer`, whose connection was accepted at `accepted_at` (Unix seconds),
/// may make grants: see the module documentation.
pub(crate) fn may_grant(peer: u32, accepted_at: u64) -> bool {
    may_grant_in(&ProcSelf, peer, std::process::id(), Some(accepted_at))
}

/// [`may_grant`] with the namespace read by `reader`: the nested-namespace rule first, then the
/// walk of [`may_grant_from`].
pub(crate) fn may_grant_in(
    reader: &dyn NamespaceReader,
    peer: u32,
    daemon: u32,
    accepted_at: Option<u64>,
) -> bool {
    refuse_nested(reader).is_ok() && may_grant_from(peer, daemon, accepted_at)
}

/// The walk of [`may_grant`] for a daemon with process id `daemon`; `accepted_at` of `None`
/// skips the check that the peer did not start after its connection.
pub(crate) fn may_grant_from(peer: u32, daemon: u32, accepted_at: Option<u64>) -> bool {
    // PID 0 is what a peer outside the daemon's PID namespace reads as (the host's `sshd`
    // forwarding into a container): nobody to vouch for.
    if peer == 0 {
        return false;
    }
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

    /// A reader that answers `status` as the kernel would have; `None` cannot be read.
    struct Status(Option<&'static str>);

    impl NamespaceReader for Status {
        fn status(&self) -> std::io::Result<Option<String>> {
            match self.0 {
                Some(text) => Ok(Some(text.to_string())),
                None => Err(std::io::Error::other("unreadable")),
            }
        }
    }

    /// No PID namespaces on this platform.
    struct NoNamespaces;

    impl NamespaceReader for NoNamespaces {
        fn status(&self) -> std::io::Result<Option<String>> {
            Ok(None)
        }
    }

    const HOST: &str = "Name:\tcat\nPid:\t812\nNSpid:\t812\nNSpgid:\t812\n";
    const CONTAINER: &str = "Name:\tcat\nPid:\t9021\nNSpid:\t9021\t7\nNSpgid:\t9021\t7\n";

    #[test]
    fn the_nspid_line_decides_whether_the_namespace_is_nested() {
        assert!(!nested_pid_namespace(HOST));
        assert!(nested_pid_namespace(CONTAINER));
        // Three levels (a container in a container).
        assert!(nested_pid_namespace("NSpid:\t40\t12\t1\n"));
        // No line, or one with no id: fail closed.
        assert!(nested_pid_namespace("Name:\tcat\nPid:\t812\n"));
        assert!(nested_pid_namespace("NSpid:\n"));
        assert!(nested_pid_namespace(""));
        // A line that only looks alike is not the one.
        assert!(nested_pid_namespace("NSpgid:\t812\n"));
    }

    #[test]
    fn a_nested_namespace_refuses_a_grant_the_walk_would_accept() {
        let mut person = child(false);
        settle();
        // The walk alone accepts this process (it is outside the stand-in daemon's tree)...
        assert!(may_grant_in(
            &Status(Some(HOST)),
            person.id(),
            u32::MAX - 1,
            None
        ));
        assert!(may_grant_in(&NoNamespaces, person.id(), u32::MAX - 1, None));
        // ...and the nested-namespace rule refuses it first, as it does a status it cannot read
        // or one without the line.
        for reader in [
            Status(Some(CONTAINER)),
            Status(None),
            Status(Some("Name:\tcat\n")),
        ] {
            assert!(!may_grant_in(&reader, person.id(), u32::MAX - 1, None));
        }
        let _ = person.kill();
        let _ = person.wait();
    }

    /// The rule reads only the kernel's answer: a process started with `IMPERIUM_*` variables
    /// that try to turn server mode off or name another namespace sees the same `NSpid` line as
    /// this one, and nothing in the configuration is an input to it.
    #[cfg(target_os = "linux")]
    #[test]
    fn imperium_variables_leave_the_namespace_answer_unchanged() {
        let output = Command::new("cat")
            .arg("/proc/self/status")
            .env("IMPERIUM_DAEMON__SERVER", "false")
            .env("IMPERIUM_ISOLATION__TIER", "policy")
            .env("IMPERIUM_NSPID", "1")
            .output()
            .unwrap();
        let theirs = nested_pid_namespace(&String::from_utf8_lossy(&output.stdout));
        assert_eq!(theirs, namespace(&ProcSelf) == Namespace::Nested);
    }

    #[test]
    fn server_mode_in_the_hosts_namespace_warns_and_nothing_else_does() {
        let single = Status(Some(HOST));
        assert_eq!(
            host_namespace_warning(true, &single),
            Some(HOST_NAMESPACE_WARNING)
        );
        assert_eq!(host_namespace_warning(false, &single), None);
        assert_eq!(host_namespace_warning(true, &Status(Some(CONTAINER))), None);
        assert_eq!(host_namespace_warning(true, &NoNamespaces), None);
    }

    #[test]
    fn a_peer_reported_as_pid_0_is_refused() {
        // What a peer outside the daemon's PID namespace (the host's sshd) reads as.
        assert!(!may_grant_from(0, u32::MAX - 1, None));
        assert!(!may_grant_in(&NoNamespaces, 0, u32::MAX - 1, None));
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
