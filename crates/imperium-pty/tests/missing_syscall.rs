//! A host that refuses `pidfd_open` (an old kernel, or a container's seccomp filter answering
//! `ENOSYS` or `EPERM`): the terminal is refused with a clear error. Its own test binary, because
//! the switch is process-wide.
#![cfg(target_os = "linux")]

mod common;

use imperium_pty::{Pty, SpawnError, force_missing_syscall};

#[test]
fn a_terminal_is_refused_on_a_host_without_pidfd_open() {
    let dir = common::work_dir();
    force_missing_syscall(true);
    let result = Pty::spawn(common::spec("/bin/sh", &[], dir.path()));
    force_missing_syscall(false);
    match result {
        Err(error @ SpawnError::MissingSyscall { .. }) => {
            let message = error.to_string();
            assert!(message.contains("pidfd_open"), "{message}");
            assert!(message.contains("none can start here"), "{message}");
        }
        Err(other) => panic!("the wrong error: {other}"),
        Ok(_) => panic!("a terminal started without pidfd_open"),
    }
}
