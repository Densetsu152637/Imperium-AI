//! The real worker through the real driver: the interpreter, its limits and its processes.
//! Unix only until the Windows launch lands (stage 1b of the code-mode plan).
#![cfg(unix)]

mod common;

use std::time::{Duration, Instant};

use common::{exc_type, launcher, quick_limits, run};
use imperium_script::{MontyObject, ScriptError, ScriptEvent};

#[tokio::test]
async fn a_program_prints() {
    let outcome = run(
        &launcher(),
        "print(sum(range(10)))\nprint('done')",
        &quick_limits(),
        &[],
    )
    .await;
    assert_eq!(outcome.result, Ok(()));
    assert_eq!(outcome.prints, "45\ndone\n");
}

#[tokio::test]
async fn a_program_suspends_at_a_host_call_and_resumes_with_the_value() {
    let code = "text = lookup('a', n=2)\nprint(text)";
    let outcome = run(&launcher(), code, &quick_limits(), &["lookup"]).await;
    assert_eq!(outcome.result, Ok(()));
    assert_eq!(outcome.prints, "result of lookup\n");
    assert_eq!(outcome.calls.len(), 1);
    assert_eq!(outcome.calls[0].function, "lookup");
    assert_eq!(outcome.calls[0].args.len(), 1);
    assert_eq!(outcome.calls[0].kwargs.len(), 1);
}

#[tokio::test]
async fn an_unknown_name_raises_name_error() {
    let outcome = run(&launcher(), "print(nothing_here)", &quick_limits(), &[]).await;
    assert_eq!(
        exc_type(&outcome.result),
        Some("NameError"),
        "{:?}",
        outcome.result
    );
}

#[tokio::test]
async fn a_failed_call_raises_in_the_program_and_can_be_caught() {
    let code = "try:\n    lookup()\nexcept RuntimeError as err:\n    print('caught', err)";
    let mut run = launcher()
        .start(code, &quick_limits())
        .await
        .expect("starts");
    let mut prints = String::new();
    let result = loop {
        match run.next().await {
            ScriptEvent::NameLookup(_) => run.resume_name(Some("lookup")).await.unwrap(),
            ScriptEvent::Call(_) => run.resume_error("the user denied this").await.unwrap(),
            ScriptEvent::Print(text) => prints.push_str(&text),
            ScriptEvent::Done(result) => break result,
        }
    };
    assert_eq!(result, Ok(()));
    assert_eq!(prints, "caught the user denied this\n");
}

/// Goal 1 of the plan: files, environment, sockets, processes and modules are not there.
#[tokio::test]
async fn capabilities_are_denied() {
    let snippets = [
        "print(open('Cargo.toml').read())",
        "import os\nprint(os.getenv('PATH'))",
        "import os\nprint(os.environ)",
        "import socket",
        "import subprocess",
        "print(__import__('os'))",
    ];
    for code in snippets {
        let outcome = run(&launcher(), code, &quick_limits(), &[]).await;
        assert!(outcome.result.is_err(), "{code:?} ran: {outcome:?}");
        assert!(
            outcome.prints.is_empty(),
            "{code:?} printed {:?}",
            outcome.prints
        );
        eprintln!("DENIED {code:?} -> {:?}", outcome.result);
    }
}

/// A2: whether `eval` and `exec` exist in a program. Recorded, not asserted: if they do, the
/// engine refuses programs that name them.
#[tokio::test]
async fn eval_and_exec_outcome_is_recorded() {
    let outcome = run(&launcher(), "print(eval('1+1'))", &quick_limits(), &[]).await;
    eprintln!(
        "A2 eval -> prints={:?} result={:?}",
        outcome.prints, outcome.result
    );
    let outcome = run(&launcher(), "exec('print(2)')", &quick_limits(), &[]).await;
    eprintln!(
        "A2 exec -> prints={:?} result={:?}",
        outcome.prints, outcome.result
    );
}

#[tokio::test]
async fn an_infinite_loop_ends_on_the_time_budget() {
    let started = Instant::now();
    let outcome = run(&launcher(), "while True:\n    pass", &quick_limits(), &[]).await;
    assert!(outcome.result.is_err(), "{outcome:?}");
    assert!(
        started.elapsed() < Duration::from_secs(9),
        "took {:?}",
        started.elapsed()
    );
    eprintln!("TIME BUDGET -> {:?}", outcome.result);
}

#[tokio::test]
async fn a_huge_allocation_ends_on_the_memory_budget() {
    let outcome = run(
        &launcher(),
        "x = [0] * 10**9\nprint(len(x))",
        &quick_limits(),
        &[],
    )
    .await;
    assert!(outcome.prints.is_empty(), "{outcome:?}");
    assert!(
        matches!(outcome.result, Err(ScriptError::OutOfMemory))
            || exc_type(&outcome.result) == Some("MemoryError"),
        "{:?}",
        outcome.result
    );
}

/// N1: the driver, not the worker, ends a program that never stops making requests.
#[tokio::test]
async fn suspension_loops_are_stopped_by_the_driver() {
    let mut limits = quick_limits();
    limits.suspensions = 20;
    let loops = [
        ("host calls", "while True:\n    lookup()", vec!["lookup"]),
        (
            "undefined-name lookups",
            "while True:\n    try:\n        never_defined\n    except NameError:\n        pass",
            vec![],
        ),
        (
            "os calls",
            "while True:\n    try:\n        open('x')\n    except OSError:\n        pass",
            vec![],
        ),
    ];
    for (what, code, host) in loops {
        let outcome = run(&launcher(), code, &limits, &host).await;
        assert_eq!(
            outcome.result,
            Err(ScriptError::TooManyRequests),
            "{what}: {outcome:?}"
        );
    }
}

#[tokio::test]
async fn sleeping_returns_at_once_and_clocks_and_entropy_work() {
    let started = Instant::now();
    let code = "import time, datetime, random\ntime.sleep(5)\nprint(datetime.datetime.now().year > 2000)\nprint(random.random() < 1.0)";
    let outcome = run(&launcher(), code, &quick_limits(), &[]).await;
    assert_eq!(outcome.result, Ok(()), "{outcome:?}");
    assert_eq!(outcome.prints, "True\nTrue\n");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "slept for {:?}",
        started.elapsed()
    );
}

fn gone(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    rustix::process::test_kill_process(pid).is_err()
}

#[tokio::test]
async fn programs_run_in_different_workers_that_are_gone_afterwards() {
    let launcher = launcher();
    let first = launcher.start("print(1)", &quick_limits()).await.unwrap();
    let second = launcher.start("print(2)", &quick_limits()).await.unwrap();
    let (a, b) = (first.pid().unwrap(), second.pid().unwrap());
    assert_ne!(a, b);
    first.finish().await;
    second.finish().await;
    assert!(gone(a) && gone(b), "a worker outlived its run");
    assert_eq!(launcher.free_slots(), 8);
}

#[tokio::test]
async fn ending_a_run_mid_program_leaves_no_worker() {
    let launcher = launcher();
    let mut run = launcher.start("lookup()", &quick_limits()).await.unwrap();
    let pid = run.pid().unwrap();
    assert!(matches!(run.next().await, ScriptEvent::NameLookup(_)));
    run.finish().await;
    assert!(gone(pid));
    assert_eq!(launcher.free_slots(), 8, "the worker slot was not returned");
}

#[tokio::test]
async fn a_killed_worker_is_reported_crashed_and_the_next_program_runs() {
    let launcher = launcher();
    let mut run = launcher.start("lookup()", &quick_limits()).await.unwrap();
    assert!(matches!(run.next().await, ScriptEvent::NameLookup(_)));
    let pid = rustix::process::Pid::from_raw(i32::try_from(run.pid().unwrap()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::KILL).unwrap();
    // The answer may go to a dead worker; either way the program ends crashed.
    let _ = run.resume_name(Some("lookup")).await;
    match run.next().await {
        ScriptEvent::Done(Err(ScriptError::Crashed(_))) => {}
        other => panic!("expected a crash, got {other:?}"),
    }
    let next = run_ok(&launcher).await;
    assert_eq!(next, "ok\n");
}

async fn run_ok(launcher: &imperium_script::Launcher) -> String {
    run(launcher, "print('ok')", &quick_limits(), &[])
        .await
        .prints
}

/// F8: the host may hold a call far longer than `request_timeout`; the worker is not timed then.
#[tokio::test]
async fn a_held_call_outlives_the_request_timeout() {
    let mut limits = quick_limits();
    limits.request_slack = Duration::from_secs(1); // request_timeout is 2 s
    let mut run = launcher().start("print(lookup())", &limits).await.unwrap();
    loop {
        match run.next().await {
            ScriptEvent::NameLookup(_) => run.resume_name(Some("lookup")).await.unwrap(),
            ScriptEvent::Call(_) => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                run.resume_value(MontyObject::string("late")).await.unwrap();
            }
            ScriptEvent::Print(text) => assert_eq!(text, "late\n"),
            ScriptEvent::Done(result) => {
                assert_eq!(result, Ok(()));
                break;
            }
        }
    }
}

/// The worker's environment is empty (read from /proc, where there is one).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_workers_environment_is_empty() {
    let mut run = launcher().start("lookup()", &quick_limits()).await.unwrap();
    let pid = run.pid().unwrap();
    assert!(matches!(run.next().await, ScriptEvent::NameLookup(_)));
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    assert!(
        environ.is_empty(),
        "the worker inherited {} bytes of environment",
        environ.len()
    );
    run.finish().await;
}

#[tokio::test]
async fn a_second_program_never_sees_the_first_ones_state() {
    let launcher = launcher();
    let first = run(&launcher, "secret = 41", &quick_limits(), &[]).await;
    assert_eq!(first.result, Ok(()));
    let second = run(&launcher, "print(secret)", &quick_limits(), &[]).await;
    assert_eq!(exc_type(&second.result), Some("NameError"));
}
