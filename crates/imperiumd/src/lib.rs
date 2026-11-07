//! The Imperium daemon: the single process that owns all state. Clients (the CLI, the desktop
//! app, remote hosts) talk to it over JSON-RPC on a local, token-protected WebSocket.

mod endpoint;
mod rpc;
mod server;
mod token;

pub use endpoint::EndpointFile;
pub use rpc::Connection;
pub use server::serve;
pub use token::AccessToken;
