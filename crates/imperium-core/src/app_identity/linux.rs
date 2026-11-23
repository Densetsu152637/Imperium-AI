//! Linux: the peer is held by a pidfd from accept onwards, so after the accept a reused process
//! id never stands in for it.
//!
//! At accept the daemon asks the socket for the peer's pidfd (`SO_PEERPIDFD`, kernel 6.5 and
//! later, or a distribution kernel that backported it). Only when the kernel answers that it
//! lacks the option (`ENOPROTOOPT`) does it open one on the `SO_PEERCRED` id (`pidfd_open`, 5.3
//! and later); any other failure refuses the peer, since it means the peer is gone. The fallback
//! refuses a process whose start time (`/proc/<pid>/stat` field 22) is later than the accept,
//! but **on a kernel without the option (before 6.5), a pid reused between the peer's connect
//! and the accept is not detected**: that process started before the accept and passes.
//!
//! Every read goes through `/proc/<pid>` opened as a folder and then confirmed with the pidfd: while the pidfd's process is alive no other process can hold its id, so the folder
//! opened before that confirmation is its own, and reads through it fail once it is gone.
//!
//! Safe wrappers only: `rustix` for the pidfd, `SO_PEERCRED`, `poll` and `/proc`, and `nix`
//! for `SO_PEERPIDFD`, which `rustix` does not offer.

use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, PidfdFlags};

/// How the pidfd was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Via {
    /// From the socket itself: the exact process that connected.
    PeerPidfd,
    /// `pidfd_open` on the id the socket reported, with the start-time check.
    PidfdOpen,
}

/// The process on the other end of a local-socket connection.
#[derive(Debug)]
pub(crate) struct Peer {
    pid: Pid,
    pidfd: OwnedFd,
    /// When the connection was accepted, in clock ticks since boot (the unit of field 22).
    accepted_ticks: u64,
    via: Via,
}

pub(super) fn at_accept(stream: &imperium_ipc::Stream) -> Option<Peer> {
    from_socket(stream, boot_ticks())
}

/// The peer of the connected socket `socket`, accepted at `accepted_ticks`.
fn from_socket(socket: &impl AsFd, accepted_ticks: u64) -> Option<Peer> {
    let credentials = rustix::net::sockopt::socket_peercred(socket).ok()?;
    // A peer whose id cannot be mapped into the daemon's pid namespace reports 0.
    let pid = Pid::from_raw(credentials.pid.as_raw_pid())?;
    let answer = nix::sys::socket::getsockopt(socket, nix::sys::socket::sockopt::PeerPidfd);
    match answer {
        Ok(pidfd) => Some(Peer {
            pid,
            pidfd,
            accepted_ticks,
            via: Via::PeerPidfd,
        }),
        Err(error) if lacks_peer_pidfd(error) => Peer::open(pid, accepted_ticks),
        // The kernel has the option, so EINVAL, ESRCH and the rest mean the peer has exited
        // (and its id may already be another process's): refuse rather than open a pidfd on it.
        Err(_) => None,
    }
}

/// Whether `error` from `SO_PEERPIDFD` means the kernel lacks the option: only `ENOPROTOOPT`,
/// whatever the kernel's version says (a distribution may have backported the option).
fn lacks_peer_pidfd(error: nix::errno::Errno) -> bool {
    error == nix::errno::Errno::ENOPROTOOPT
}

/// Whether the running kernel is `major.minor` or later.
#[cfg(test)]
fn kernel_at_least(major: u32, minor: u32) -> bool {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut parts = release
        .split(|c: char| !c.is_ascii_digit())
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (major, minor)
}

impl Peer {
    /// The `pidfd_open` fallback for process `pid`, accepted at `accepted_ticks`.
    fn open(pid: Pid, accepted_ticks: u64) -> Option<Peer> {
        let pidfd = rustix::process::pidfd_open(pid, PidfdFlags::empty()).ok()?;
        Some(Peer {
            pid,
            pidfd,
            accepted_ticks,
            via: Via::PidfdOpen,
        })
    }

    /// This test process as a peer accepted now.
    #[cfg(test)]
    pub(crate) fn test_peer() -> Peer {
        Peer::open(rustix::process::getpid(), boot_ticks()).unwrap()
    }

    /// `/proc/<pid>` of the peer, opened and then confirmed alive through the pidfd.
    fn proc_dir(&self) -> Result<OwnedFd, String> {
        let path = format!("/proc/{}", self.pid.as_raw_pid());
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let dir = rustix::fs::open(path.as_str(), flags, Mode::empty())
            .map_err(|_| "the calling process has exited".to_string())?;
        if exited(&self.pidfd) {
            return Err("the calling process has exited".into());
        }
        Ok(dir)
    }
}

/// Checks 2 to 4 for `peer`, with `app` the desktop app installed beside `imperium-core`.
pub(super) fn identify(peer: &Peer, app: &Path) -> Result<(), String> {
    let dir = peer.proc_dir()?;
    // The socket's own pidfd is the process that connected; one opened on its id may not be.
    if peer.via == Via::PidfdOpen {
        check_started(&dir, peer.accepted_ticks)?;
    }
    check_exe(&dir, app)?;
    check_namespaces(&dir)?;
    check_environment(&dir)?;
    // The reads went through a folder that fails once the process is gone; confirm it was not
    // gone before the last of them either.
    if exited(&peer.pidfd) {
        return Err("the calling process has exited".into());
    }
    Ok(())
}

/// Whether the pidfd's process has exited (a pidfd polls readable then).
fn exited(pidfd: &OwnedFd) -> bool {
    let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    match rustix::event::poll(&mut fds, Some(&now)) {
        Ok(_) => !fds[0].revents().is_empty(),
        Err(_) => true,
    }
}

/// The time since boot in clock ticks, as `/proc/<pid>/stat` counts a start time (floored, as
/// the kernel floors it).
fn boot_ticks() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    let nanos = u128::try_from(now.tv_sec).unwrap_or(0) * 1_000_000_000
        + u128::try_from(now.tv_nsec).unwrap_or(0);
    let ticks = nanos * u128::from(rustix::param::clock_ticks_per_second()) / 1_000_000_000;
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

fn read_at(dir: &OwnedFd, name: &str) -> Result<Vec<u8>, String> {
    let fd = rustix::fs::openat(dir, name, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .map_err(|error| format!("cannot read the calling process's {name}: {error}"))?;
    let mut bytes = Vec::new();
    std::fs::File::from(fd)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read the calling process's {name}: {error}"))?;
    Ok(bytes)
}

fn link_at(dir: &OwnedFd, name: &str) -> Result<Vec<u8>, String> {
    rustix::fs::readlinkat(dir, name, Vec::new())
        .map(|target| target.into_bytes())
        .map_err(|error| format!("cannot read the calling process's {name}: {error}"))
}

/// Field 22 of a `stat` line: the start time in clock ticks since boot. The second field, the
/// command name, may hold spaces and parentheses, so fields are counted after its last `)`.
fn start_ticks(stat: &[u8]) -> Option<u64> {
    let close = stat.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(&stat[close + 1..]).ok()?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// A process that started after its connection was accepted holds a reused id.
fn check_started(dir: &OwnedFd, accepted_ticks: u64) -> Result<(), String> {
    let started = start_ticks(&read_at(dir, "stat")?)
        .ok_or("cannot read the calling process's start time")?;
    if started > accepted_ticks {
        return Err("the calling process started after its connection: its id was reused".into());
    }
    Ok(())
}

fn check_exe(dir: &OwnedFd, app: &Path) -> Result<(), String> {
    let exe = link_at(dir, "exe")?;
    if exe != app.as_os_str().as_bytes() {
        return Err(format!(
            "the caller ({}) is not the desktop app installed beside imperium-core",
            String::from_utf8_lossy(&exe)
        ));
    }
    Ok(())
}

/// An OS-tier sandbox runs in mount and pid namespaces of its own.
fn check_namespaces(dir: &OwnedFd) -> Result<(), String> {
    for kind in ["mnt", "pid"] {
        let theirs = link_at(dir, &format!("ns/{kind}"))?;
        let ours = std::fs::read_link(format!("/proc/self/ns/{kind}"))
            .map_err(|error| format!("cannot read the daemon's {kind} namespace: {error}"))?;
        if theirs != ours.as_os_str().as_bytes() {
            return Err(format!(
                "the caller runs in another {kind} namespace (a sandbox)"
            ));
        }
    }
    Ok(())
}

fn check_environment(dir: &OwnedFd) -> Result<(), String> {
    let environ = read_at(dir, "environ")?;
    let names = environ.split(|&byte| byte == 0).filter_map(|entry| {
        let name = entry.split(|&byte| byte == b'=').next()?;
        std::str::from_utf8(name).ok()
    });
    match super::refused_variable(names) {
        Some(name) => Err(format!("the caller's environment sets {name}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    use super::*;
    use crate::app_identity::Identify;

    /// `sleep` with an empty environment plus `env`, and its canonical path.
    fn sleeper(env: &[(&str, &str)]) -> (Child, std::path::PathBuf) {
        let path = Path::new("/bin/sleep").canonicalize().unwrap();
        let child = Command::new(&path)
            .arg("30")
            .env_clear()
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Let it reach `sleep` itself (past `exec`).
        std::thread::sleep(Duration::from_millis(200));
        (child, path)
    }

    fn pid_of(child: &Child) -> Pid {
        Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap()
    }

    fn end(mut child: Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_socket_peer_is_held_by_a_pidfd() {
        let (server, _client) = std::os::unix::net::UnixStream::pair().unwrap();
        let peer = from_socket(&server, boot_ticks()).unwrap();
        assert_eq!(peer.pid, rustix::process::getpid());
        let expected = if kernel_at_least(6, 5) {
            Via::PeerPidfd
        } else {
            Via::PidfdOpen
        };
        assert_eq!(peer.via, expected);
        let dir = peer.proc_dir().unwrap();
        check_started(&dir, peer.accepted_ticks).unwrap();
        check_namespaces(&dir).unwrap();
        check_exe(
            &dir,
            &std::env::current_exe().unwrap().canonicalize().unwrap(),
        )
        .unwrap();
        // The test binary is not the desktop app installed beside imperium-core.
        assert!(super::super::InstalledApp.identify(Some(&peer)).is_err());
    }

    #[test]
    fn a_clean_peer_at_the_expected_path_passes_and_another_path_is_refused() {
        let (child, path) = sleeper(&[]);
        let peer = Peer::open(pid_of(&child), boot_ticks()).unwrap();
        identify(&peer, &path).unwrap();
        let error = identify(&peer, &std::env::current_exe().unwrap()).unwrap_err();
        assert!(error.contains("not the desktop app"), "{error}");
        end(child);
    }

    #[test]
    fn an_exited_peer_is_refused_through_its_pidfd() {
        // Pid reuse cannot be forced without privileges (pid_max is large), so this shows the
        // check that covers it: once the process behind the pidfd has exited, the peer is
        // refused whatever now holds its id. Killed but not reaped, `/proc/<pid>` still exists,
        // so only the pidfd tells.
        let (mut child, path) = sleeper(&[]);
        let peer = Peer::open(pid_of(&child), boot_ticks()).unwrap();
        child.kill().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let error = identify(&peer, &path).unwrap_err();
        assert!(error.contains("exited"), "{error}");
        let _ = child.wait();
        assert!(identify(&peer, &path).is_err());
    }

    #[test]
    fn a_peer_that_started_after_the_accept_is_refused() {
        // The fallback's accept time, injected: before the process started.
        let before = boot_ticks();
        std::thread::sleep(Duration::from_millis(50));
        let (child, path) = sleeper(&[]);
        let peer = Peer::open(pid_of(&child), before).unwrap();
        assert_eq!(peer.via, Via::PidfdOpen);
        let error = identify(&peer, &path).unwrap_err();
        assert!(error.contains("reused"), "{error}");
        end(child);
    }

    #[test]
    fn loader_and_webview_variables_in_the_peers_environment_are_refused() {
        for name in ["LD_PRELOAD", "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"] {
            let (child, path) = sleeper(&[(name, "/nonexistent.so")]);
            let peer = Peer::open(pid_of(&child), boot_ticks()).unwrap();
            let error = identify(&peer, &path).unwrap_err();
            assert!(error.contains(name), "{error}");
            end(child);
        }
    }

    #[test]
    fn only_a_kernel_without_the_option_falls_back_to_pidfd_open() {
        use nix::errno::Errno;
        assert!(lacks_peer_pidfd(Errno::ENOPROTOOPT));
        for error in [Errno::EINVAL, Errno::ESRCH, Errno::ENODATA, Errno::EPERM] {
            assert!(!lacks_peer_pidfd(error), "{error}");
        }
    }

    #[test]
    fn a_peer_that_exited_before_the_accept_is_not_reopened_by_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let script = format!(
            "import socket, time
s = socket.socket(socket.AF_UNIX)
s.connect({:?})
time.sleep(30)",
            path.to_str().unwrap()
        );
        let spawned = Command::new("python3")
            .args(["-c", &script])
            .stdin(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            // CI must prove this ran; elsewhere a machine without python3 skips it.
            assert!(
                std::env::var_os("CI").is_none(),
                "python3 is missing on this CI runner"
            );
            eprintln!("skipped: python3 is not available here");
            return;
        };
        let (server, _) = listener.accept().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        if kernel_at_least(6, 5) {
            // Evidence that the kernel has the option: on a reaped peer it answers with a pidfd or
            // an error other than "no such option", so `from_socket` refuses instead of falling
            // back to the reaped peer's id.
            let raw = nix::sys::socket::getsockopt(&server, nix::sys::socket::sockopt::PeerPidfd);
            assert_ne!(raw.err(), Some(nix::errno::Errno::ENOPROTOOPT));
        }
        match from_socket(&server, boot_ticks()) {
            None => {}
            Some(peer) => {
                // Never a pidfd opened on the id on a kernel with SO_PEERPIDFD; a pidfd the
                // socket gives for the exited process (later kernels) is refused when read.
                assert!(
                    !(kernel_at_least(6, 5) && peer.via == Via::PidfdOpen),
                    "a pidfd was opened on a dead peer's id"
                );
                assert!(peer.proc_dir().is_err() || peer.via == Via::PidfdOpen);
            }
        }
    }

    #[test]
    fn the_start_time_is_read_after_the_command_name() {
        let stat = b"42 (a) b (c) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 777 19";
        assert_eq!(start_ticks(stat), Some(777));
    }

    /// The pids of `pid`'s descendants, from every process's parent in `/proc/<pid>/stat`
    /// (readable for other users' processes too, such as `sudo`'s).
    fn descendants(pid: u32) -> Vec<u32> {
        let parents: Vec<(u32, u32)> = std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let child: u32 = entry.file_name().to_str()?.parse().ok()?;
                let stat = std::fs::read(entry.path().join("stat")).ok()?;
                let close = stat.iter().rposition(|&byte| byte == b')')?;
                let rest = std::str::from_utf8(&stat[close + 1..]).ok()?;
                Some((child, rest.split_whitespace().nth(1)?.parse().ok()?))
            })
            .collect();
        let mut found = Vec::new();
        let mut queue = vec![pid];
        while let Some(next) = queue.pop() {
            for &(child, parent) in &parents {
                if parent == next && !found.contains(&child) {
                    found.push(child);
                    queue.push(child);
                }
            }
        }
        found
    }

    /// A `sleep` of this user in new namespaces made by `unshare` with `flags`: unprivileged
    /// (a user namespace) where allowed, else through passwordless `sudo`, dropping back to this
    /// user. `None` when neither can make one.
    fn sleeper_in(flags: &[&str]) -> Option<(Child, Pid)> {
        let uid = rustix::process::getuid().as_raw().to_string();
        let gid = rustix::process::getgid().as_raw().to_string();
        let attempts: [Vec<&str>; 2] = [
            [
                &["unshare", "--user", "--map-root-user"][..],
                flags,
                &["--fork", "sleep", "5"],
            ]
            .concat(),
            [
                &["sudo", "-n", "unshare"][..],
                flags,
                &[
                    "--fork",
                    "setpriv",
                    "--reuid",
                    uid.as_str(),
                    "--regid",
                    gid.as_str(),
                    "--clear-groups",
                    "sleep",
                    "5",
                ],
            ]
            .concat(),
        ];
        for attempt in attempts {
            let Ok(mut child) = Command::new(attempt[0])
                .args(&attempt[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                continue;
            };
            for _ in 0..20 {
                std::thread::sleep(Duration::from_millis(100));
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                let sleeping = descendants(child.id()).into_iter().find(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/comm"))
                        .is_ok_and(|comm| comm.trim() == "sleep")
                });
                if let Some(pid) = sleeping {
                    return Some((child, Pid::from_raw(i32::try_from(pid).ok()?)?));
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        None
    }

    #[test]
    fn a_peer_in_another_mount_or_pid_namespace_is_refused() {
        for (flag, kind) in [("--mount", "mnt"), ("--pid", "pid")] {
            let Some((mut child, pid)) = sleeper_in(&[flag]) else {
                // CI must prove this ran; elsewhere a machine without unshare skips it.
                assert!(
                    std::env::var_os("CI").is_none(),
                    "unshare could not make a {kind} namespace on this CI runner"
                );
                eprintln!("skipped: unshare could not make a {kind} namespace here");
                return;
            };
            let peer = Peer::open(pid, boot_ticks()).unwrap();
            let dir = peer.proc_dir().unwrap();
            let error = check_namespaces(&dir).unwrap_err();
            assert!(error.contains(kind), "{error}");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
