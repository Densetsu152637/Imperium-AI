//! Closing every descriptor the worker inherited beyond stdin, stdout and stderr (plan C1, F1).
//!
//! The daemon keeps other descriptors that are not close-on-exec (a socket, a log file, a pipe
//! of another run). A child inherits them, and after confinement a descriptor that is already
//! open still works: Landlock and `sandbox_init` stop new opens, not use of an open file. So the
//! worker closes everything from 3 up, before any confinement stage, and exits when it cannot.
//!
//! This module, and only this one beside `macos`, allows `unsafe`: closing a raw number has no
//! safe wrapper, and nothing in the worker owns a descriptor above 2 at this point.
#![allow(unsafe_code)]

use super::ConfineError;

/// The first descriptor to close. 0, 1 and 2 are the protocol pipes and stderr.
const FIRST: u32 = 3;

/// The most descriptors the fallback loop walks. A higher `RLIMIT_NOFILE` fails closed instead.
const LOOP_CEILING: u64 = 1 << 20;

/// Closes every descriptor from 3 up and says how: `close_range` or `loop`. `skip_fast` (a test
/// hook) goes straight to the loop so CI exercises it on Linux too.
pub(super) fn close_inherited(skip_fast: bool) -> Result<&'static str, ConfineError> {
    if !skip_fast && close_range() {
        return Ok("close_range");
    }
    close_loop()?;
    Ok("loop")
}

/// `close_range(3, ~0, 0)`: one call, Linux 5.9 and later. False when the kernel refuses it
/// (older kernel, or a filter that does not allow it), and the loop takes over.
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
fn close_range() -> bool {
    false
}

// `syscall` reads its variadic arguments as `long`, so they are passed as `c_long` (64-bit only).
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
fn close_range() -> bool {
    // SAFETY: the system call takes three integers (passed as `c_long`) and no pointer.
    let status = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            libc::c_long::from(FIRST),
            libc::c_long::from(u32::MAX),
            libc::c_long::from(0_u8),
        )
    };
    status == 0
}

/// The highest descriptor number plus one the process may have open.
fn descriptor_limit() -> Result<u64, ConfineError> {
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{Resource, getrlimit};
        // `None` means unlimited.
        Ok(getrlimit(Resource::Nofile).current.unwrap_or(u64::MAX))
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: no arguments, no memory.
        let limit = unsafe { libc::getdtablesize() };
        u64::try_from(limit)
            .map_err(|_| ConfineError::new("fds", "getdtablesize returned a negative number"))
    }
}

fn close_loop() -> Result<(), ConfineError> {
    let limit = descriptor_limit()?;
    if limit > LOOP_CEILING {
        return Err(ConfineError::new(
            "fds",
            format!("the descriptor limit {limit} is too high to close one by one"),
        ));
    }
    for fd in FIRST..u32::try_from(limit).unwrap_or(u32::MAX) {
        let Ok(fd) = libc::c_int::try_from(fd) else {
            break;
        };
        // SAFETY: closing a number; nothing in this process owns a descriptor above 2 yet, and
        // an unused number only gives `EBADF`.
        if unsafe { libc::close(fd) } != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EBADF) {
                return Err(ConfineError::new("fds", format!("close({fd}): {err}")));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_first_descriptor_to_close_is_after_stderr() {
        assert_eq!(super::FIRST, 3);
    }
}
