//! Reads `.mcp.json`, the project server file Claude Code and other hosts use:
//!
//! ```json
//! { "mcpServers": {
//!     "files": { "command": "npx", "args": ["-y", "@scope/server"], "env": { "KEY": "${KEY}" } },
//!     "docs":  { "type": "http", "url": "https://example.com/mcp", "headers": { "X": "y" } } } }
//! ```
//!
//! `${VAR}` and `${VAR:-default}` are expanded from the environment in commands, arguments,
//! environment values, URLs, and headers.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::servers::{Launch, ServerSpec};

#[derive(Deserialize)]
struct File {
    #[serde(rename = "mcpServers", default)]
    servers: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

/// Parses `.mcp.json` text. Stdio servers run in `project_dir`. `env` looks up variables.
pub fn parse(
    text: &str,
    project_dir: &Path,
    timeout: Duration,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Vec<ServerSpec>, String> {
    let file: File = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let expand = |text: &str| expand(text, &env);
    file.servers
        .into_iter()
        .map(|(name, entry)| {
            let launch = match (entry.kind.as_deref(), entry.command, entry.url) {
                (None | Some("stdio"), Some(command), _) => Launch::Stdio {
                    command: expand(&command),
                    args: entry.args.iter().map(|arg| expand(arg)).collect(),
                    env: entry
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), expand(value)))
                        .collect(),
                    cwd: Some(project_dir.to_path_buf()),
                },
                (None | Some("http" | "streamable-http"), None, Some(url)) => Launch::Http {
                    url: expand(&url),
                    headers: entry
                        .headers
                        .iter()
                        .map(|(key, value)| (key.clone(), expand(value)))
                        .collect(),
                },
                (Some("sse"), ..) => {
                    return Err(format!(
                        "{name}: the legacy SSE transport is not supported; use http"
                    ));
                }
                _ => return Err(format!("{name}: needs a command or a url")),
            };
            Ok(ServerSpec {
                name,
                launch,
                allow: Vec::new(),
                timeout,
            })
        })
        .collect()
}

/// Expands `${VAR}` and `${VAR:-default}`; unknown variables without a default become empty.
fn expand(text: &str, env: &impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expression = &after[..end];
        let (name, default) = match expression.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (expression, None),
        };
        out.push_str(
            &env(name)
                .or(default.map(str::to_string))
                .unwrap_or_default(),
        );
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_and_http_servers_parse_with_expansion() {
        let text = r#"{"mcpServers": {
            "files": {"command": "npx", "args": ["-y", "server", "${ROOT:-.}"], "env": {"TOKEN": "${TOKEN}"}},
            "docs": {"type": "http", "url": "https://${HOST}/mcp", "headers": {"Authorization": "Bearer ${TOKEN}"}}
        }}"#;
        let env = |name: &str| match name {
            "TOKEN" => Some("t".to_string()),
            "HOST" => Some("example.com".to_string()),
            _ => None,
        };
        let specs = parse(text, Path::new("/p"), Duration::from_secs(5), env).unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(
            specs[0].launch,
            Launch::Http {
                url: "https://example.com/mcp".into(),
                headers: vec![("Authorization".into(), "Bearer t".into())],
            }
        );
        let Launch::Stdio { args, env, cwd, .. } = &specs[1].launch else {
            panic!("files is a stdio server");
        };
        assert_eq!(args, &["-y", "server", "."]);
        assert_eq!(env, &[("TOKEN".to_string(), "t".to_string())]);
        assert_eq!(cwd.as_deref(), Some(Path::new("/p")));
    }

    #[test]
    fn unusable_entries_are_explained() {
        let none = |_: &str| None;
        let sse = r#"{"mcpServers": {"old": {"type": "sse", "url": "http://x"}}}"#;
        assert!(
            parse(sse, Path::new("."), Duration::ZERO, none)
                .unwrap_err()
                .contains("SSE")
        );
        let empty = r#"{"mcpServers": {"bad": {}}}"#;
        assert!(parse(empty, Path::new("."), Duration::ZERO, none).is_err());
        assert_eq!(expand("a${MISSING}b${UNCLOSED", &none), "ab${UNCLOSED");
    }
}
