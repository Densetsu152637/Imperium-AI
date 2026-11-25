//! Shared helpers for the worker tests: where the binaries are, and a driver loop.
#![allow(dead_code)]

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

pub fn launcher() -> Launcher {
    Launcher::new(worker_path(), 8)
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
                let value = MontyObject::string(format!("result of {}", call.function));
                outcome.calls.push(call);
                run.resume_value(value).await.expect("the call is answered");
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
