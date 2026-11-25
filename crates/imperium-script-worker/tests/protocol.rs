//! The driver's event allowlist against a worker that speaks the protocol wrongly (plan C3, F7).
#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{misbehaving_path, quick_limits};
use imperium_script::{Launcher, ScriptError, ScriptEvent};

async fn outcome_of(mode: &str) -> Result<(), ScriptError> {
    let launcher = Launcher::new(misbehaving_path(), 1).with_args([mode]);
    let mut limits = quick_limits();
    limits.exec = Duration::from_secs(1);
    limits.request_slack = Duration::from_secs(1);
    let mut run = launcher
        .start("print(1)", &limits)
        .await
        .expect("the test worker starts");
    let pid = run.pid().unwrap();
    let result = loop {
        match run.next().await {
            ScriptEvent::Done(result) => break result,
            other => panic!("{mode}: unexpected event {other:?}"),
        }
    };
    // The worker is killed whatever the outcome.
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    assert!(
        rustix::process::test_kill_process(pid).is_err(),
        "{mode}: the worker is still alive"
    );
    result
}

#[tokio::test]
async fn events_outside_the_allowlist_kill_the_worker() {
    for mode in [
        "resolve_futures",
        "streamed_calls",
        "dump_result",
        "typing_error",
        "shutdown_dump",
        "ok_while_running",
        "unknown_tag",
        "garbage",
    ] {
        let result = outcome_of(mode).await;
        assert!(
            matches!(result, Err(ScriptError::Misbehaved(_))),
            "{mode}: {result:?}"
        );
    }
}

#[tokio::test]
async fn a_ping_is_answered_and_the_program_completes() {
    assert_eq!(outcome_of("ping_then_complete").await, Ok(()));
}

#[tokio::test]
async fn a_worker_that_stops_answering_is_killed_at_the_request_timeout() {
    let started = std::time::Instant::now();
    let result = outcome_of("hang").await;
    assert_eq!(result, Err(ScriptError::Unresponsive));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_worker_that_exits_without_a_fatal_error_is_crashed() {
    let result = outcome_of("crash").await;
    assert!(matches!(result, Err(ScriptError::Crashed(_))), "{result:?}");
}
