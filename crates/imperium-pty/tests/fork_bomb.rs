//! An exponential fork bomb must not outrun the kill-all. Its own test binary: while it runs, the
//! user's process table is full, which would fail other tests' spawns.
#![cfg(target_os = "linux")]

mod common;

use std::time::Instant;

use common::{BOUND, session_members, spec, wait_until, work_dir};
use imperium_pty::Pty;

#[test]
fn an_exponential_fork_bomb_is_ended() {
    let dir = work_dir();
    // `ulimit -u` caps this process tree at 400 more processes than the user has now, so the
    // bomb cannot starve the machine; inside the cap it forks as fast as it can.
    let script = "n=$(ps -u $(id -u) -o pid= | wc -l); ulimit -u $((n + 400)); b() { b | b & }; b";
    let pty = Pty::spawn(spec("/bin/sh", &["-c", script], dir.path())).unwrap();
    let supervisor = pty.pid().unwrap();
    wait_until("the bomb to spread", || {
        session_members(supervisor).len() > 100
    });
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
