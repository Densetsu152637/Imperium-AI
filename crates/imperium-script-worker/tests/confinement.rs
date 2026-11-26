//! OS confinement of the worker, and that it fails closed (plan C1, stage 1 tests).

mod common;

use std::{
    collections::HashMap,
    net::TcpListener,
    process::{Command, Stdio},
};

use common::worker_path;

/// Runs the probe, which confines itself and then tries what a program must not do.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn probe(extra: &[&str]) -> HashMap<String, String> {
    probe_with(extra, None)
}

/// As [`probe`], leaving `inherit` open in the probe as a descriptor that is not close-on-exec.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn probe_with(extra: &[&str], inherit: Option<&std::fs::File>) -> HashMap<String, String> {
    use std::os::fd::AsRawFd;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.txt");
    std::fs::write(&file, "secret").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let mut command = Command::new(common::probe_path());
    if let Some(file) = inherit {
        command.args(["--fd", &file.as_raw_fd().to_string()]);
    }
    let output = command
        .args(["--file", file.to_str().unwrap()])
        .args(["--port", &port.to_string()])
        .args(["--pid", &std::process::id().to_string()])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the probe failed: {:?}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    eprintln!("PROBE\n{text}");
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn linux_worker_cannot_open_connect_spawn_or_signal() {
    let seen = probe(&["--memory-mb", "64"]);
    assert!(seen["confined"].contains("seccomp"), "{seen:?}");
    assert!(seen["open_file"].starts_with("denied"), "open: {seen:?}");
    assert!(seen["connect"].starts_with("denied"), "socket: {seen:?}");
    assert!(seen["spawn"].starts_with("denied"), "execve: {seen:?}");
    assert!(seen["kill_other"].starts_with("denied"), "kill: {seen:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_clone3_is_enosys_threads_start_getrandom_works_and_the_data_limit_is_set() {
    let seen = probe(&["--memory-mb", "64"]);
    assert!(
        seen["clone3"].contains("ret=-1") && seen["clone3"].contains("errno=38"),
        "{seen:?}"
    );
    assert_eq!(seen["thread"], "allowed", "{seen:?}");
    assert_eq!(seen["getrandom"], "8", "{seen:?}");
    assert_eq!(
        seen["rlimit_data"],
        (2 * 64 * 1024 * 1024_u64).to_string(),
        "{seen:?}"
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn linux_x32_system_calls_are_rejected() {
    let seen = probe(&[]);
    assert!(seen["x32"].contains("ret=-1"), "{seen:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_worker_cannot_read_connect_spawn_or_reach_mach_services() {
    let seen = probe(&[]);
    assert!(seen["confined"].contains("sandbox_init"), "{seen:?}");
    assert!(seen["open_file"].starts_with("denied"), "read: {seen:?}");
    assert!(seen["connect"].starts_with("denied"), "connect: {seen:?}");
    assert!(seen["spawn"].starts_with("denied"), "spawn: {seen:?}");
    assert!(
        seen["mach_lookup"].starts_with("denied"),
        "pasteboard: {seen:?}"
    );
}

/// F1: a descriptor the daemon left open without close-on-exec is closed before confinement, so
/// `fstat` on it gives `EBADF` inside the worker. The probe first checks it really was open.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn an_inherited_descriptor_is_closed_before_the_worker_is_confined() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = std::fs::File::open(dir.path()).unwrap();
    // `File::open` sets close-on-exec; clear it so the child inherits the descriptor.
    rustix::io::fcntl_setfd(&leaked, rustix::io::FdFlags::empty()).unwrap();
    let seen = probe_with(&[], Some(&leaked));
    assert_eq!(seen["inherited_fd_before"], "open", "{seen:?}");
    assert_eq!(seen["inherited_fd_after"], "closed", "{seen:?}");
    assert!(seen["confined"].contains("fds:"), "{seen:?}");
}

/// N1: the fallback loop (what an old kernel or a refused `close_range` gets) closes it too.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn the_fallback_descriptor_loop_closes_an_inherited_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = std::fs::File::open(dir.path()).unwrap();
    rustix::io::fcntl_setfd(&leaked, rustix::io::FdFlags::empty()).unwrap();
    let seen = probe_with(&["--skip-close-range", "1"], Some(&leaked));
    assert_eq!(seen["inherited_fd_before"], "open", "{seen:?}");
    assert_eq!(seen["inherited_fd_after"], "closed", "{seen:?}");
    assert!(seen["confined"].contains("(loop)"), "{seen:?}");
}

/// N2: the seccomp filter lets `prlimit64` and `sched_getaffinity` ask only about the worker.
#[cfg(target_os = "linux")]
#[test]
fn linux_prlimit_and_affinity_are_only_about_the_worker_itself() {
    let seen = probe(&[]);
    for key in ["prlimit_other", "prlimit_write", "affinity_other"] {
        assert_eq!(
            seen[key], "ret=-1 errno=1",
            "{key} should be EPERM: {seen:?}"
        );
    }
    assert_eq!(seen["prlimit_self_read"], "ret=0", "{seen:?}");
    assert_eq!(seen["prlimit_own_pid_read"], "ret=0", "{seen:?}");
    let affinity: i64 = seen["affinity_self"]
        .trim_start_matches("ret=")
        .parse()
        .unwrap();
    assert!(affinity > 0, "{seen:?}");
}

/// F1, fail closed: when the sweep cannot be applied the worker exits before reading a request.
#[cfg(unix)]
#[test]
fn the_worker_exits_when_the_descriptor_sweep_is_forced_to_fail() {
    let output = Command::new(worker_path())
        .args(["--force-confinement-failure", "fds"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(i32::from(imperium_script_worker::EXIT_CONFINEMENT_FAILED))
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("fds"));
}

/// F10, at the process level: when a stage cannot be applied the worker exits before reading a
/// request, and says which stage.
#[test]
fn the_worker_exits_without_reading_when_confinement_fails() {
    let stage = if cfg!(target_os = "linux") {
        "seccomp"
    } else if cfg!(target_os = "macos") {
        "sandbox_init"
    } else {
        // Started by hand rather than by the daemon's confined launch, the worker is not at Low
        // integrity, and refuses by itself.
        "integrity"
    };
    let mut command = Command::new(worker_path());
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        command.args(["--force-confinement-failure", stage]);
    }
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(i32::from(imperium_script_worker::EXIT_CONFINEMENT_FAILED))
    );
    assert!(output.stdout.is_empty(), "the worker wrote to stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(stage),
        "the reason does not name the stage: {stderr}"
    );
}

/// F10, through the driver: no program runs and the reason names what failed.
#[cfg(unix)]
#[tokio::test]
async fn no_program_runs_when_confinement_is_forced_to_fail() {
    let stage = if cfg!(target_os = "linux") {
        "seccomp"
    } else {
        "sandbox_init"
    };
    let launcher = common::launcher().with_args(["--force-confinement-failure", stage]);
    let error = launcher
        .start("print(1)", &common::quick_limits())
        .await
        .unwrap_err();
    match error {
        imperium_script::ScriptError::Unavailable(reason) => {
            assert!(reason.contains(stage), "{reason}")
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    let reason = launcher.check().await.unwrap_err();
    assert!(reason.contains(stage), "{reason}");
    assert!(common::launcher().check().await.is_ok());
}

/// F10 on Windows: every stage of the daemon's confined launch, and each of the worker's own
/// checks, fails closed. No program runs, the reason names the stage, and the slot is returned.
#[cfg(windows)]
#[tokio::test]
async fn no_program_runs_when_any_windows_stage_is_forced_to_fail() {
    for stage in [
        "labels",
        "token",
        "appcontainer",
        "job",
        "folder",
        "pipes",
        "process",
        "handles",
        "assign",
        "resume",
    ] {
        let launcher = common::launcher().with_forced_failure(stage);
        match launcher.start("print(1)", &common::quick_limits()).await {
            Err(imperium_script::ScriptError::Unavailable(reason)) => {
                assert!(reason.contains(stage), "{stage}: {reason}");
            }
            other => panic!("{stage}: expected Unavailable, got {other:?}"),
        }
        let reason = launcher.check().await.unwrap_err();
        assert!(reason.contains(stage), "{stage}: {reason}");
        assert_eq!(
            launcher.free_slots(),
            8,
            "{stage}: the slot was not returned"
        );
    }
    for stage in ["integrity", "privileges", "appcontainer", "job"] {
        let launcher = common::launcher().with_args(["--force-confinement-failure", stage]);
        let reason = launcher.check().await.unwrap_err();
        assert!(reason.contains(stage), "worker {stage}: {reason}");
    }
    assert!(common::launcher().check().await.is_ok());
}

#[allow(dead_code)]
fn unused(_: HashMap<String, String>, _: Option<TcpListener>) {}
