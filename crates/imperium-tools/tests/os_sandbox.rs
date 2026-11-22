//! A command in the OS isolation tier must not be able to obtain a credential for the daemon or
//! a connection to it: the endpoint file (it holds the access token) is unreadable and the local
//! socket cannot be opened, while the workspace, even when it sits inside the daemon's data
//! folder (the default), still works.
//!
//! These run the real sandbox (`bwrap` on Linux, `sandbox-exec` on macOS). Where it cannot run
//! (bubblewrap missing, or user namespaces refused) they skip, unless
//! `IMPERIUM_REQUIRE_OS_SANDBOX` is set, as CI sets it: there a skip is a failure.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Stdio;

use imperium_tools::{Sandbox, hide_from_os_sandbox};

const TOKEN: &str = "TOKEN-THE-SANDBOX-MUST-NOT-SEE";

/// Runs `command` in the OS tier with `root` as the workspace; the exit status and what it
/// printed, or `None` when the sandbox could not be started.
async fn in_sandbox(root: &Path, command: &str) -> Option<(bool, String)> {
    let mut process = Sandbox::Os { network: true }
        .command(&[root], &[], command)
        .expect("the OS tier is available on this platform");
    let output = process.stdin(Stdio::null()).output().await.ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some((output.status.success(), text))
}

/// Whether the sandbox works here, by a control command that must succeed; otherwise the test
/// skips (or fails, under `IMPERIUM_REQUIRE_OS_SANDBOX`).
async fn sandbox_works(root: &Path) -> bool {
    match in_sandbox(root, "cat inside.txt").await {
        Some((true, text)) if text.contains("visible") => true,
        other => {
            let required = std::env::var("IMPERIUM_REQUIRE_OS_SANDBOX")
                .is_ok_and(|value| !matches!(value.as_str(), "" | "0"));
            assert!(
                !required,
                "the OS sandbox is required here but a control command failed: {other:?}"
            );
            eprintln!("skipping: the OS sandbox cannot run here ({other:?})");
            false
        }
    }
}

/// A fake data folder next to the sources (not under `/tmp`, which the sandbox replaces), laid
/// out as the daemon's is: `daemon.json`, the socket's folder, and the default workspaces folder
/// inside it.
struct Fixture {
    _dir: tempfile::TempDir,
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    root: std::path::PathBuf,
    endpoint: std::path::PathBuf,
    socket: std::path::PathBuf,
    workspace: std::path::PathBuf,
    _listener: UnixListener,
}

fn fixture() -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix(".t")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let data = dir.path().join("data");
    let workspace = data.join("workspaces").join("p");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("inside.txt"), "visible").unwrap();
    let endpoint = data.join("daemon.json");
    std::fs::write(&endpoint, format!("{{\"token\":\"{TOKEN}\"}}")).unwrap();
    let ipc = data.join("ipc");
    std::fs::create_dir_all(&ipc).unwrap();
    let socket = ipc.join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    hide_from_os_sandbox([endpoint.clone(), ipc]);
    Fixture {
        root: dir.path().to_path_buf(),
        _dir: dir,
        endpoint,
        socket,
        workspace,
        _listener: listener,
    }
}

#[tokio::test]
async fn a_command_cannot_read_the_endpoint_file_or_see_the_socket() {
    let fixture = fixture();
    if !sandbox_works(&fixture.workspace).await {
        return;
    }
    // The workspace inside the data folder stays usable (the control above), and writable.
    let (ok, text) = in_sandbox(&fixture.workspace, "echo made > out.txt && cat out.txt")
        .await
        .unwrap();
    assert!(ok && text.contains("made"), "{text}");

    let (_, text) = in_sandbox(
        &fixture.workspace,
        &format!("cat '{}'", fixture.endpoint.display()),
    )
    .await
    .unwrap();
    assert!(!text.contains(TOKEN), "the token was readable: {text}");

    // Where the folder is replaced (Linux) the socket is gone; where it is only denied (macOS)
    // it cannot be connected to.
    #[cfg(target_os = "linux")]
    {
        let (visible, text) = in_sandbox(
            &fixture.workspace,
            &format!("test -S '{}'", fixture.socket.display()),
        )
        .await
        .unwrap();
        assert!(!visible, "the socket was visible: {text}");
    }
    #[cfg(target_os = "macos")]
    {
        // `nc` must work at all (an open socket outside the hidden folder), or this proves nothing.
        let open = fixture.root.join("o.sock");
        let _open = UnixListener::bind(&open).unwrap();
        let (control, _) = in_sandbox(
            &fixture.workspace,
            &format!("nc -U '{}' </dev/null", open.display()),
        )
        .await
        .unwrap();
        if control {
            let (connected, text) = in_sandbox(
                &fixture.workspace,
                &format!("nc -U '{}' </dev/null", fixture.socket.display()),
            )
            .await
            .unwrap();
            assert!(!connected, "the socket accepted a command: {text}");
        } else {
            eprintln!("skipping the socket connection check: nc -U does not work here");
        }
    }
}

/// macOS only: Seatbelt can deny one loopback port. (Bubblewrap cannot while the network is
/// shared; there the port stays reachable and the unreadable token is the protection.)
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_command_cannot_connect_to_the_daemons_port() {
    let fixture = fixture();
    if !sandbox_works(&fixture.workspace).await {
        return;
    }
    let daemon = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    imperium_tools::hide_loopback_port(daemon.local_addr().unwrap().port());
    let connect = |port: u16| format!("bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'");
    let (control, _) = in_sandbox(
        &fixture.workspace,
        &connect(other.local_addr().unwrap().port()),
    )
    .await
    .unwrap();
    if !control {
        eprintln!("skipping the port check: bash /dev/tcp does not work here");
        return;
    }
    let (connected, text) = in_sandbox(
        &fixture.workspace,
        &connect(daemon.local_addr().unwrap().port()),
    )
    .await
    .unwrap();
    assert!(!connected, "the daemon's port accepted a command: {text}");
}
