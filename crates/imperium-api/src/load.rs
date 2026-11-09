//! How busy the machine is, as the daemon sees it, so background work can yield to the user.

use std::fmt;

/// The machine's load as autonomous work should treat it. Ordered: `Quiet < Busy < Saturated`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Load {
    /// Nobody is using the machine: background work may run.
    #[default]
    Quiet,
    /// The user's sessions are working, or the machine is under load: background work that is
    /// running pauses at its next step boundary and scheduled background work waits to start.
    Busy,
    /// Interactive model calls are queueing, models are swapping, or memory is short:
    /// background model calls wait.
    Saturated,
}

impl Load {
    pub fn label(self) -> &'static str {
        match self {
            Load::Quiet => "quiet",
            Load::Busy => "busy",
            Load::Saturated => "saturated",
        }
    }
}

/// The daemon's current load level. Implemented by the daemon's load monitor; tests supply
/// their own.
pub trait LoadView: Send + Sync + fmt::Debug {
    fn level(&self) -> Load;
}
