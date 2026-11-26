//! macOS: the peer is the audit token the socket reports at accept (`LOCAL_PEERTOKEN`). A token
//! names one process, including its pid version, so it never stands for a process that reuses
//! the id. Everything comes from the token, or is bracketed by it:
//!
//! * the code object of the process is looked up by its token
//!   (`SecCodeCopyGuestWithAttributes`), which fails once that process has gone; its on-disk
//!   path (`SecCodeCopyPath`, the executable or its bundle) must be the app beside
//!   `imperium-core`;
//! * `sandbox_check_by_audit_token` must say it is not sandboxed, and its signature must not
//!   carry the App Sandbox entitlement;
//! * its executable and environment are read by pid (`sysinfo`), and the token is looked up again
//!   afterwards: while the token's process is alive no other process holds its pid, so what was
//!   read was its own. The environment may set no `DYLD_*`, `WEBVIEW2_*` or `LD_*` override.
//!   `dyld` removes `DYLD_*` variables from a restricted (hardened or setuid) process at start,
//!   so for such a process the hardened runtime's library validation is what holds instead.

use std::path::Path;
use std::str::FromStr;

use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use security_framework::os::macos::code_signing::{
    Flags, GuestAttributes, SecCode, SecRequirement,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// The process on the other end of a local-socket connection, by its audit token.
#[derive(Debug)]
pub(crate) struct Peer {
    token: [u32; 8],
}

pub(super) fn at_accept(stream: &imperium_ipc::Stream) -> Option<Peer> {
    from_socket(stream)
}

fn from_socket(socket: &impl std::os::fd::AsFd) -> Option<Peer> {
    let token =
        nix::sys::socket::getsockopt(socket, nix::sys::socket::sockopt::LocalPeerToken).ok()?;
    Some(Peer { token: token.val })
}

impl Peer {
    /// This test process as a peer.
    #[cfg(test)]
    pub(crate) fn test_peer() -> Peer {
        let (one, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        from_socket(&one).unwrap()
    }

    /// The process id inside the token (`audit_token_to_pid`).
    fn pid(&self) -> u32 {
        self.token[5]
    }

    /// The code object of the token's process; fails once that process has gone.
    fn code(&self) -> Result<SecCode, String> {
        let bytes: Vec<u8> = self
            .token
            .iter()
            .flat_map(|word| word.to_ne_bytes())
            .collect();
        let data = CFData::from_buffer(&bytes);
        let mut attributes = GuestAttributes::new();
        attributes.set_audit_token(data.as_concrete_TypeRef());
        SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE)
            .map_err(|_| "the calling process has exited".to_string())
    }
}

/// Checks 2 to 4 for `peer`, with `app` the desktop app installed beside `imperium-core`.
pub(super) fn identify(peer: &Peer, app: &Path) -> Result<(), String> {
    let code = peer.code()?;
    let on_disk = code
        .path(Flags::NONE)
        .ok()
        .and_then(|url| url.to_path())
        .and_then(|path| path.canonicalize().ok())
        .ok_or("cannot read where the calling process's code is")?;
    // The executable itself, or the bundle whose executable it is.
    let bundled = on_disk
        .join("Contents/MacOS")
        .join(app.file_name().unwrap_or_default());
    if on_disk != app && bundled != app {
        return Err(format!(
            "the caller ({}) is not the desktop app installed beside imperium-core",
            on_disk.display()
        ));
    }
    match imperium_ipc::is_sandboxed(peer.token) {
        Some(false) => {}
        Some(true) => return Err("the caller runs in a sandbox".into()),
        None => return Err("cannot tell whether the caller runs in a sandbox".into()),
    }
    let sandbox =
        SecRequirement::from_str(r#"entitlement["com.apple.security.app-sandbox"] exists"#)
            .map_err(|error| error.to_string())?;
    if code.check_validity(Flags::NONE, &sandbox).is_ok() {
        return Err("the caller carries the App Sandbox entitlement".into());
    }
    check_by_pid(peer.pid(), app)?;
    // The token's process is still alive, so the pid read above was its own.
    peer.code()?;
    Ok(())
}

/// The executable and environment of process `pid`.
fn check_by_pid(pid: u32, app: &Path) -> Result<(), String> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_environ(UpdateKind::Always),
    );
    let process = system
        .process(pid)
        .ok_or("the calling process has exited")?;
    let names: Vec<String> = process
        .environ()
        .iter()
        .filter_map(|entry| {
            let entry = entry.to_string_lossy();
            entry.split('=').next().map(str::to_string)
        })
        .collect();
    // An empty answer is what an unreadable environment looks like: refuse it.
    if names.is_empty() {
        return Err("cannot read the calling process's environment".into());
    }
    if let Some(name) = super::refused_variable(names.iter().map(String::as_str)) {
        return Err(format!("the caller's environment sets {name}"));
    }
    let exe = process.exe().and_then(|exe| exe.canonicalize().ok());
    if exe.as_deref() != Some(app) {
        return Err(
            "the caller's executable is not the desktop app installed beside imperium-core".into(),
        );
    }
    Ok(())
}

/// Check 7 applies on Windows only.
pub(super) fn may_count(_: crate::plugin_records::ApprovalKind) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::process::{Child, Command, Stdio};

    use super::*;
    use crate::app_identity::{Identify, InstalledApp};

    fn python() -> Option<std::path::PathBuf> {
        let output = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .ok()?;
        let path = String::from_utf8(output.stdout).ok()?;
        std::path::PathBuf::from(path.trim()).canonicalize().ok()
    }

    /// A `python3` (behind `prefix`, if any) connected to a socket of ours, with `env`, and the
    /// accepted end. `None` (outside CI) when `python3` or the prefix is missing.
    fn connected(
        prefix: &[&str],
        env: &[(&str, &str)],
    ) -> Option<(Child, std::os::unix::net::UnixStream)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        let script = format!(
            "import socket, time\ns = socket.socket(socket.AF_UNIX)\ns.connect({:?})\ntime.sleep(30)",
            path.to_str().unwrap()
        );
        let mut command: Vec<&str> = prefix.to_vec();
        command.extend(["python3", "-c", &script]);
        let spawned = Command::new(command[0])
            .args(&command[1..])
            // Only what is asked for: the test runner's own environment holds `DYLD_*` paths.
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .spawn();
        let Ok(child) = spawned else {
            assert!(
                std::env::var_os("CI").is_none(),
                "{} is missing on this CI runner",
                command[0]
            );
            eprintln!("skipped: {} is not available here", command[0]);
            return None;
        };
        let (server, _) = listener.accept().unwrap();
        Some((child, server))
    }

    fn end(mut child: Child) {
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_socket_peer_is_this_process_and_not_the_app() {
        let peer = Peer::test_peer();
        assert_eq!(peer.pid(), std::process::id());
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        let path = peer
            .code()
            .unwrap()
            .path(Flags::NONE)
            .unwrap()
            .to_path()
            .unwrap();
        assert_eq!(path.canonicalize().unwrap(), exe);
        assert!(InstalledApp::DESKTOP.identify(Some(&peer)).is_err());
    }

    /// A child that is killed and reaped when dropped, so no test leaves one behind.
    struct Reaped(Child);

    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_clean_peer_at_its_path_passes_and_another_path_is_refused() {
        // `/usr/bin/nc`, a platform binary that every macOS has, connected to a socket of ours
        // with a 30-second idle limit and a stdin held open, so it neither leaves early nor hangs.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let nc = Reaped(
            Command::new("/usr/bin/nc")
                .args(["-w", "30", "-U"])
                .arg(&path)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let server = loop {
            match listener.accept() {
                Ok((server, _)) => break server,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline, "nc did not connect");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => panic!("{error}"),
            }
        };
        let peer = from_socket(&server).unwrap();
        assert_eq!(peer.pid(), nc.0.id());
        assert_eq!(imperium_ipc::is_sandboxed(peer.token), Some(false));
        identify(&peer, Path::new("/usr/bin/nc")).unwrap();
        let error = identify(&peer, &std::env::current_exe().unwrap()).unwrap_err();
        assert!(error.contains("not the desktop app"), "{error}");
        drop(nc);
    }

    #[test]
    fn loader_and_webview_overrides_in_the_peers_environment_are_refused() {
        // `python3` here is not a restricted process, so `dyld` leaves `DYLD_*` in place.
        for name in [
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "DYLD_FALLBACK_LIBRARY_PATH",
        ] {
            let Some((child, server)) = connected(&[], &[(name, "/nonexistent")]) else {
                return;
            };
            let peer = from_socket(&server).unwrap();
            let error = check_by_pid(peer.pid(), &python().unwrap()).unwrap_err();
            assert!(error.contains(name), "{error}");
            end(child);
        }
    }

    #[test]
    fn a_sandboxed_peer_is_refused() {
        let Some((child, server)) =
            connected(&["sandbox-exec", "-p", "(version 1)(allow default)"], &[])
        else {
            return;
        };
        let peer = from_socket(&server).unwrap();
        assert_eq!(imperium_ipc::is_sandboxed(peer.token), Some(true));
        assert_eq!(
            imperium_ipc::is_sandboxed(Peer::test_peer().token),
            Some(false)
        );
        end(child);
    }

    #[test]
    fn an_exited_peer_is_refused_by_its_token() {
        let Some((mut child, server)) = connected(&[], &[]) else {
            return;
        };
        let peer = from_socket(&server).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let error = identify(&peer, &python().unwrap()).unwrap_err();
        assert!(error.contains("exited"), "{error}");
    }
}
