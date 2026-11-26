//! The driver's event allowlist against a worker that speaks the protocol wrongly (plan C3, F7),
//! on every OS. The worker's own loop never sends these events (src/serve.rs); a misbehaving test
//! worker does.

mod common;

use std::time::Duration;

use common::{is_gone, launcher_for, misbehaving_path, quick_limits};
use imperium_script::{Launcher, ScriptError, ScriptEvent};

fn misbehaving(mode: &str) -> Launcher {
    launcher_for(misbehaving_path(), 1).with_args([mode])
}

async fn outcome_of(mode: &str) -> Result<(), ScriptError> {
    let launcher = misbehaving(mode);
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
    let launcher = misbehaving("call");
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
    let launcher = misbehaving("hang");
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

/// M1: a worker that never stops writing frames is still cut at the deadline.
#[tokio::test]
async fn a_worker_that_floods_prints_is_cut_at_the_wall_deadline() {
    let launcher = misbehaving("print_flood");
    let mut limits = quick_limits();
    limits.exec = Duration::from_secs(60);
    let mut run = launcher
        .start("print(1)", &limits)
        .await
        .expect("the test worker starts");
    let pid = run.pid().unwrap();
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_millis(300);
    loop {
        match run.next_until(Some(deadline)).await {
            ScriptEvent::Print(_) => {}
            ScriptEvent::Done(Err(ScriptError::WallBudget)) => break,
            other => panic!("unexpected event {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "still reading after {:?}",
            started.elapsed()
        );
    }
    assert!(is_gone(pid), "the worker is still alive");
}

#[tokio::test]
async fn the_worker_starts_in_an_empty_private_working_folder() {
    assert_eq!(outcome_of("cwd_empty").await, Ok(()));
}

/// macOS has no OS memory limit, so the daemon's watchdog kills a worker whose resident memory
/// passes twice `script_memory_mb`, as an escaped program allocating outside Monty would; the
/// program ends as out of memory. (The test worker allocates with the system allocator.)
#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_macos_watchdog_kills_a_worker_over_its_memory_ceiling() {
    let launcher = misbehaving("hog");
    let mut limits = quick_limits();
    limits.memory_mb = 32;
    limits.exec = Duration::from_secs(60);
    let mut run = launcher
        .start("print(1)", &limits)
        .await
        .expect("the test worker starts");
    let pid = run.pid().unwrap();
    let started = std::time::Instant::now();
    match run.next().await {
        ScriptEvent::Done(Err(ScriptError::OutOfMemory)) => {}
        other => panic!("unexpected event {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "took {:?}",
        started.elapsed()
    );
    assert!(is_gone(pid), "the worker is still alive");
}
