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
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.txt");
    std::fs::write(&file, "secret").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let output = Command::new(common::probe_path())
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

/// F10, at the process level: when a stage cannot be applied the worker exits before reading a
/// request, and says which stage.
#[test]
fn the_worker_exits_without_reading_when_confinement_fails() {
    let stage = if cfg!(target_os = "linux") {
        "seccomp"
    } else if cfg!(target_os = "macos") {
        "sandbox_init"
    } else {
        // Without a hook, an unconfined platform refuses by itself.
        "platform"
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
}

/// Windows is launched confined by the daemon in stage 1b; until then nothing runs there.
#[cfg(windows)]
#[tokio::test]
async fn windows_reports_code_mode_unavailable() {
    let error = common::launcher()
        .start("print(1)", &common::quick_limits())
        .await
        .unwrap_err();
    assert!(
        matches!(error, imperium_script::ScriptError::Unavailable(_)),
        "{error:?}"
    );
}

#[allow(dead_code)]
fn unused(_: HashMap<String, String>, _: Option<TcpListener>) {}
