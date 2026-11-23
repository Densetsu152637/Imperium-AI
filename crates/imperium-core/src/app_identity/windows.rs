//! Windows: the client of the pipe is opened at accept (`GetNamedPipeClientProcessId`, then
//! `OpenProcess`) and its handle is held for the connection, in `imperium_ipc::ClientProcess`,
//! which keeps the Win32 calls (and their `unsafe`) out of this crate. A handle refers to its own
//! process until it is closed, never to one that reuses the id, so every later check goes through
//! it: the process must not have exited, must have been created no later than the accept, and
//! must run the app installed beside `imperium-core`.
//!
//! The peer's environment is **not** read here (that would mean reading another process's
//! memory); the desktop app checks its own environment for `WEBVIEW2_*` and loader overrides at
//! start (stage 8b). Check 7 (U7) is [`release_gate`]: no approval but a debug build's `dev` one
//! counts unless this is a release build compiled with the signing job's constant and running
//! from under the per-machine Program Files folder.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use imperium_ipc::ClientProcess;

use crate::plugin_records::ApprovalKind;

/// Set at compile time only by the release signing job's build step; never read at run time.
const SIGNED_BUILD: Option<&str> = option_env!("IMPERIUM_SIGNED_BUILD");

/// The process on the other end of a local-socket connection.
#[derive(Debug)]
pub(crate) struct Peer {
    process: ClientProcess,
    accepted: SystemTime,
}

pub(super) fn at_accept(stream: &imperium_ipc::Stream) -> Option<Peer> {
    let accepted = SystemTime::now();
    Some(Peer {
        process: imperium_ipc::client_process(stream)?,
        accepted,
    })
}

impl Peer {
    /// This test process as a peer accepted now.
    #[cfg(test)]
    pub(crate) fn test_peer() -> Peer {
        Peer {
            process: ClientProcess::open(std::process::id()).unwrap(),
            accepted: SystemTime::now(),
        }
    }
}

/// Checks 2 and 3 for `peer`, with `app` the desktop app installed beside `imperium-core`.
pub(super) fn identify(peer: &Peer, app: &Path) -> Result<(), String> {
    let exited = || "the calling process has exited".to_string();
    if peer.process.has_exited() {
        return Err(exited());
    }
    let created = peer
        .process
        .creation_time()
        .ok_or("cannot read the calling process's creation time")?;
    if created > peer.accepted {
        return Err("the calling process started after its connection: its id was reused".into());
    }
    let image = peer
        .process
        .image_path()
        .ok_or("cannot read the calling process's executable")?;
    // Both sides canonical, so letter case and `\\?\` prefixes compare alike.
    if image.canonicalize().ok().as_deref() != Some(app) {
        return Err(format!(
            "the caller ({}) is not the desktop app installed beside imperium-core",
            image.display()
        ));
    }
    if peer.process.has_exited() {
        return Err(exited());
    }
    Ok(())
}

/// Check 7 for an approval of `kind`: see [`release_gate`]. A debug build's `dev` approval,
/// which release daemons ignore, is exempt.
pub(super) fn may_count(kind: ApprovalKind) -> Result<(), String> {
    if kind == ApprovalKind::Dev {
        return Ok(());
    }
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|error| error.to_string())?;
    release_gate(
        SIGNED_BUILD.is_some(),
        !cfg!(debug_assertions),
        &exe,
        program_files().as_deref(),
    )
}

/// The per-machine Program Files folder from `SHGetKnownFolderPath(FOLDERID_ProgramFiles)`,
/// canonical; never the `ProgramFiles` variable, which the caller's environment controls.
fn program_files() -> Option<PathBuf> {
    dirs_sys::known_folder(windows_sys::Win32::UI::Shell::FOLDERID_ProgramFiles)?
        .canonicalize()
        .ok()
}

/// U7: an approval counts on Windows only from a release build that the signing job compiled
/// (`signed`) whose executable `exe` lies under `program_files`.
fn release_gate(
    signed: bool,
    release: bool,
    exe: &Path,
    program_files: Option<&Path>,
) -> Result<(), String> {
    const NOT_YET: &str = "no approval counts on Windows until signed per-machine releases";
    if !signed || !release {
        return Err(format!("{NOT_YET}: this is not a signed release build"));
    }
    match program_files {
        Some(folder) if exe.starts_with(folder) => Ok(()),
        _ => Err(format!(
            "{NOT_YET}: imperium-core does not run from under Program Files"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::*;
    use crate::app_identity::{Identify, InstalledApp};

    fn pinger() -> std::process::Child {
        Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn ping_path() -> PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap();
        Path::new(&root)
            .join("System32")
            .join("PING.EXE")
            .canonicalize()
            .unwrap()
    }

    fn peer_of(child: &std::process::Child, accepted: SystemTime) -> Peer {
        Peer {
            process: ClientProcess::open(child.id()).unwrap(),
            accepted,
        }
    }

    #[test]
    fn a_live_peer_at_the_expected_path_passes_and_another_path_is_refused() {
        let mut child = pinger();
        let peer = peer_of(&child, SystemTime::now());
        identify(&peer, &ping_path()).unwrap();
        let error = identify(&peer, &std::env::current_exe().unwrap()).unwrap_err();
        assert!(error.contains("not the desktop app"), "{error}");
        // The test binary itself is not the app installed beside imperium-core either.
        assert!(InstalledApp.identify(Some(&Peer::test_peer())).is_err());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_process_created_after_the_accept_is_refused() {
        let accepted = SystemTime::now() - Duration::from_secs(60);
        let mut child = pinger();
        let error = identify(&peer_of(&child, accepted), &ping_path()).unwrap_err();
        assert!(error.contains("reused"), "{error}");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn a_peer_that_exits_is_refused_whatever_reuses_its_id() {
        // The handle outlives the process, so a later process with the same id (which cannot be
        // forced here) is never what it reads.
        let mut child = pinger();
        let peer = peer_of(&child, SystemTime::now());
        child.kill().unwrap();
        child.wait().unwrap();
        let error = identify(&peer, &ping_path()).unwrap_err();
        assert!(error.contains("exited"), "{error}");
    }

    #[test]
    fn only_a_signed_release_under_program_files_counts() {
        let folder = Path::new(r"C:\Program Files");
        let inside = folder.join(r"Imperium\imperium-core.exe");
        release_gate(true, true, &inside, Some(folder)).unwrap();
        assert!(release_gate(false, true, &inside, Some(folder)).is_err());
        assert!(release_gate(true, false, &inside, Some(folder)).is_err());
        let outside = Path::new(r"C:\Users\me\AppData\Local\Imperium\imperium-core.exe");
        assert!(release_gate(true, true, outside, Some(folder)).is_err());
        assert!(release_gate(true, true, &inside, None).is_err());
        // A `dev` approval is exempt; nothing else counts in this unsigned test build.
        may_count(ApprovalKind::Dev).unwrap();
        assert!(may_count(ApprovalKind::Counted).is_err());
    }

    /// Run by `run_time_names_change_nothing` in a child process with `IMPERIUM_SIGNED_BUILD`
    /// set, `ProgramFiles` pointed at the test binary's folder, and a file of the constant's
    /// name beside it.
    #[test]
    #[ignore = "run by run_time_names_change_nothing in a prepared child"]
    fn counts_nothing_in_a_prepared_environment() {
        assert!(SIGNED_BUILD.is_none(), "the constant is compile-time only");
        let error = may_count(ApprovalKind::Counted).unwrap_err();
        assert!(error.contains("not a signed release"), "{error}");
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        // Even a signed release here would be refused: the known folder ignores `ProgramFiles`.
        let error = release_gate(true, true, &exe, program_files().as_deref()).unwrap_err();
        assert!(error.contains("Program Files"), "{error}");
    }

    #[test]
    fn run_time_names_change_nothing() {
        let exe = std::env::current_exe().unwrap();
        let folder = exe.parent().unwrap().to_path_buf();
        let planted = folder.join("IMPERIUM_SIGNED_BUILD");
        std::fs::write(&planted, "1").unwrap();
        let output = Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "app_identity::windows::tests::counts_nothing_in_a_prepared_environment",
            ])
            .env("IMPERIUM_SIGNED_BUILD", "1")
            .env("ProgramFiles", &folder)
            .env("ProgramW6432", &folder)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&planted);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{text}");
        // It must have run, not matched nothing.
        assert!(text.contains("1 passed"), "{text}");
    }
}
