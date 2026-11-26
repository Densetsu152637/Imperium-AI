//! A descriptor of the daemon that is *not* close-on-exec must still not reach the shell. Its own
//! test binary: the descriptor is process-wide while it is open.
#![cfg(unix)]

mod common;

use std::os::fd::AsRawFd;

use common::{spec, wait_for, work_dir};

#[test]
fn only_stdio_reaches_the_shell() {
    let dir = work_dir();
    let canary = std::fs::File::open("/dev/null").expect("/dev/null opens");
    // What a library that forgot O_CLOEXEC would leave behind.
    rustix::io::fcntl_setfd(&canary, rustix::io::FdFlags::empty()).expect("flags clear");
    let number = canary.as_raw_fd();
    assert!(number > 2, "the canary is {number}");
    // Writing to the canary's number succeeds only if the shell holds it open.
    let script = format!("if true >&{number} 2>/dev/null; then echo LEAK; else echo CLOSED; fi");
    let pty = common::spawn_pty(spec("/bin/sh", &["-c", &script], dir.path()));
    let text = wait_for(&pty, "CLOSED");
    assert!(!text.contains("LEAK"), "output: {text:?}");
    drop(canary);
}
