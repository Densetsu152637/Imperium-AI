//! A command in the OS isolation tier must not be able to obtain a credential for the daemon or
//! a connection to it, nor write the daemon's configuration and data folders: the endpoint file
//! (it holds the access token) is unreadable, the local socket cannot be opened, the folders
//! cannot be written even when a session folder contains them, and the workspace, even when it
//! sits inside the daemon's data folder (the default), still works.
//!
//! These run the real sandbox (`bwrap` on Linux, `sandbox-exec` on macOS). Where it cannot run
//! (bubblewrap missing, or user namespaces refused) they skip, unless
//! `IMPERIUM_REQUIRE_OS_SANDBOX` is set, as CI sets it: there a skip is a failure.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use imperium_tools::{
    GrantedPath, Sandbox, guard_from_commands, hide_from_os_sandbox, leave_writable_in_guards,
};

/// The tests register paths in the process-wide hide list and then delete them; run one at a
/// time, so a command never starts while another test is tearing down a folder it would mask.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TOKEN: &str = "TOKEN-THE-SANDBOX-MUST-NOT-SEE";

/// Whether a skipped check is a failure (CI sets `IMPERIUM_REQUIRE_OS_SANDBOX`).
fn required() -> bool {
    std::env::var("IMPERIUM_REQUIRE_OS_SANDBOX")
        .is_ok_and(|value| !matches!(value.as_str(), "" | "0"))
}

/// Runs `command` in the OS tier with `roots` as the workspace folders and `grants`; the exit
/// status and what it printed, or `None` when the sandbox could not be started.
async fn run(roots: &[&Path], grants: &[GrantedPath], command: &str) -> Option<(bool, String)> {
    // A command the daemon refuses to start (a guarded folder went missing) printed no secret.
    let mut process = match (Sandbox::Os { network: true }).command(roots, grants, command) {
        Ok(process) => process,
        Err(error) => return Some((false, error.to_string())),
    };
    // A command that hangs is a failure, not a stuck test run.
    process.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), process.output())
        .await
        .ok()?
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some((output.status.success(), text))
}

async fn in_sandbox(root: &Path, command: &str) -> Option<(bool, String)> {
    run(&[root], &[], command).await
}

/// Whether the sandbox works here, by a control command that must succeed; otherwise the test
/// skips (or fails, under `IMPERIUM_REQUIRE_OS_SANDBOX`).
async fn sandbox_works(root: &Path) -> bool {
    match in_sandbox(root, "cat inside.txt").await {
        Some((true, text)) if text.contains("visible") => true,
        other => {
            assert!(
                !required(),
                "the OS sandbox is required here but a control command failed: {other:?}"
            );
            eprintln!("skipping: the OS sandbox cannot run here ({other:?})");
            false
        }
    }
}

/// A fake daemon home next to the sources (not under `/tmp`, which the sandbox replaces), laid
/// out as the daemon's is: `data/daemon.json`, the socket's folder, `data/remotes`, the default
/// workspaces folder inside `data`, a `cfg` folder, and a `run` folder standing for the runtime
/// folder with the systemd user manager's private socket in it.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    cfg: PathBuf,
    endpoint: PathBuf,
    remote: PathBuf,
    socket: PathBuf,
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    systemd: PathBuf,
    workspace: PathBuf,
    _listeners: Vec<UnixListener>,
}

/// With `guard`, `cfg` and `data` are registered as the daemon does for its own folders.
fn fixture(guard: bool) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix(".t")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let root = dir.path().to_path_buf();
    let data = root.join("data");
    let cfg = root.join("cfg");
    let workspace = data.join("workspaces").join("p");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(cfg.join("sub")).unwrap();
    std::fs::create_dir_all(data.join("sessions")).unwrap();
    std::fs::write(cfg.join("config.toml"), "# config\n").unwrap();
    std::fs::write(workspace.join("inside.txt"), "visible").unwrap();
    let endpoint = data.join("daemon.json");
    std::fs::write(&endpoint, format!("{{\"token\":\"{TOKEN}\"}}")).unwrap();
    let remotes = data.join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    let remote = remotes.join("box.json");
    std::fs::write(&remote, format!("{{\"token\":\"{TOKEN}\"}}")).unwrap();
    let ipc = data.join("ipc");
    std::fs::create_dir_all(&ipc).unwrap();
    let socket = ipc.join("d.sock");
    let daemon = UnixListener::bind(&socket).unwrap();
    let run = root.join("run");
    std::fs::create_dir_all(run.join("systemd")).unwrap();
    let systemd = run.join("systemd").join("private");
    let manager = UnixListener::bind(&systemd).unwrap();
    hide_from_os_sandbox([endpoint.clone(), ipc, remotes, run]);
    if guard {
        guard_from_commands([cfg.clone(), data.clone()]);
        leave_writable_in_guards([data.join("workspaces")]);
    }
    Fixture {
        _dir: dir,
        root,
        data,
        cfg,
        endpoint,
        remote,
        socket,
        systemd,
        workspace,
        _listeners: vec![daemon, manager],
    }
}

#[tokio::test]
async fn a_command_cannot_read_the_endpoint_file_or_see_the_sockets() {
    let _serial = SERIAL.lock().await;
    let fixture = fixture(false);
    if !sandbox_works(&fixture.workspace).await {
        return;
    }
    // The workspace inside the data folder stays usable (the control above), and writable.
    let (ok, text) = in_sandbox(&fixture.workspace, "echo made > out.txt && cat out.txt")
        .await
        .unwrap();
    assert!(ok && text.contains("made"), "{text}");

    for secret in [&fixture.endpoint, &fixture.remote] {
        let (_, text) = in_sandbox(&fixture.workspace, &format!("cat '{}'", secret.display()))
            .await
            .unwrap();
        assert!(
            !text.contains(TOKEN),
            "{} was readable: {text}",
            secret.display()
        );
    }

    // Where the folder is replaced (Linux) the socket is gone; where it is only denied (macOS)
    // it cannot be connected to.
    #[cfg(target_os = "linux")]
    {
        for socket in [&fixture.socket, &fixture.systemd] {
            let (visible, text) = in_sandbox(
                &fixture.workspace,
                &format!("test -e '{}'", socket.display()),
            )
            .await
            .unwrap();
            assert!(!visible, "{} was visible: {text}", socket.display());
        }
    }
    #[cfg(target_os = "macos")]
    {
        // `nc` must work at all (an open socket outside the hidden folder), or this proves nothing.
        let open = fixture.root.join("o.sock");
        let _open = UnixListener::bind(&open).unwrap();
        let (control, text) = in_sandbox(
            &fixture.workspace,
            &format!("nc -w 2 -U '{}' </dev/null", open.display()),
        )
        .await
        .unwrap();
        assert!(
            control || !required(),
            "the nc control failed where the sandbox is required: {text}"
        );
        if control {
            let (connected, text) = in_sandbox(
                &fixture.workspace,
                &format!("nc -w 2 -U '{}' </dev/null", fixture.socket.display()),
            )
            .await
            .unwrap();
            assert!(!connected, "the socket accepted a command: {text}");
        } else {
            eprintln!("skipping the socket connection check: nc -U does not work here");
        }
    }
}

/// The user's real runtime folder is hidden whole: the systemd user manager's private socket
/// (`systemd-run --user` would start a process outside the sandbox) is not there, where the host
/// has one. (Runners often have no user manager, so the fixture's `run` folder above is the
/// check that always runs.)
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_real_runtime_folder_is_not_reachable() {
    let _serial = SERIAL.lock().await;
    use std::os::unix::fs::MetadataExt;
    let fixture = fixture(false);
    if !sandbox_works(&fixture.workspace).await {
        return;
    }
    let uid = std::fs::metadata("/proc/self").unwrap().uid();
    let mut candidates = vec![PathBuf::from(format!("/run/user/{uid}"))];
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        candidates.push(PathBuf::from(runtime));
    }
    for runtime in candidates.into_iter().filter(|dir| dir.is_dir()) {
        for inside in ["systemd/private", "bus", "keyring"] {
            let path = runtime.join(inside);
            let (visible, text) =
                in_sandbox(&fixture.workspace, &format!("test -e '{}'", path.display()))
                    .await
                    .unwrap();
            assert!(!visible, "{} was visible: {text}", path.display());
        }
    }
}

/// Where a writable grant contains the daemon's folders (here the whole fake home), a command
/// still cannot rename or link a hidden file to a path it can read, nor rename the folder around
/// it. (Bubblewrap cannot do so for a mount point; Seatbelt denies it by path.)
#[tokio::test]
async fn a_grant_around_the_data_folder_does_not_reopen_a_hidden_file() {
    let _serial = SERIAL.lock().await;
    let fixture = fixture(false);
    if !sandbox_works(&fixture.workspace).await {
        return;
    }
    let grants = [GrantedPath {
        path: fixture.root.clone(),
        writable: true,
    }];
    let moved = fixture.data.join("moved.json");
    let link = fixture.workspace.join("link.json");
    let renamed = fixture.root.join("data2");
    let commands = [
        format!(
            "mv '{}' '{}' 2>&1; cat '{}' 2>&1",
            fixture.endpoint.display(),
            moved.display(),
            moved.display()
        ),
        format!(
            "ln '{}' '{}' 2>&1; cat '{}' 2>&1",
            fixture.endpoint.display(),
            link.display(),
            link.display()
        ),
        format!(
            "mv '{}' '{}' 2>&1; cat '{}/daemon.json' 2>&1",
            fixture.data.display(),
            renamed.display(),
            renamed.display()
        ),
    ];
    for command in commands {
        let (_, text) = run(&[&fixture.workspace], &grants, &command).await.unwrap();
        assert!(
            !text.contains(TOKEN),
            "{command}: the token was revealed: {text}"
        );
    }
    // Seatbelt denies renaming the folder around a hidden path; bubblewrap makes every folder
    // between the grant and a hidden path a mount point, which refuses a rename.
    assert!(fixture.endpoint.exists(), "the endpoint file was moved");
}

/// A session folder that contains the configuration and data folders (a workspace of the home
/// folder) cannot write them; its own files, and the default workspace inside the data folder,
/// stay writable.
#[tokio::test]
async fn a_workspace_of_the_home_folder_cannot_write_config_or_data() {
    let _serial = SERIAL.lock().await;
    let fixture = fixture(true);
    if !sandbox_works(&fixture.workspace).await {
        return;
    }
    let roots: [&Path; 2] = [&fixture.root, &fixture.workspace];
    let config = fixture.cfg.join("config.toml");
    // Grants inside the guarded folders, outside the workspaces root, stay read-only too.
    let grants = [
        GrantedPath {
            path: fixture.cfg.join("sub"),
            writable: true,
        },
        GrantedPath {
            path: fixture.data.join("sessions"),
            writable: true,
        },
    ];
    let (ok, text) = run(
        &roots,
        &grants,
        &format!(
            "echo q > '{sub}/q.txt'; echo q > '{d}/sessions/q.txt'; \
             echo '[hooks]' >> '{c}'; echo x > '{d}/planted.txt'; \
             echo y > '{r}/home.txt' && echo z > '{w}/z.txt' && echo wrote",
            sub = fixture.cfg.join("sub").display(),
            c = config.display(),
            d = fixture.data.display(),
            r = fixture.root.display(),
            w = fixture.workspace.display(),
        ),
    )
    .await
    .unwrap();
    assert!(ok && text.contains("wrote"), "{text}");
    // Its own folders work.
    assert!(fixture.root.join("home.txt").exists(), "{text}");
    assert!(fixture.workspace.join("z.txt").exists(), "{text}");
    // The configuration and data folders were not written.
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "# config\n");
    assert!(!fixture.data.join("planted.txt").exists(), "{text}");
    assert!(!fixture.cfg.join("sub/q.txt").exists(), "{text}");
    assert!(!fixture.data.join("sessions/q.txt").exists(), "{text}");
    // The data folder cannot be renamed away either: it is read-only.
    let renamed = fixture.root.join("data2");
    let (_, text) = run(
        &roots,
        &[],
        &format!(
            "mv '{}' '{}' 2>&1; echo",
            fixture.data.display(),
            renamed.display()
        ),
    )
    .await
    .unwrap();
    assert!(fixture.endpoint.exists() && !renamed.exists(), "{text}");
    // And the endpoint file is unreadable all the same.
    let (_, text) = run(
        &roots,
        &[],
        &format!("cat '{}'", fixture.endpoint.display()),
    )
    .await
    .unwrap();
    assert!(!text.contains(TOKEN), "{text}");
}

/// macOS only: Seatbelt can deny one loopback port. (Bubblewrap cannot while the network is
/// shared; there the port stays reachable and the unreadable token is the protection.)
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_command_cannot_connect_to_the_daemons_port() {
    let _serial = SERIAL.lock().await;
    let fixture = fixture(false);
    if !sandbox_works(&fixture.workspace).await {
        return;
    }
    let daemon = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    imperium_tools::hide_loopback_port(daemon.local_addr().unwrap().port());
    let connect = |port: u16| format!("bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}'");
    let (control, text) = in_sandbox(
        &fixture.workspace,
        &connect(other.local_addr().unwrap().port()),
    )
    .await
    .unwrap();
    assert!(
        control || !required(),
        "the /dev/tcp control failed where the sandbox is required: {text}"
    );
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

/// Linux: a grant of the home folder cannot rename a folder around the guarded and hidden paths
/// (`mv ~/.local/share ~/.local/sx`) to get them unmasked on the next command: the folders in
/// between are mount points. Either the rename fails, or the next command is refused.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_renamed_ancestor_does_not_unmask_the_daemons_folders() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::Builder::new()
        .prefix(".t")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))
        .unwrap();
    let root = dir.path().to_path_buf();
    let base = root.join("a").join("b");
    let data = base.join("data");
    let cfg = base.join("cfg");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(root.join("inside.txt"), "visible").unwrap();
    std::fs::write(cfg.join("config.toml"), "# config\n").unwrap();
    let endpoint = data.join("daemon.json");
    std::fs::write(&endpoint, format!("{{\"token\":\"{TOKEN}\"}}")).unwrap();
    hide_from_os_sandbox([endpoint.clone()]);
    guard_from_commands([cfg.clone(), data.clone()]);
    if !sandbox_works(&root).await {
        return;
    }
    let renamed = root.join("z");
    let (_, first) = in_sandbox(
        &root,
        &format!(
            "mv '{}' '{}' 2>&1; echo",
            root.join("a").display(),
            renamed.display()
        ),
    )
    .await
    .unwrap();
    let (_, second) = in_sandbox(
        &root,
        &format!(
            "cat '{d}/daemon.json' 2>&1; echo '[hooks]' >> '{c}/config.toml' 2>&1; \
             cat '{rd}/daemon.json' 2>&1; echo '[hooks]' >> '{rc}/config.toml' 2>&1",
            d = renamed.join("b/data").display(),
            c = renamed.join("b/cfg").display(),
            rd = data.display(),
            rc = cfg.display(),
        ),
    )
    .await
    .unwrap();
    assert!(
        !first.contains(TOKEN) && !second.contains(TOKEN),
        "{first}{second}"
    );
    // Wherever the folder is now, the configuration was not written.
    for config in [cfg.join("config.toml"), renamed.join("b/cfg/config.toml")] {
        if let Ok(text) = std::fs::read_to_string(&config) {
            assert_eq!(
                text,
                "# config\n",
                "{} was written: {first}{second}",
                config.display()
            );
        }
    }
    // The rename was refused (a mount point), or the next command was.
    assert!(
        endpoint.exists() || second.contains("is missing"),
        "{first}{second}"
    );
}
