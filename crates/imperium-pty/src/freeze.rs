//! The freeze-then-kill loop, shared by the Linux supervisor (its descendants) and the daemon's
//! session sweep (a session's members).
//!
//! A fork bomb outruns a plain walk-and-kill, so every running process is first frozen with
//! `SIGSTOP` (a stopped process cannot fork) and the walk is repeated at once. But a loop that
//! waits for "nothing is running" before it kills can wait for ever, so the rules are:
//!
//! - every round also `SIGKILL`s whatever the walk already saw stopped, so each round makes
//!   progress whatever else happens;
//! - a process in uninterruptible-but-killable disk wait (`D`, for example a `vfork` parent whose
//!   child stopped before `exec`, or a stalled network file system) cannot be frozen: it is
//!   `SIGKILL`ed at once, and so is any process still running after a few `SIGSTOP`s;
//! - a leader (the supervisor, seen by the daemon's sweep) is never stopped, only killed;
//! - a process that answers `EPERM` (setuid or privileged) is recorded and left out of the walk's
//!   results from then on: it is the documented residual, not a reason to spin;
//! - a round that changes nothing backs off (1 ms, doubling to 50 ms), and a safety cap ends the
//!   loop, which logs what is left instead of spinning for ever.

use std::collections::{HashMap, HashSet};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::process::Signal;

/// What came of one signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Sent,
    /// The process was already gone, or is no longer the one the walk saw.
    Gone,
    /// `EPERM`: the process cannot be signalled by us.
    Denied,
}

/// A live process found by a walk, named by its pid and start time.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub key: (i32, u64),
    /// Live and not stopped, so it may still fork.
    pub running: bool,
    /// In `D` state: killed at once, never stopped first.
    pub disk_wait: bool,
    /// Killed, never stopped.
    pub leader: bool,
}

/// How the loop ended.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// Processes that answered `EPERM`.
    pub denied: Vec<(i32, u64)>,
    /// Live processes still there when the cap was reached.
    pub left: usize,
}

/// `SIGSTOP`s a running process this many times before it is killed instead.
const STOP_ATTEMPTS: u32 = 3;
const BACKOFF_START: Duration = Duration::from_millis(1);
const BACKOFF_MAX: Duration = Duration::from_millis(50);

/// Walks, then stops or kills what the walk found, until the walk finds nothing (denied
/// processes excluded) or `cap` passes.
pub(crate) fn run(
    mut walk: impl FnMut() -> Vec<Candidate>,
    mut send: impl FnMut(&Candidate, Signal) -> Delivery,
    cap: Instant,
) -> Outcome {
    let mut denied: HashSet<(i32, u64)> = HashSet::new();
    let mut attempts: HashMap<(i32, u64), u32> = HashMap::new();
    let mut previous: Vec<(i32, u64)> = Vec::new();
    let mut backoff = BACKOFF_START;
    loop {
        let left: Vec<Candidate> = walk()
            .into_iter()
            .filter(|candidate| !denied.contains(&candidate.key))
            .collect();
        let finish = |denied: HashSet<(i32, u64)>, left: usize| {
            let mut denied: Vec<(i32, u64)> = denied.into_iter().collect();
            denied.sort_unstable();
            Outcome { denied, left }
        };
        if left.is_empty() {
            return finish(denied, 0);
        }
        if Instant::now() >= cap {
            return finish(denied, left.len());
        }
        for candidate in &left {
            let tries = attempts.entry(candidate.key).or_insert(0);
            let signal = if !candidate.leader
                && candidate.running
                && !candidate.disk_wait
                && *tries < STOP_ATTEMPTS
            {
                *tries += 1;
                Signal::STOP
            } else {
                Signal::KILL
            };
            if send(candidate, signal) == Delivery::Denied {
                denied.insert(candidate.key);
            }
        }
        let mut now: Vec<(i32, u64)> = left.iter().map(|candidate| candidate.key).collect();
        now.sort_unstable();
        if now == previous {
            sleep(backoff);
            backoff = (backoff * 2).min(BACKOFF_MAX);
        } else {
            backoff = BACKOFF_START;
        }
        previous = now;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    fn candidate(pid: i32, running: bool) -> Candidate {
        Candidate {
            key: (pid, 1),
            running,
            disk_wait: false,
            leader: false,
        }
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn a_round_kills_what_is_stopped_while_it_stops_what_runs() {
        let calls = Cell::new(0);
        let sent = RefCell::new(Vec::new());
        let outcome = run(
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    vec![candidate(1, true), candidate(2, false)]
                } else {
                    Vec::new()
                }
            },
            |candidate, signal| {
                sent.borrow_mut().push((candidate.key.0, signal));
                Delivery::Sent
            },
            soon(),
        );
        assert_eq!(*sent.borrow(), [(1, Signal::STOP), (2, Signal::KILL)]);
        assert_eq!((outcome.denied.len(), outcome.left), (0, 0));
    }

    #[test]
    fn a_process_in_disk_wait_and_a_leader_are_killed_never_stopped() {
        let calls = Cell::new(0);
        let sent = RefCell::new(Vec::new());
        run(
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 1 {
                    let mut stuck = candidate(1, true);
                    stuck.disk_wait = true;
                    let mut leader = candidate(2, true);
                    leader.leader = true;
                    vec![stuck, leader]
                } else {
                    Vec::new()
                }
            },
            |candidate, signal| {
                sent.borrow_mut().push((candidate.key.0, signal));
                Delivery::Sent
            },
            soon(),
        );
        assert_eq!(*sent.borrow(), [(1, Signal::KILL), (2, Signal::KILL)]);
    }

    #[test]
    fn a_process_that_refuses_signals_is_set_aside_and_the_loop_ends() {
        let sent = Cell::new(0);
        let outcome = run(
            || vec![candidate(7, true)],
            |_, _| {
                sent.set(sent.get() + 1);
                Delivery::Denied
            },
            soon(),
        );
        assert_eq!(outcome.denied, [(7, 1)]);
        assert_eq!(outcome.left, 0);
        assert_eq!(sent.get(), 1, "a denied process is not signalled again");
    }

    #[test]
    fn a_process_that_never_stops_is_killed_and_the_cap_ends_the_loop() {
        let sent = RefCell::new(Vec::new());
        let started = Instant::now();
        let outcome = run(
            || vec![candidate(9, true)],
            |_, signal| {
                sent.borrow_mut().push(signal);
                Delivery::Sent
            },
            Instant::now() + Duration::from_millis(300),
        );
        assert_eq!(outcome.left, 1);
        let sent = sent.borrow();
        assert_eq!(sent[..3], [Signal::STOP; 3]);
        assert!(sent[3..].iter().all(|signal| *signal == Signal::KILL));
        assert!(sent.len() > 3, "it must go on to kill");
        // The backoff keeps it from spinning: far fewer than the thousands of rounds a hot loop
        // would make in 300 ms.
        assert!(sent.len() < 200, "{} rounds", sent.len());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
