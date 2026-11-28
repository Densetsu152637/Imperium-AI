//! Keeping a child from inheriting descriptors beyond stdin, stdout and stderr.
//!
//! A descriptor the daemon opened without close-on-exec, or that another thread has not marked
//! yet, reaches every program the daemon starts in that moment. `portable-pty` marks a new
//! pseudo terminal's master and slave close-on-exec only after `openpty` returns, so an agent's
//! command forked in that window would hold the user's terminal: it could read what is typed
//! and type into it, past the terminal gate and the OS sandbox alike. [`close_inherited_on_exec`]
//! makes every descriptor from 3 up close-on-exec in the child, between `fork` and `exec`, so a
//! command starts with 0, 1 and 2 alone, whatever the rest of the daemon is doing.
//!
//! This module, and besides the macOS ones on the Unix side, allows `unsafe`:
//! the hook runs between `fork` and `exec` (`CommandExt::pre_exec`), and the calls it makes take
//! raw descriptor numbers, which no safe wrapper does.
#![allow(unsafe_code)]

/// The first descriptor marked. 0, 1 and 2 are the child's stdio, set up before the hook runs.
#[cfg(unix)]
const FIRST: libc::c_int = 3;

/// The most descriptors the fallback loop walks (when `close_range` is not there).
#[cfg(unix)]
const LOOP_CEILING: libc::c_int = 1 << 16;

/// Makes `command`'s child close every descriptor from 3 up when it runs its program. A no-op on
/// Windows, where a child inherits only handles marked inheritable.
pub fn close_inherited_on_exec(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Worked out here, in the parent: the hook may only make async-signal-safe calls.
        let limit = descriptor_limit();
        // SAFETY: the hook runs in the child between `fork` and `exec`, where only
        // async-signal-safe functions may be called. It calls `close_range` (a system call) and,
        // failing that, `fcntl` on plain integers, and allocates nothing, takes no lock and
        // touches no memory but its own two integers.
        unsafe {
            command.pre_exec(move || {
                mark_from_first(limit);
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

/// One more than the highest descriptor the fallback loop marks.
#[cfg(unix)]
fn descriptor_limit() -> libc::c_int {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes one `rlimit` into the struct it is given, which lives here.
    let found = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0;
    if !found {
        return LOOP_CEILING;
    }
    libc::c_int::try_from(limit.rlim_cur)
        .unwrap_or(LOOP_CEILING)
        .clamp(FIRST, LOOP_CEILING)
}

/// In the child: every descriptor from [`FIRST`] up becomes close-on-exec.
#[cfg(unix)]
fn mark_from_first(limit: libc::c_int) {
    if close_range_cloexec() {
        return;
    }
    for fd in FIRST..limit {
        // SAFETY: `fcntl` on a number; one that is not open only gives `EBADF`.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`: one call, Linux 5.11 and later. It marks rather
/// than closes, so the pipe through which the standard library reports a failed `exec` stays
/// open until `exec`. False when the kernel refuses it, and the loop takes over.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn close_range_cloexec() -> bool {
    /// `CLOSE_RANGE_CLOEXEC` from `<linux/close_range.h>`.
    const CLOSE_RANGE_CLOEXEC: libc::c_long = 1 << 2;
    // SAFETY: the system call takes three integers (passed as `c_long`, as `syscall` reads its
    // variadic arguments) and no pointer.
    let status = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            libc::c_long::from(FIRST),
            libc::c_long::from(u32::MAX),
            CLOSE_RANGE_CLOEXEC,
        )
    };
    status == 0
}

#[cfg(all(unix, not(all(target_os = "linux", target_pointer_width = "64"))))]
fn close_range_cloexec() -> bool {
    false
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    /// A descriptor the parent leaves inheritable is gone in the child; stdio is not.
    #[test]
    fn a_child_keeps_only_its_stdio() {
        let file = tempfile::tempfile().unwrap();
        let fd = file.as_raw_fd();
        // Inheritable, as a descriptor not yet marked by another thread is.
        // SAFETY: `fcntl` on a descriptor this test owns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
        }
        let probe =
            format!("if [ -e /dev/fd/{fd} ]; then echo HELD; else echo GONE; fi; echo ok >&2");
        let run = |closing: bool| {
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", &probe]);
            if closing {
                close_inherited_on_exec(&mut command);
            }
            command.output().unwrap()
        };
        // The control: without the hook the child holds it.
        assert_eq!(String::from_utf8_lossy(&run(false).stdout).trim(), "HELD");
        let closed = run(true);
        assert_eq!(String::from_utf8_lossy(&closed.stdout).trim(), "GONE");
        assert_eq!(String::from_utf8_lossy(&closed.stderr).trim(), "ok");
        // The fallback loop would reach it too.
        assert!(descriptor_limit() > fd);
        drop(file);
    }
}
