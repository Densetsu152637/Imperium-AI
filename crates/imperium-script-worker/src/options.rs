//! The worker's command line. The environment is empty by design, so everything the daemon
//! tells the worker before the protocol starts travels as arguments.

use std::fmt;

/// The default `script_memory_mb` (plan C5), used when the daemon passes none.
pub const DEFAULT_MEMORY_MB: u64 = 256;

/// The smallest `--memory-mb`: below it the runtime itself cannot start inside the OS backstop.
pub const MIN_MEMORY_MB: u64 = 32;

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// `script_memory_mb`: the interpreter's own limit is set over the protocol; this sizes the OS
    /// backstop (twice this, as `RLIMIT_DATA` on Unix).
    pub memory_mb: u64,
    /// Test hook: fail the named confinement stage instead of applying it.
    pub force_failure: Option<String>,
    /// Test hook: skip `close_range` so the fallback descriptor loop runs.
    pub skip_close_range: bool,
}

/// An argument the worker does not accept.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

impl Options {
    /// Parses the arguments after the program name.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, UsageError> {
        let mut options = Self {
            memory_mb: DEFAULT_MEMORY_MB,
            force_failure: None,
            skip_close_range: false,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--memory-mb" => {
                    let value = args
                        .next()
                        .ok_or_else(|| UsageError("--memory-mb needs a value".to_owned()))?;
                    options.memory_mb = value
                        .parse::<u64>()
                        .ok()
                        .filter(|mb| (MIN_MEMORY_MB..=1_048_576).contains(mb))
                        .ok_or_else(|| {
                            UsageError(format!(
                                "--memory-mb: {value:?} is not a size in {MIN_MEMORY_MB}..=1048576"
                            ))
                        })?;
                }
                #[cfg(feature = "test-support")]
                "--force-confinement-failure" => {
                    options.force_failure = Some(args.next().ok_or_else(|| {
                        UsageError("--force-confinement-failure needs a stage".to_owned())
                    })?);
                }
                #[cfg(feature = "test-support")]
                "--skip-close-range" => options.skip_close_range = true,
                other => return Err(UsageError(format!("unknown argument {other:?}"))),
            }
        }
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, UsageError> {
        Options::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn defaults_and_memory() {
        assert_eq!(parse(&[]).unwrap().memory_mb, DEFAULT_MEMORY_MB);
        assert_eq!(parse(&["--memory-mb", "64"]).unwrap().memory_mb, 64);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&["--memory-mb"]).is_err());
        assert!(parse(&["--memory-mb", "0"]).is_err());
        assert!(parse(&["--memory-mb", "31"]).is_err());
        assert_eq!(parse(&["--memory-mb", "32"]).unwrap().memory_mb, 32);
        assert!(parse(&["--memory-mb", "lots"]).is_err());
        assert!(parse(&["subprocess"]).is_err());
    }
}
