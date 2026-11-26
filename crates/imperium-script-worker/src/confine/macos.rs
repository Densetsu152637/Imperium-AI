//! macOS confinement: `sandbox_init` with a profile that starts from `(deny default)` (plan C1, N3).
//!
//! The profile allows reading the worker's own binary and the system libraries it may still load,
//! reading a few `sysctl` keys the Rust runtime asks for, and signalling itself. Everything else
//! is denied by the default: file writes and other reads, the network, process creation,
//! `mach-lookup` (so no pasteboard, launch services or any other system service), IPC and IOKit.
//! No crate wraps `sandbox_init`, so this module, and only this one, allows `unsafe`.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_int};

use rustix::process::{Resource, Rlimit, setrlimit};

use super::{ConfineError, Report, Settings, forced};

unsafe extern "C" {
    // <sandbox.h>: `flags` 0 means `profile` is the profile text itself.
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

fn set_limit(resource: Resource, value: u64) -> rustix::io::Result<()> {
    setrlimit(
        resource,
        Rlimit {
            current: Some(value),
            maximum: Some(value),
        },
    )
}

pub(super) fn apply(settings: &Settings) -> Result<Report, ConfineError> {
    let mut report = Report::default();

    forced(settings, "rlimit")?;
    // The core limit is the one that must hold: no core dump of the program's memory.
    set_limit(Resource::Core, 0).map_err(|err| ConfineError::new("rlimit", err))?;
    // Darwin does not enforce `RLIMIT_DATA` (the allocator's hard limit is the memory bound
    // there) and some releases refuse to set it (EINVAL), so it is a best-effort extra.
    match set_limit(Resource::Data, settings.memory_limit_bytes) {
        Ok(()) => report
            .applied
            .push(format!("rlimit_data={}", settings.memory_limit_bytes)),
        Err(err) => report
            .applied
            .push(format!("rlimit_data=unavailable ({err})")),
    }

    forced(settings, "sandbox_init")?;
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|err| {
            ConfineError::new(
                "sandbox_init",
                format!("cannot locate the worker binary: {err}"),
            )
        })?;
    let exe = exe
        .to_str()
        .filter(|path| !path.contains(['"', '\\', '\0']))
        .ok_or_else(|| {
            ConfineError::new(
                "sandbox_init",
                "the worker path cannot be written into a profile",
            )
        })?;
    let profile = profile(exe);
    let profile = CString::new(profile).map_err(|err| ConfineError::new("sandbox_init", err))?;

    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `profile` is a valid NUL-terminated string that outlives the call; `error` is a
    // valid out-pointer. With flags 0 the first argument is the profile text, as documented in
    // <sandbox.h>.
    let status = unsafe { sandbox_init(profile.as_ptr(), 0, &raw mut error) };
    if status != 0 {
        let reason = if error.is_null() {
            format!("sandbox_init returned {status}")
        } else {
            // SAFETY: on failure `error` points to a NUL-terminated message allocated by the
            // library, valid until it is freed below.
            let message = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: `error` came from `sandbox_init` and is freed exactly once.
            unsafe { sandbox_free_error(error) };
            message
        };
        return Err(ConfineError::new("sandbox_init", reason));
    }
    report
        .applied
        .push("sandbox_init: (deny default)".to_owned());
    Ok(report)
}

/// The profile text for a worker at `exe`.
fn profile(exe: &str) -> String {
    format!(
        r#"(version 1)
(deny default)
(allow file-read* (literal "{exe}"))
(allow file-read*
  (subpath "/usr/lib")
  (subpath "/System/Library")
  (subpath "/System/Cryptexes")
  (subpath "/System/Volumes/Preboot/Cryptexes")
  (literal "/dev/urandom")
  (literal "/dev/random"))
(allow sysctl-read
  (sysctl-name
    "hw.ncpu" "hw.activecpu" "hw.logicalcpu" "hw.logicalcpu_max"
    "hw.physicalcpu" "hw.physicalcpu_max" "hw.pagesize" "hw.pagesize32" "hw.memsize"))
(allow signal (target self))
"#
    )
}

#[cfg(test)]
mod tests {
    use super::profile;

    #[test]
    fn profile_denies_by_default_and_names_the_binary() {
        let text = profile("/opt/imperium-script");
        assert!(text.starts_with("(version 1)\n(deny default)"));
        assert!(text.contains(r#"(literal "/opt/imperium-script")"#));
        assert!(!text.contains("network"));
        assert!(!text.contains("mach-lookup"));
    }
}
