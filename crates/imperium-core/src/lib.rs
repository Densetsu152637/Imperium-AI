//! Sessions and the agent loop.
//!
//! Each [`Session`] runs on its own OS thread with a single-threaded Tokio runtime, so a busy
//! session cannot starve others or the daemon's transport. Callers talk to it over channels
//! and receive its [`SessionEvent`](imperium_protocol::session::SessionEvent)s by subscribing.

mod agent;
mod journal;
mod model;
mod session;

pub use model::{ModelError, ResolvedModel, resolve_model};
pub use session::{Session, SessionClosed, SessionConfig, new_session_id};
