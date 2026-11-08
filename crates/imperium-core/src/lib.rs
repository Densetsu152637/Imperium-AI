//! Sessions and the agent loop.
//!
//! Every agent (a session's root and each subagent) is its own task on the daemon's
//! multi-threaded Tokio runtime, so agents run in parallel across cores and idle agents use no
//! CPU. Callers talk to a [`Session`] over channels and receive its
//! [`AgentEvent`](imperium_protocol::session::AgentEvent)s by subscribing.

mod agent;
mod breakdown;
mod journal;
mod launcher;
mod model;
mod session;
mod status;

pub use imperium_api::{AcquireFuture, Availability, LocalLease, LocalModels};
pub use launcher::{LaunchFuture, SessionLauncher};
pub use model::{CatalogModel, ModelError, ResolvedModel, TaskRequest, resolve_model, route};
pub use session::{
    BrowserSetup, Session, SessionClosed, SessionConfig, SessionMemory, WebTools, new_session_id,
};
pub use status::next_attempt;

/// The resume policy used when nothing is configured: wait for the provider's reset, retrying
/// every five minutes, for up to twelve hours.
pub fn default_resume_policy() -> imperium_protocol::session::ResumePolicy {
    imperium_protocol::session::ResumePolicy {
        mode: "wait_for_reset".into(),
        at_time: None,
        retry_every_minutes: 5,
        max_wait_hours: 12,
    }
}
