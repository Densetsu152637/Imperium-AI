//! Model Context Protocol client: connects a session to MCP servers (stdio processes and
//! streamable-HTTP endpoints) and exposes their tools to agents as `mcp__<server>__<tool>`,
//! plus tools to list and read a server's resources and prompts
//! (`mcp__<server>__resources_list`, `resources_read`, `prompts_list`, `prompts_get`). A server
//! may also ask the session's model for a reply (sampling) when the user's config allows it.

#![warn(unreachable_pub)]

pub mod client;
mod extras;
pub mod mcp_json;
pub mod oauth;
pub mod sampling;
pub mod servers;
mod tools;

pub use client::{Client, McpError, PROTOCOL_VERSION, TokenFuture, TokenSource};
pub use extras::ToolKind;
pub use sampling::Sampling;
pub use servers::{Failed, Launch, McpServers, McpTool, NEEDS_SIGN_IN, ServerSpec, qualified_name};
pub use tools::McpTools;
