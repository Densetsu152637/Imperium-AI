//! Platforms with no confinement for the worker (neither Linux, macOS nor Windows): the worker
//! refuses to run rather than run unconfined.

use super::{ConfineError, Report, Settings};

pub(super) fn apply(_settings: &Settings) -> Result<Report, ConfineError> {
    Err(ConfineError::new(
        "platform",
        "this platform has no confinement for the worker, so code mode is unavailable here",
    ))
}
