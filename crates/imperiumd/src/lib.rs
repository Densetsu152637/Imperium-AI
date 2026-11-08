//! The Imperium daemon: the single process that owns all state. Clients (the CLI, the desktop
//! app, remote hosts) talk to it over JSON-RPC on a local, token-protected WebSocket.

mod approvals;
mod autofix;
mod config;
mod connectors;
mod downloads;
mod endpoint;
mod local;
mod mcp;
mod mods;
mod pets;
mod phone;
mod project_agent;
mod rpc;
mod scheduler;
mod server;
mod state;
mod suggestions;
mod token;

pub use approvals::Approvals;
pub use autofix::{fix_ci, watch_pull_requests};
pub use downloads::Downloads;
pub use endpoint::EndpointFile;
pub use local::LocalRuntime;
pub use pets::PetEngine;
pub use phone::Devices;
pub use project_agent::ProjectAgents;
pub use rpc::{Connection, Deferred, Handled, Subscription};
pub use scheduler::Scheduler;
pub use server::serve;
pub use state::{CreateError, DaemonState, SessionOptions};
pub use token::AccessToken;
