//! OS self-confinement, applied before the worker reads any program (plan C1).
//!
//! Linux: `PR_SET_NO_NEW_PRIVS`, a data-size limit, Landlock where the kernel has it, and a
//! seccomp-bpf allowlist ([`linux`]). macOS: `sandbox_init` with a `(deny default)` profile
//! ([`macos`]). Any stage that cannot be applied is an error, and the worker then exits before it
//! reads a request: it runs with the confinement the plan states or not at all. Windows is
//! confined by the daemon at launch (stage 1b); until then the worker refuses to run there.

use std::fmt;

use crate::options::Options;

#[cfg(unix)]
mod fds;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;

#[cfg(target_os = "linux")]
use linux::apply as apply_platform;
#[cfg(target_os = "macos")]
use macos::apply as apply_platform;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use unsupported::apply as apply_platform;

/// What to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The OS backstop for memory: `RLIMIT_DATA` on Linux, twice the program's `script_memory_mb`
    /// (macOS has no enforced one, plan C1).
    pub memory_limit_bytes: u64,
    /// Test hook: the stage that must fail instead of being applied (`test-support` builds only).
    pub force_failure: Option<String>,
}

impl From<&Options> for Settings {
    fn from(options: &Options) -> Self {
        Self {
            memory_limit_bytes: options
                .memory_mb
                .saturating_mul(2)
                .saturating_mul(1024 * 1024),
            force_failure: options.force_failure.clone(),
        }
    }
}

/// What was applied, for diagnostics and tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// One line per applied stage.
    pub applied: Vec<String>,
}

/// A stage that could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfineError {
    /// The stage, for example `seccomp` or `sandbox_init`.
    pub stage: &'static str,
    /// Why it failed.
    pub reason: String,
}

impl ConfineError {
    pub(crate) fn new(stage: &'static str, reason: impl fmt::Display) -> Self {
        Self {
            stage,
            reason: reason.to_string(),
        }
    }
}

impl fmt::Display for ConfineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.stage, self.reason)
    }
}

impl std::error::Error for ConfineError {}

/// Applies this platform's confinement to the calling process. Call it on the main thread, before
/// any other thread exists and before the first read from stdin.
pub fn apply(settings: &Settings) -> Result<Report, ConfineError> {
    // First, before any other stage: a descriptor inherited from the daemon would stay usable
    // under the confinement (plan C1, F1). Failing here is failing closed.
    #[cfg(unix)]
    {
        forced(settings, "fds")?;
        fds::close_inherited()?;
        let mut report = apply_platform(settings)?;
        report
            .applied
            .insert(0, "fds: every descriptor above 2 closed".to_owned());
        Ok(report)
    }
    #[cfg(not(unix))]
    {
        apply_platform(settings)
    }
}

/// Fails with the stage's name when a test asked for exactly that stage to fail.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn forced(settings: &Settings, stage: &'static str) -> Result<(), ConfineError> {
    if settings.force_failure.as_deref() == Some(stage) {
        Err(ConfineError::new(stage, "forced to fail by the test hook"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_backstop_is_twice_the_budget() {
        let options = Options {
            memory_mb: 256,
            force_failure: None,
        };
        assert_eq!(
            Settings::from(&options).memory_limit_bytes,
            512 * 1024 * 1024
        );
    }

    #[test]
    fn forced_failure_names_the_stage() {
        let settings = Settings {
            memory_limit_bytes: 1,
            force_failure: Some("seccomp".to_owned()),
        };
        assert!(
            forced(&settings, "seccomp")
                .unwrap_err()
                .to_string()
                .starts_with("seccomp:")
        );
        assert!(forced(&settings, "landlock").is_ok());
    }
}
