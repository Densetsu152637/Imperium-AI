//! Plugins' MCP servers, started on first use. A session begins with each plugin's name and
//! description in the agent's index and none of its servers running; `activate_plugin` (or the
//! plugin being eager, or a resumed session that had activated it) starts them, and their tools
//! are offered from the next model call on, as `mcp__<plugin>_<server>__<tool>`.

use std::sync::{Arc, Mutex, PoisonError};

use imperium_api::{ToolCheck, ToolContext, ToolFuture, ToolGroup, ToolProvider, ToolSpec};
use imperium_mcp::{McpServers, ServerSpec};

/// A plugin whose servers start on activation.
#[derive(Debug, Clone)]
pub struct LazyPlugin {
    pub name: String,
    pub description: String,
    pub servers: Vec<ServerSpec>,
    /// Started with the session instead of on first use.
    pub eager: bool,
}

/// What activating a plugin did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    /// Its servers started now: the tools they offer, and servers that failed, with reasons.
    Started {
        tools: Vec<String>,
        failed: Vec<String>,
    },
    /// It was already active; its tools.
    AlreadyActive { tools: Vec<String> },
}

struct Active {
    name: String,
    servers: Arc<McpServers>,
}

/// The session's plugins with servers, as a [`ToolProvider`] offering the tools of those that
/// are active.
#[derive(Default)]
pub struct PluginTools {
    plugins: Vec<LazyPlugin>,
    active: Mutex<Vec<Active>>,
    /// One activation at a time, so two agents asking for one plugin start it once.
    starting: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for PluginTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginTools")
            .field(
                "plugins",
                &self.plugins.iter().map(|p| &p.name).collect::<Vec<_>>(),
            )
            .field("active", &self.active_names())
            .finish()
    }
}

impl PluginTools {
    pub fn new(plugins: Vec<LazyPlugin>) -> Self {
        PluginTools {
            plugins,
            ..Default::default()
        }
    }

    /// Every plugin with servers, active or not.
    pub fn plugins(&self) -> &[LazyPlugin] {
        &self.plugins
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.lock().iter().any(|active| active.name == name)
    }

    /// The active plugins' names, in activation order.
    pub fn active_names(&self) -> Vec<String> {
        self.lock()
            .iter()
            .map(|active| active.name.clone())
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Active>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a plugin's servers, once. Fails when there is no such plugin or none of its
    /// servers start; a plugin whose servers all failed can be activated again.
    pub async fn activate(&self, name: &str) -> Result<Activation, String> {
        let plugin = self
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .ok_or_else(|| format!("no plugin named {name:?} has tools to activate"))?;
        let _starting = self.starting.lock().await;
        if let Some(tools) = self.tools_of(name) {
            return Ok(Activation::AlreadyActive { tools });
        }
        let (servers, failed) = McpServers::connect(plugin.servers.clone()).await;
        let failed: Vec<String> = failed
            .into_iter()
            .map(|failure| format!("{}: {}", failure.server, failure.reason))
            .collect();
        let tools: Vec<String> = servers.tools().map(|tool| tool.qualified.clone()).collect();
        if failed.len() == plugin.servers.len() && !failed.is_empty() {
            return Err(format!(
                "plugin {name}'s servers did not start: {}",
                failed.join("; ")
            ));
        }
        self.lock().push(Active {
            name: name.to_string(),
            servers: Arc::new(servers),
        });
        Ok(Activation::Started { tools, failed })
    }

    /// Activates the eager plugins and those named in `restore` (activated before a resumed
    /// session's restart). Returns problems worth telling the user about.
    pub async fn start(&self, restore: &[String]) -> Vec<String> {
        let mut notices = Vec::new();
        let names: Vec<String> = self
            .plugins
            .iter()
            .filter(|plugin| plugin.eager || restore.contains(&plugin.name))
            .map(|plugin| plugin.name.clone())
            .collect();
        for name in names {
            match self.activate(&name).await {
                Ok(Activation::Started { failed, .. }) => notices.extend(
                    failed
                        .into_iter()
                        .map(|reason| format!("plugin {name}: MCP server {reason}")),
                ),
                Ok(Activation::AlreadyActive { .. }) => {}
                Err(error) => notices.push(error),
            }
        }
        notices
    }

    /// The tools of an active plugin; `None` when it is not active.
    fn tools_of(&self, name: &str) -> Option<Vec<String>> {
        self.lock()
            .iter()
            .find(|active| active.name == name)
            .map(|active| {
                active
                    .servers
                    .tools()
                    .map(|tool| tool.qualified.clone())
                    .collect()
            })
    }

    /// The servers of the active plugin that offers `tool`.
    fn servers_for(&self, tool: &str) -> Option<Arc<McpServers>> {
        self.lock()
            .iter()
            .find(|active| active.servers.find(tool).is_some())
            .map(|active| Arc::clone(&active.servers))
    }
}

impl ToolProvider for PluginTools {
    fn group(&self) -> ToolGroup {
        ToolGroup::External
    }

    fn specs(&self) -> Vec<ToolSpec> {
        self.lock()
            .iter()
            .flat_map(|active| active.servers.tool_specs())
            .collect()
    }

    fn handles(&self, name: &str) -> bool {
        self.servers_for(name).is_some()
    }

    /// As for any MCP tool: asks first unless the server's `allow` list covers the tool.
    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        let servers = self
            .servers_for(name)
            .ok_or_else(|| format!("unknown tool {name}"))?;
        let tool = servers
            .find(name)
            .ok_or_else(|| format!("unknown tool {name}"))?;
        Ok(ToolCheck {
            summary: format!(
                "{} on MCP server {}: {}",
                tool.name,
                tool.server,
                imperium_api::truncate_chars(arguments, 200)
            ),
            ask: !tool.allowed,
            needs_context: true,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let servers = self
                .servers_for(name)
                .ok_or_else(|| format!("unknown MCP tool {name}"))?;
            servers.call(name, arguments).await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use imperium_mcp::Launch;

    use super::*;

    fn plugin(name: &str, command: &str, eager: bool) -> LazyPlugin {
        LazyPlugin {
            name: name.into(),
            description: "test".into(),
            servers: vec![ServerSpec {
                name: format!("{name}_server"),
                launch: Launch::Stdio {
                    command: command.into(),
                    args: Vec::new(),
                    env: Vec::new(),
                    cwd: None,
                },
                allow: Vec::new(),
                timeout: Duration::from_secs(5),
            }],
            eager,
        }
    }

    #[tokio::test]
    async fn nothing_starts_until_a_plugin_is_activated() {
        let tools = PluginTools::new(vec![plugin(
            "broken",
            "imperium-no-such-command-for-tests",
            false,
        )]);
        assert!(
            tools.start(&[]).await.is_empty(),
            "a lazy plugin starts nothing"
        );
        assert!(!tools.is_active("broken"));
        assert!(tools.specs().is_empty());
        let error = tools.activate("broken").await.unwrap_err();
        assert!(error.contains("did not start"), "{error}");
        assert!(
            !tools.is_active("broken"),
            "a failed activation can be retried"
        );
        assert!(tools.activate("missing").await.is_err());
    }

    #[tokio::test]
    async fn eager_and_restored_plugins_start_with_the_session() {
        let tools = PluginTools::new(vec![
            plugin("eager", "imperium-no-such-command-for-tests", true),
            plugin("restored", "imperium-no-such-command-for-tests", false),
            plugin("lazy", "imperium-no-such-command-for-tests", false),
        ]);
        // Starting fails (no such command), which shows each was attempted, and only those.
        let notices = tools.start(&["restored".into()]).await;
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(notices[0].contains("eager"), "{notices:?}");
        assert!(notices[1].contains("restored"), "{notices:?}");
    }
}
