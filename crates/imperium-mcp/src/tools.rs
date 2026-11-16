//! A session's MCP servers as a [`ToolProvider`]: started before the session's first turn, their
//! tools offered as `mcp__<server>__<tool>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use imperium_api::{
    StartFuture, ToolCheck, ToolContext, ToolFuture, ToolGroup, ToolProvider, ToolSpec,
    truncate_chars,
};

use crate::{McpServers, ServerSpec, TokenSource};

/// MCP servers for one session. Until [`ToolProvider::start`] connects them it offers no tools.
#[derive(Debug, Default)]
pub struct McpTools {
    /// Servers waiting to be started.
    pending: Mutex<Vec<ServerSpec>>,
    /// OAuth access tokens for the HTTP servers the user signs in to, by server name.
    auths: HashMap<String, Arc<dyn TokenSource>>,
    servers: OnceLock<McpServers>,
}

impl McpTools {
    pub fn new(specs: Vec<ServerSpec>) -> Self {
        McpTools {
            pending: Mutex::new(specs),
            auths: HashMap::new(),
            servers: OnceLock::new(),
        }
    }

    /// Gives the named HTTP servers OAuth access tokens.
    #[must_use]
    pub fn with_auth(mut self, auths: HashMap<String, Arc<dyn TokenSource>>) -> Self {
        self.auths = auths;
        self
    }

    fn servers(&self) -> Option<&McpServers> {
        self.servers.get()
    }
}

impl ToolProvider for McpTools {
    fn group(&self) -> ToolGroup {
        ToolGroup::External
    }

    /// Starts every server concurrently; each that fails becomes a notice.
    fn start(&self) -> StartFuture<'_> {
        Box::pin(async move {
            let specs =
                std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner));
            if specs.is_empty() {
                return Vec::new();
            }
            let (servers, failed) = McpServers::connect_with(specs, self.auths.clone()).await;
            let _ = self.servers.set(servers);
            failed
                .into_iter()
                .map(|failure| {
                    format!(
                        "MCP server {} is unavailable: {}",
                        failure.server, failure.reason
                    )
                })
                .collect()
        })
    }

    fn specs(&self) -> Vec<ToolSpec> {
        self.servers()
            .map(McpServers::tool_specs)
            .unwrap_or_default()
    }

    fn handles(&self, name: &str) -> bool {
        self.servers()
            .is_some_and(|servers| servers.find(name).is_some())
    }

    fn server_of(&self, name: &str) -> Option<String> {
        self.servers()
            .and_then(|servers| servers.find(name))
            .map(|tool| tool.server.clone())
    }

    /// Asks first unless the server's `allow` list covers the tool. MCP tools may act on
    /// anything, so they wait for required context.
    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        let tool = self
            .servers()
            .and_then(|servers| servers.find(name))
            .ok_or_else(|| format!("unknown tool {name}"))?;
        Ok(ToolCheck {
            summary: format!(
                "{} on MCP server {}: {}",
                tool.name,
                tool.server,
                truncate_chars(arguments, 200)
            ),
            ask: !tool.allowed,
            needs_context: true,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let servers = self
                .servers()
                .ok_or_else(|| format!("unknown MCP tool {name}"))?;
            servers.call(name, arguments, context.sampler).await
        })
    }
}
