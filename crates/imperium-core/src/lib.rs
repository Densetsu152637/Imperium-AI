//! Sessions and the agent loop.
//!
//! Every agent (a session's root and each subagent) is its own task on the daemon's
//! multi-threaded Tokio runtime, so agents run in parallel across cores and idle agents use no
//! CPU. Callers talk to a [`Session`] over channels and receive its
//! [`AgentEvent`](imperium_protocol::session::AgentEvent)s by subscribing.

mod agent;
mod journal;
mod local;
mod model;
mod session;

pub use local::{AcquireFuture, Availability, LocalLease, LocalModels};
pub use model::{CatalogModel, ModelError, ResolvedModel, TaskRequest, resolve_model, route};
pub use session::{Session, SessionClosed, SessionConfig, new_session_id};
