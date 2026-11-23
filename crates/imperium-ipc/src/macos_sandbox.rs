//! Whether a macOS process runs in a sandbox, asked by its audit token
//! (`sandbox_check_by_audit_token`, in libSystem's `libsystem_sandbox`). No crate wraps it, so
//! this module, and only this one on the Unix side, allows `unsafe`; the block says why it is
//! sound.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int};

/// `audit_token_t`: eight unsigned ints, passed by value.
#[repr(C)]
#[derive(Clone, Copy)]
struct AuditToken {
    val: [u32; 8],
}

/// `SANDBOX_FILTER_NONE`: no filter argument follows.
const SANDBOX_FILTER_NONE: c_int = 0;

unsafe extern "C" {
    // Declared as in <sandbox.h>: variadic, though no variadic argument is ever passed here.
    fn sandbox_check_by_audit_token(
        token: AuditToken,
        operation: *const c_char,
        filter: c_int,
        ...
    ) -> c_int;
}

/// Whether the process with audit token `token` is sandboxed: `Some(true)` or `Some(false)`, or
/// `None` when the check fails (for example, the process has exited).
pub fn is_sandboxed(token: [u32; 8]) -> Option<bool> {
    // SAFETY: the token is passed by value as the C struct it mirrors; a null operation with
    // SANDBOX_FILTER_NONE asks only whether the process is sandboxed at all and reads no further
    // argument, so passing none of the variadic ones is the documented use.
    let answer = unsafe {
        sandbox_check_by_audit_token(
            AuditToken { val: token },
            std::ptr::null(),
            SANDBOX_FILTER_NONE,
        )
    };
    match answer {
        0 => Some(false),
        n if n > 0 => Some(true),
        _ => None,
    }
}
