//! The driver's event allowlist against a worker that speaks the protocol wrongly (plan C3, F7).
#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{misbehaving_path, quick_limits};
use imperium_script::{Launcher, ScriptError, ScriptEvent};

/// True when no process has this id any more.
fn is_gone(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::test_kill_process(pid).is_err()
}

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
    let result = match run.next().await {
        ScriptEvent::Done(result) => result,
        other => panic!("{mode}: unexpected event {other:?}"),
    };
    // The worker is killed whatever the outcome.
    assert!(is_gone(pid), "{mode}: the worker is still alive");
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

#[tokio::test]
async fn an_oversized_event_frame_kills_the_worker() {
    let result = outcome_of("oversized_frame").await;
    match result {
        Err(ScriptError::Misbehaved(what)) => assert!(what.contains("bytes"), "{what}"),
        other => panic!("{other:?}"),
    }
}

/// The host never answers a call and asks for the next event: the program ends as misuse and the
/// worker is killed, rather than the driver reading on while a call is outstanding.
#[tokio::test]
async fn an_unanswered_call_ends_the_program_when_the_next_event_is_asked_for() {
    let launcher = Launcher::new(misbehaving_path(), 1).with_args(["call"]);
    let mut run = launcher
        .start("f()", &quick_limits())
        .await
        .expect("the test worker starts");
    let pid = run.pid().unwrap();
    assert!(matches!(run.next().await, ScriptEvent::Call(_)));
    match run.next().await {
        ScriptEvent::Done(Err(ScriptError::Misuse(_))) => {}
        other => panic!("unexpected event {other:?}"),
    }
    assert!(is_gone(pid), "the worker is still alive");
    // Answering after the end is refused, not sent to a dead worker.
    assert!(run.resume_error("late").await.is_err());
}

/// F3: a worker that never sends an event is cut at the caller's wall deadline, not at the much
/// longer `request_timeout`.
#[tokio::test]
async fn the_wall_deadline_cuts_a_wait_shorter_than_the_request_timeout() {
    let launcher = Launcher::new(misbehaving_path(), 1).with_args(["hang"]);
    let mut limits = quick_limits();
    limits.exec = Duration::from_secs(60);
    let mut run = launcher
        .start("print(1)", &limits)
        .await
        .expect("the test worker starts");
    let pid = run.pid().unwrap();
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_millis(300);
    match run.next_until(Some(deadline)).await {
        ScriptEvent::Done(Err(ScriptError::WallBudget)) => {}
        other => panic!("unexpected event {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
    assert!(is_gone(pid), "the worker is still alive");
}

#[tokio::test]
async fn the_worker_starts_in_an_empty_working_folder() {
    assert_eq!(outcome_of("cwd_empty").await, Ok(()));
}
