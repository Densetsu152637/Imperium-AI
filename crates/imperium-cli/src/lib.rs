//! Client side of the daemon protocol, used by the `imperium` command.

#![warn(unreachable_pub)]

pub use imperium_client as client;
pub mod eval;
pub mod mcp_bridge;
pub mod mcp_serve;
pub mod pr;
