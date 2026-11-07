//! The Imperium daemon: the single process that owns all state. Clients (the CLI, the desktop
//! app, remote hosts) talk to it over JSON-RPC on a local, token-protected WebSocket.

mod endpoint;
mod local;
mod mcp;
mod mods;
mod rpc;
mod server;
mod state;
mod token;

pub use endpoint::EndpointFile;
pub use local::LocalRuntime;
pub use rpc::{Connection, Handled, Subscription};
pub use server::serve;
pub use state::{CreateError, DaemonState};
pub use token::AccessToken;
