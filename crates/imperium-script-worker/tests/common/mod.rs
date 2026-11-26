//! Shared helpers for the worker tests: where the binaries are, launchers, process checks and a
//! driver loop.
#![allow(dead_code, unsafe_code)]

use std::{path::PathBuf, time::Duration};

use imperium_script::{Call, Launcher, Limits, MontyObject, ScriptError, ScriptEvent};

pub fn worker_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_imperium-script"))
}

pub fn probe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_confinement-probe"))
}

pub fn misbehaving_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_misbehaving-worker"))
}

pub fn windows_probe_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_windows-probe"))
}

/// A launcher for the real worker.
pub fn launcher() -> Launcher {
    launcher_for(worker_path(), 8)
}

/// A launcher for `worker`. On Windows the launcher refuses to start a worker until the daemon's
/// folders are labelled, so this labels a stand-in data folder first, as the daemon does at start.
pub fn launcher_for(worker: PathBuf, max_workers: usize) -> Launcher {
    #[cfg(windows)]
    labelled_stand_in();
    Launcher::new(worker, max_workers)
}

/// A stand-in for the daemon's data folder, labelled once per test binary.
#[cfg(windows)]
pub fn labelled_stand_in() -> &'static std::path::Path {
    static FOLDER: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    FOLDER
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            imperium_script::windows::labels::apply(&[dir.path().to_path_buf()])
                .expect("a fresh folder takes the label");
            dir
        })
        .path()
}

/// Short limits so a runaway program ends in a second or two.
pub fn quick_limits() -> Limits {
    Limits {
        memory_mb: 64,
        exec: Duration::from_secs(1),
        suspensions: 200,
        request_slack: Duration::from_secs(10),
    }
}

/// True when no process has this id any more (or it has exited and only its record remains).
#[cfg(unix)]
pub fn is_gone(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::test_kill_process(pid).is_err()
}

#[cfg(windows)]
pub fn is_gone(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    // SAFETY: opening, querying and closing a handle this function owns.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return true;
        }
        let mut code = 0_u32;
        let queried = GetExitCodeProcess(process, &raw mut code);
        CloseHandle(process);
        queried != 0 && code != STILL_ACTIVE as u32
    }
}

/// Kills process `pid` from outside, as a crash would end it.
#[cfg(unix)]
pub fn kill(pid: u32) {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
}

#[cfg(windows)]
pub fn kill(pid: u32) {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
    };
    // SAFETY: opening, terminating and closing a handle this function owns.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        assert!(!process.is_null(), "cannot open process {pid}");
        assert_ne!(TerminateProcess(process, 9), 0, "cannot kill process {pid}");
        CloseHandle(process);
    }
}

/// What a program did.
#[derive(Debug)]
pub struct Outcome {
    pub prints: String,
    pub calls: Vec<Call>,
    pub result: Result<(), ScriptError>,
}

/// Runs `code` to its end. Names in `host` resolve to host functions that return
/// `"result of <name>"`; any other name is undefined.
pub async fn run(launcher: &Launcher, code: &str, limits: &Limits, host: &[&str]) -> Outcome {
    let mut run = launcher
        .start(code, limits)
        .await
        .expect("the worker starts");
    let mut outcome = Outcome {
        prints: String::new(),
        calls: Vec::new(),
        result: Ok(()),
    };
    loop {
        match run.next().await {
            ScriptEvent::Print(text) => outcome.prints.push_str(&text),
            ScriptEvent::NameLookup(name) => {
                let known = host.contains(&name.as_str()).then_some(name.as_str());
                run.resume_name(known)
                    .await
                    .expect("the lookup is answered");
            }
            ScriptEvent::Call(call) => {
                // A bare-name call arrives as a call by name; unknown names are NameError.
                if host.contains(&call.function.as_str()) {
                    let value = MontyObject::string(format!("result of {}", call.function));
                    outcome.calls.push(call);
                    run.resume_value(value).await.expect("the call is answered");
                } else {
                    run.resume_not_found().await.expect("the call is answered");
                }
            }
            ScriptEvent::Done(result) => {
                outcome.result = result;
                break;
            }
        }
    }
    outcome
}

/// The exception type of a failed program, if it raised.
pub fn exc_type(result: &Result<(), ScriptError>) -> Option<&str> {
    match result {
        Err(ScriptError::Program { exc_type, .. }) => Some(exc_type),
        _ => None,
    }
}
