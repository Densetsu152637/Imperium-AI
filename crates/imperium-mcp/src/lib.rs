//! Model Context Protocol client: connects a session to MCP servers (stdio processes and
//! streamable-HTTP endpoints) and exposes their tools to agents as `mcp__<server>__<tool>`.

pub mod client;
pub mod mcp_json;
pub mod servers;
mod tools;

pub use client::{Client, McpError, PROTOCOL_VERSION};
pub use servers::{Failed, Launch, McpServers, McpTool, ServerSpec, qualified_name};
pub use tools::McpTools;
