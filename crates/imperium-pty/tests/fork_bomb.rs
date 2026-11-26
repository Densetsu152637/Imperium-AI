//! An exponential fork bomb must not outrun the kill-all. Its own test binary: while it runs, the
//! user's process table is full, which would fail other tests' spawns.
#![cfg(target_os = "linux")]

mod common;

use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{BOUND, session_members, spec, wait_for_number, wait_until, work_dir};
use imperium_pty::Pty;

/// How many more tasks than the user has now the bomb may start.
const HEADROOM: usize = 400;

/// The bomb, run by bash. It caps itself first and refuses to start without the cap.
///
/// The cap is `RLIMIT_NPROC`, which counts the user's *tasks* (threads included), so the count it
/// starts from is of tasks (`ps -L`), not processes. It is set with bash's `ulimit -u`: dash,
/// Ubuntu's `/bin/sh`, has no `-u` (its process limit is `-p`), so when this ran under `sh` the
/// `ulimit` failed, the script went on and the bomb ran uncapped. On a GitHub runner that filled
/// the pids cgroup the runner's own agent shares (`pids.max` about 19,000); the agent's Go runtime
/// died on `newosproc` and it shut the runner down mid-job. So the script checks that the limit
/// took and exits before the bomb if it did not.
fn script() -> String {
    format!(
        "n=$(ps -L -u \"$(id -u)\" -o lwp= | wc -l); cap=$((n + {HEADROOM})); \
         ulimit -u \"$cap\" && [ \"$(ulimit -u)\" = \"$cap\" ] || {{ echo NO-CAP; exit 99; }}; \
         echo \"CAP:$cap:E\"; b() {{ b | b & }}; b"
    )
}

#[test]
fn an_exponential_fork_bomb_is_ended() {
    let dir = work_dir();
    let script = script();
    let pty = Pty::spawn(spec("/bin/bash", &["-c", &script], dir.path())).unwrap();
    let supervisor = pty.pid().unwrap();
    // The cap is in force before the bomb's first fork, or the test stops here.
    wait_for_number(&pty, "CAP:", ":E");
    wait_until("the bomb to spread", || {
        session_members(supervisor).len() > 100
    });
    // Capped, the bomb levels off near its headroom; uncapped it reaches thousands in a second.
    sleep(Duration::from_secs(1));
    let spread = session_members(supervisor).len();
    assert!(
        spread <= HEADROOM + 200,
        "the bomb outgrew its cap: {spread} processes in the terminal's session"
    );
    let started = Instant::now();
    pty.end();
    assert!(
        started.elapsed() < BOUND,
        "end took {:?}; the bomb outran the kill",
        started.elapsed()
    );
    wait_until("the session to be empty", || {
        session_members(supervisor).is_empty()
    });
}
