//! Platforms with no self-confinement in the worker (Windows until stage 1b): the worker refuses
//! to run rather than run unconfined.

use super::{ConfineError, Report, Settings};

pub(super) fn apply(_settings: &Settings) -> Result<Report, ConfineError> {
    Err(ConfineError::new(
        "platform",
        "this platform has no worker self-confinement yet; the daemon launches the worker \
         confined on Windows (stage 1b of the code-mode plan)",
    ))
}
