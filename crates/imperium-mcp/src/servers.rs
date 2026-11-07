//! A session's MCP servers: starting them, listing their tools, and calling them.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use imperium_models::ToolSpec;
use serde_json::{Value, json};
use tokio::process::{Child, Command};

use crate::client::{Client, McpError, PROTOCOL_VERSION};

/// How to reach a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// A local process speaking MCP on stdin and stdout.
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<PathBuf>,
    },
    /// A streamable-HTTP endpoint.
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    /// Unique per session; tools are named `mcp__<name>__<tool>`.
    pub name: String,
    pub launch: Launch,
    /// Tools that run without asking (by MCP tool name, or `*` for all).
    pub allow: Vec<String>,
    pub timeout: Duration,
}

/// A tool offered by a connected server.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub server: String,
    pub name: String,
    /// `mcp__<server>__<tool>`, the name models call.
    pub qualified: String,
    pub description: String,
    pub input_schema: Value,
    /// Runs without the user's approval (from the server's `allow` list).
    pub allowed: bool,
}

struct Server {
    client: Client,
    tools: Vec<McpTool>,
    /// The server process for stdio servers; dropping it stops the server.
    _child: Option<Child>,
}

/// The MCP servers connected for one session.
#[derive(Default)]
pub struct McpServers {
    servers: Vec<Server>,
}

impl std::fmt::Debug for McpServers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServers")
            .field(
                "tools",
                &self.tools().map(|tool| &tool.qualified).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// A server that could not be started, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub server: String,
    pub reason: String,
}

impl McpServers {
    /// Starts every server concurrently. Servers that fail are reported, not fatal.
    pub async fn connect(specs: Vec<ServerSpec>) -> (Self, Vec<Failed>) {
        let attempts = futures_util::future::join_all(specs.into_iter().map(|spec| async move {
            let name = spec.name.clone();
            (name, start(spec).await)
        }))
        .await;
        let mut servers = McpServers::default();
        let mut failed = Vec::new();
        for (server, attempt) in attempts {
            match attempt {
                Ok(connected) => servers.servers.push(connected),
                Err(reason) => failed.push(Failed { server, reason }),
            }
        }
        (servers, failed)
    }

    pub fn tools(&self) -> impl Iterator<Item = &McpTool> {
        self.servers.iter().flat_map(|server| &server.tools)
    }

    /// Tool definitions for the model.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools()
            .map(|tool| ToolSpec {
                name: tool.qualified.clone(),
                description: format!("[MCP {}] {}", tool.server, tool.description),
                parameters: tool.input_schema.clone(),
            })
            .collect()
    }

    pub fn find(&self, qualified: &str) -> Option<&McpTool> {
        self.tools().find(|tool| tool.qualified == qualified)
    }

    /// Calls a tool with JSON `arguments` and returns its content as text; a tool-reported
    /// error is returned as `Err`.
    pub async fn call(&self, qualified: &str, arguments: &str) -> Result<String, String> {
        let (server, tool) = self
            .servers
            .iter()
            .find_map(|server| {
                let tool = server
                    .tools
                    .iter()
                    .find(|tool| tool.qualified == qualified)?;
                Some((server, tool))
            })
            .ok_or_else(|| format!("unknown MCP tool {qualified}"))?;
        let arguments: Value = if arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments)
                .map_err(|error| format!("invalid arguments: {error}"))?
        };
        let result = server
            .client
            .request(
                "tools/call",
                json!({"name": tool.name, "arguments": arguments}),
            )
            .await
            .map_err(|error| error.to_string())?;
        let text = content_text(&result);
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

async fn start(spec: ServerSpec) -> Result<Server, String> {
    let (client, child) = match &spec.launch {
        Launch::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            let mut process = Command::new(command);
            process
                .args(args)
                .envs(env.iter().map(|(key, value)| (key, value)))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            if let Some(cwd) = cwd {
                process.current_dir(cwd);
            }
            let mut child = process
                .spawn()
                .map_err(|error| format!("could not start {command}: {error}"))?;
            let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                return Err("the server's standard streams are unavailable".into());
            };
            (
                Client::over_stream(stdout, stdin, spec.timeout),
                Some(child),
            )
        }
        Launch::Http { url, headers } => (
            Client::over_http(url.clone(), headers.clone(), spec.timeout),
            None,
        ),
    };
    let tools = initialize(&client, &spec)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Server {
        client,
        tools,
        _child: child,
    })
}

async fn initialize(client: &Client, spec: &ServerSpec) -> Result<Vec<McpTool>, McpError> {
    let init = client
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "imperium", "version": env!("CARGO_PKG_VERSION")},
            }),
        )
        .await?;
    if init.get("capabilities").is_none() {
        return Err(McpError::Protocol(
            "initialize returned no capabilities".into(),
        ));
    }
    client
        .notify("notifications/initialized", json!({}))
        .await?;
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |c| json!({"cursor": c}));
        let page = client.request("tools/list", params).await?;
        for tool in page
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            tools.push(McpTool {
                server: spec.name.clone(),
                name: name.to_string(),
                qualified: qualified_name(&spec.name, name),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
                allowed: spec
                    .allow
                    .iter()
                    .any(|allowed| allowed == "*" || allowed == name),
            });
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            return Ok(tools);
        }
    }
}

/// `mcp__<server>__<tool>`, with characters model APIs reject replaced and the length capped
/// at 64.
pub fn qualified_name(server: &str, tool: &str) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let mut name = format!("mcp__{}__{}", clean(server), clean(tool));
    name.truncate(64);
    name
}

/// The text of a tool result's content blocks; non-text blocks are described.
fn content_text(result: &Value) -> String {
    let blocks = result.get("content").and_then(Value::as_array);
    let mut parts: Vec<String> = blocks
        .into_iter()
        .flatten()
        .map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Some("resource") => block
                .pointer("/resource/text")
                .and_then(Value::as_str)
                .map_or_else(
                    || {
                        format!(
                            "[resource {}]",
                            block.pointer("/resource/uri").unwrap_or(&Value::Null)
                        )
                    },
                    str::to_string,
                ),
            Some(kind) => format!(
                "[{kind} {}]",
                block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("content")
            ),
            None => String::new(),
        })
        .collect();
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent")
    {
        parts.push(structured.to_string());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_qualified_and_sanitised() {
        assert_eq!(
            qualified_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(qualified_name("my server", "a.b"), "mcp__my_server__a_b");
        assert_eq!(qualified_name("s", &"x".repeat(100)).len(), 64);
    }

    #[test]
    fn content_blocks_become_text() {
        let result = json!({"content": [
            {"type": "text", "text": "hello"},
            {"type": "image", "mimeType": "image/png", "data": "..."},
            {"type": "resource", "resource": {"uri": "file:///a", "text": "inline"}},
        ]});
        assert_eq!(content_text(&result), "hello\n[image image/png]\ninline");
        assert_eq!(
            content_text(&json!({"content": [], "structuredContent": {"n": 1}})),
            r#"{"n":1}"#
        );
    }
}
