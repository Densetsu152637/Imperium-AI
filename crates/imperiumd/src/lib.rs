//! The Imperium daemon: the single process that owns all state. Clients (the CLI, the desktop
//! app, remote hosts) talk to it over JSON-RPC on a local, token-protected WebSocket.

mod approvals;
mod autofix;
mod endpoint;
mod local;
mod mcp;
mod mods;
mod project_agent;
mod rpc;
mod scheduler;
mod server;
mod state;
mod token;

pub use approvals::Approvals;
pub use autofix::fix_ci;
pub use endpoint::EndpointFile;
pub use local::LocalRuntime;
pub use project_agent::ProjectAgents;
pub use rpc::{Connection, Handled, Subscription};
pub use scheduler::Scheduler;
pub use server::serve;
pub use state::{CreateError, DaemonState, SessionOptions};
pub use token::AccessToken;
