//! A session's MCP servers: starting them, listing their tools, and calling them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use imperium_api::{Sampler, SamplingFuture, SamplingRequest, ToolSpec};
use serde_json::{Value, json};
use tokio::process::{Child, Command};

use crate::client::{Client, McpError, PROTOCOL_VERSION, Sampling, TokenSource};
use crate::extras::{self, ToolKind};
use crate::sampling::MAX_REQUESTS_PER_CALL;

/// How to reach a server.
#[derive(Clone, PartialEq, Eq)]
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

/// Names, never values: what `Debug` shows of a launch's environment and headers, which may hold
/// secrets.
struct Hidden<'a>(Vec<&'a str>);

impl std::fmt::Debug for Hidden<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|name| (name, &"<hidden>")))
            .finish()
    }
}

/// `Debug` shows the names of the environment and headers but not their values.
impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Launch::Stdio {
                command,
                args,
                env,
                cwd,
            } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field(
                    "env",
                    &Hidden(env.iter().map(|(key, _)| key.as_str()).collect()),
                )
                .field("cwd", cwd)
                .finish(),
            Launch::Http { url, headers } => f
                .debug_struct("Http")
                .field("url", url)
                .field(
                    "headers",
                    &Hidden(headers.iter().map(|(key, _)| key.as_str()).collect()),
                )
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    /// Unique per session; tools are named `mcp__<name>__<tool>`.
    pub name: String,
    pub launch: Launch,
    /// Tools that run without asking (by MCP tool name, or `*` for all).
    pub allow: Vec<String>,
    pub timeout: Duration,
    /// Whether the server may ask the session's model to write replies (sampling). Each
    /// request still waits for the user's approval; off unless the user's own config turns it on.
    pub sampling: bool,
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
    /// One of the server's own tools, or an extra tool for its resources or prompts.
    pub kind: ToolKind,
}

/// The sampler for one tool call: a few requests at most, and none after one was declined or
/// failed.
struct Limited<'a> {
    inner: &'a dyn Sampler,
    asked: AtomicUsize,
    stopped: AtomicBool,
}

impl Sampler for Limited<'_> {
    fn sample<'a>(&'a self, request: SamplingRequest) -> SamplingFuture<'a> {
        Box::pin(async move {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(
                    "sampling was stopped for this call after an earlier request was \
                            declined or failed"
                        .into(),
                );
            }
            if self.asked.fetch_add(1, Ordering::SeqCst) >= MAX_REQUESTS_PER_CALL {
                self.stopped.store(true, Ordering::SeqCst);
                return Err(format!(
                    "at most {MAX_REQUESTS_PER_CALL} sampling requests are allowed per tool call"
                ));
            }
            let reply = self.inner.sample(request).await;
            if reply.is_err() {
                self.stopped.store(true, Ordering::SeqCst);
            }
            reply
        })
    }
}

struct Server {
    name: String,
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
        McpServers::connect_with(specs, HashMap::new()).await
    }

    /// Like [`McpServers::connect`], giving each HTTP server named in `auths` the OAuth access
    /// tokens of its [`TokenSource`]. Connecting never starts a sign-in: a server that needs one
    /// fails with a reason that says so.
    pub async fn connect_with(
        specs: Vec<ServerSpec>,
        auths: HashMap<String, Arc<dyn TokenSource>>,
    ) -> (Self, Vec<Failed>) {
        let attempts = futures_util::future::join_all(specs.into_iter().map(|spec| {
            let auth = auths.get(&spec.name).cloned();
            async move {
                let name = spec.name.clone();
                (name, start(spec, auth).await)
            }
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
    pub async fn call(
        &self,
        qualified: &str,
        arguments: &str,
        sampler: Option<&dyn Sampler>,
    ) -> Result<String, String> {
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
        let limited = sampler.map(|inner| Limited {
            inner,
            asked: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
        });
        let sampling = limited.as_ref().map(|sampler| Sampling {
            sampler,
            server: &server.name,
        });
        let ask = |method: &'static str, params: Value| {
            server.client.request_with(method, params, sampling)
        };
        let fail = |error: McpError| error.to_string();
        let page = |arguments: &Value| match arguments.get("cursor").and_then(Value::as_str) {
            Some(cursor) => json!({"cursor": cursor}),
            None => json!({}),
        };
        match tool.kind {
            ToolKind::Remote => {
                let result = ask(
                    "tools/call",
                    json!({"name": tool.name, "arguments": arguments}),
                )
                .await
                .map_err(fail)?;
                let text = content_text(&result);
                if result.get("isError").and_then(Value::as_bool) == Some(true) {
                    Err(text)
                } else {
                    Ok(text)
                }
            }
            ToolKind::ResourcesList => {
                let params = page(&arguments);
                let first = params.get("cursor").is_none();
                let listed = ask("resources/list", params).await.map_err(fail)?;
                // Templates come with the first page; a server without them is not an error.
                let templates = if first {
                    ask("resources/templates/list", json!({})).await.ok()
                } else {
                    None
                };
                Ok(extras::frame(
                    "listing",
                    &server.name,
                    ("of", "resources"),
                    &extras::resources_listing(&listed, templates.as_ref()),
                ))
            }
            ToolKind::ResourcesRead => {
                let uri = arguments
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|uri| !uri.is_empty())
                    .ok_or("uri is required")?;
                let read = ask("resources/read", json!({"uri": uri}))
                    .await
                    .map_err(fail)?;
                Ok(extras::frame(
                    "resource",
                    &server.name,
                    ("uri", uri),
                    &extras::resource_text(&read),
                ))
            }
            ToolKind::PromptsList => {
                let listed = ask("prompts/list", page(&arguments)).await.map_err(fail)?;
                Ok(extras::frame(
                    "listing",
                    &server.name,
                    ("of", "prompts"),
                    &extras::prompts_listing(&listed),
                ))
            }
            ToolKind::PromptsGet => {
                let name = arguments
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or("name is required")?;
                // Prompt arguments are text; other JSON values are sent as their JSON.
                let given: serde_json::Map<String, Value> = arguments
                    .get("arguments")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .map(|(key, value)| {
                        let text = value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_string);
                        (key.clone(), Value::String(text))
                    })
                    .collect();
                let got = ask("prompts/get", json!({"name": name, "arguments": given}))
                    .await
                    .map_err(fail)?;
                Ok(extras::frame(
                    "prompt",
                    &server.name,
                    ("name", name),
                    &extras::prompt_text(&got),
                ))
            }
        }
    }
}

/// Set in the environment of every stdio server a session starts. `imperium mcp-serve` refuses
/// to run when it is set (unless it serves a remote daemon), whatever the command line looked
/// like. (The CLI keeps a copy of the name.)
pub const CHILD_MARK: &str = "IMPERIUM_MCP_CHILD";

/// A reserved entry of a stdio server's `env`: when present the server starts with a cleared
/// environment plus [`plugin_child_env`] and the rest of its `env`, not the daemon's. The daemon
/// sets it for the servers of plugins under `<config>/plugins`; it is never passed on.
pub const CLEAN_ENV: &str = "IMPERIUM_PLUGIN_CLEAN_ENV";

/// The mark every process the daemon spawns carries (`imperium_tools::SPAWNED_MARK`).
const SPAWNED_MARK: &str = "IMPERIUM_SPAWNED_BY_DAEMON";

/// The only variables of the daemon's own environment a plugin's hook or stdio server inherits:
/// `PATH`, `HOME` or `USERPROFILE`, `SYSTEMROOT`, `TEMP`, `TMP`, `LANG`, `LC_*`, `TERM`, and on
/// Windows `APPDATA`, `LOCALAPPDATA`, `PATHEXT` and `COMSPEC`; and the spawned-by-daemon mark.
/// Nothing else: not `IMPERIUM_*`, the daemon's token, or any secret in its environment.
pub fn plugin_child_env() -> Vec<(String, String)> {
    let allowed = |name: &str| {
        let upper = name.to_ascii_uppercase();
        matches!(
            upper.as_str(),
            "PATH" | "HOME" | "USERPROFILE" | "SYSTEMROOT" | "TEMP" | "TMP" | "LANG" | "TERM"
        ) || upper.starts_with("LC_")
            || (cfg!(windows)
                && matches!(
                    upper.as_str(),
                    "APPDATA" | "LOCALAPPDATA" | "PATHEXT" | "COMSPEC"
                ))
    };
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| allowed(key))
        .collect();
    env.push((SPAWNED_MARK.to_string(), "1".to_string()));
    env
}

/// Whether a stdio launch runs `imperium mcp-serve` (the harness or the tools bridge): the
/// `imperium` binary, in any spelling of its path or `.exe`, and the `mcp-serve` word, in the
/// arguments or inside a single shell-style argument such as `imperium mcp-serve --tools`.
pub fn is_own_bridge(command: &str, args: &[String]) -> bool {
    let words: Vec<String> = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .flat_map(str::split_whitespace)
        .map(|word| {
            let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
            let name = name
                .trim_matches(|c| c == '"' || c == '\'')
                .to_ascii_lowercase();
            name.strip_suffix(".exe").unwrap_or(&name).to_string()
        })
        .collect();
    words
        .iter()
        .any(|word| word == "imperium" || word == "imperium-cli")
        && words.iter().any(|word| word == "mcp-serve")
}

/// What a server reports when it needs a sign-in the user has not done (or that expired).
pub const NEEDS_SIGN_IN: &str = "needs sign-in";

async fn start(spec: ServerSpec, auth: Option<Arc<dyn TokenSource>>) -> Result<Server, String> {
    let (client, child) = match &spec.launch {
        Launch::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            // Imperium's own server bridge is for outside agents. A session that connected to
            // it would call back into the daemon it runs in.
            if is_own_bridge(command, args) {
                return Err(format!(
                    "{} is Imperium's own MCP server (`imperium mcp-serve`), which is for other \
                     agents to use Imperium: it is not started from inside Imperium, so tool \
                     calls cannot loop back into this daemon",
                    spec.name
                ));
            }
            let mut process = Command::new(command);
            if env.iter().any(|(key, _)| key == CLEAN_ENV) {
                process.env_clear().envs(plugin_child_env());
            }
            process
                .args(args)
                .envs(
                    env.iter()
                        .filter(|(key, _)| key != CLEAN_ENV)
                        .map(|(key, value)| (key, value)),
                )
                // Set after the configured variables, so they cannot clear it: a started
                // `imperium mcp-serve` sees it and refuses to serve its own daemon.
                .env(CHILD_MARK, "1")
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
            Client::over_http_with(url.clone(), headers.clone(), spec.timeout, auth.clone()),
            None,
        ),
    };
    if spec.sampling {
        client.allow_sampling();
    }
    let tools = initialize(&client, &spec)
        .await
        .map_err(|error| match error {
            McpError::AuthRequired(_) if auth.is_some() => format!(
                "{NEEDS_SIGN_IN}: run `imperium mcp login {}` or use Sign in on the server in Connectors",
                spec.name
            ),
            McpError::AuthRequired(_) => {
                "the server answered 401: its credentials were refused".to_string()
            }
            other => other.to_string(),
        })?;
    Ok(Server {
        name: spec.name,
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
                "capabilities": if spec.sampling { json!({"sampling": {}}) } else { json!({}) },
                "clientInfo": {"name": "imperium", "version": env!("CARGO_PKG_VERSION")},
            }),
        )
        .await?;
    let Some(capabilities) = init.get("capabilities") else {
        return Err(McpError::Protocol(
            "initialize returned no capabilities".into(),
        ));
    };
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
                kind: ToolKind::Remote,
            });
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    // A server that offers resources or prompts gets tools to list and read them. They are
    // named like its own tools, so the agent's MCP allowlist, the local-files lock, the
    // permission rules, and the server's `allow` list govern them the same way.
    let extra = [
        (
            "resources",
            [ToolKind::ResourcesList, ToolKind::ResourcesRead],
        ),
        ("prompts", [ToolKind::PromptsList, ToolKind::PromptsGet]),
    ];
    for (capability, kinds) in extra {
        if capabilities.get(capability).is_none() {
            continue;
        }
        for kind in kinds {
            let Some(name) = kind.tool_name() else {
                continue;
            };
            let qualified = qualified_name(&spec.name, name);
            // A tool of the server's own with the same name wins.
            if tools.iter().any(|tool| tool.qualified == qualified) {
                continue;
            }
            tools.push(McpTool {
                server: spec.name.clone(),
                name: name.to_string(),
                qualified,
                description: kind.description().to_string(),
                input_schema: kind.schema(),
                allowed: spec
                    .allow
                    .iter()
                    .any(|allowed| allowed == "*" || allowed == name),
                kind,
            });
        }
    }
    Ok(tools)
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
    fn imperiums_own_server_is_not_started_from_inside_imperium() {
        let own = |command: &str, args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            is_own_bridge(command, &args)
        };
        assert!(own("imperium", &["mcp-serve"]));
        assert!(own(
            "imperium",
            &["--remote", "box", "mcp-serve", "--tools"]
        ));
        assert!(own(
            "C:\\Program Files\\Imperium\\imperium.exe",
            &["mcp-serve"]
        ));
        assert!(own("/usr/local/bin/imperium", &["mcp-serve", "--tools"]));
        assert!(own("cmd", &["/c", "imperium mcp-serve --tools"]));
        assert!(!own("imperium", &["status"]));
        assert!(!own(
            "npx",
            &["-y", "@modelcontextprotocol/server-filesystem"]
        ));
        assert!(!own("other-server", &["mcp-serve"]));
    }

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
