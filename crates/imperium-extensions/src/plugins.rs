//! Plugins in the Claude Code layout: a folder with `.claude-plugin/plugin.json` contributing
//! `skills/`, `commands/`, `hooks/hooks.json`, and `.mcp.json`. `${CLAUDE_PLUGIN_ROOT}` in
//! hook commands and server definitions is the plugin folder. `agents/` is reported as not yet
//! supported.
//!
//! `${VAR}` in `.mcp.json` falls back to the daemon's environment, except for a plugin an agent
//! installed (its folder holds the `.imperium-source.json` the daemon writes, naming the
//! session): downloaded content must not read the daemon's secrets into a server's command,
//! arguments, headers, or URL, so there only `CLAUDE_PLUGIN_ROOT` expands and any other
//! variable is empty (or its `:-default`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use imperium_mcp::{ServerSpec, mcp_json};
use serde::Deserialize;

use crate::hooks::{Hook, parse_hooks_json};

/// The provenance file the daemon writes into each extension an agent installs.
pub const SOURCE_FILE: &str = ".imperium-source.json";

#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    /// The manifest's description; empty when it has none.
    pub description: String,
    pub root: PathBuf,
    pub hooks: Vec<Hook>,
    pub servers: Vec<ServerSpec>,
}

impl Plugin {
    pub fn origin(&self) -> String {
        format!("plugin:{}", self.name)
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    pub fn commands_dir(&self) -> PathBuf {
        self.root.join("commands")
    }
}

#[derive(Deserialize)]
struct Manifest {
    name: String,
    #[serde(default)]
    description: String,
}

/// Loads each plugin folder in `dirs` (folders that themselves hold plugins are searched one
/// level down). Returns the plugins and problems worth telling the user about.
pub fn discover(dirs: &[PathBuf], server_timeout: Duration) -> (Vec<Plugin>, Vec<String>) {
    let mut roots = Vec::new();
    for dir in dirs {
        if is_plugin(dir) {
            roots.push(dir.clone());
        } else if let Ok(entries) = std::fs::read_dir(dir) {
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| is_plugin(path))
                .collect();
            children.sort();
            roots.extend(children);
        }
    }
    let mut plugins = Vec::new();
    let mut problems = Vec::new();
    for root in roots {
        match load(&root, server_timeout) {
            Ok((plugin, mut notes)) => {
                problems.append(&mut notes);
                if plugins.iter().any(|p: &Plugin| p.name == plugin.name) {
                    problems.push(format!(
                        "plugin {} at {} is ignored: the name is already used",
                        plugin.name,
                        root.display()
                    ));
                } else {
                    plugins.push(plugin);
                }
            }
            Err(error) => problems.push(format!("plugin at {}: {error}", root.display())),
        }
    }
    (plugins, problems)
}

/// Whether an agent installed the plugin in `root`: its provenance file records a session. A
/// provenance file that cannot be read or parsed counts as agent-installed, so a damaged record
/// never widens what the plugin may read.
fn installed_by_agent(root: &Path) -> bool {
    let path = root.join(SOURCE_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return path.exists();
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => value
            .get("session_id")
            .is_some_and(|session| !session.is_null()),
        Err(_) => true,
    }
}

fn is_plugin(dir: &Path) -> bool {
    dir.join(".claude-plugin").join("plugin.json").is_file()
}

fn load(root: &Path, server_timeout: Duration) -> Result<(Plugin, Vec<String>), String> {
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(root.join(".claude-plugin/plugin.json"))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("plugin.json: {error}"))?;
    let origin = format!("plugin:{}", manifest.name);
    let mut notes = Vec::new();
    let mut hooks = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join("hooks/hooks.json")) {
        let (loaded, mut skipped) = parse_hooks_json(&text, root, &origin)?;
        hooks = loaded;
        notes.append(&mut skipped);
    }
    let mut servers = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join(".mcp.json")) {
        let plugin_root = root.display().to_string();
        let local_only = installed_by_agent(root);
        let specs = mcp_json::parse(&text, root, server_timeout, |name| {
            if name == "CLAUDE_PLUGIN_ROOT" {
                Some(plugin_root.clone())
            } else if local_only {
                None
            } else {
                std::env::var(name).ok()
            }
        })
        .map_err(|error| format!(".mcp.json: {error}"))?;
        // Plugin servers are namespaced so they cannot collide with the user's own.
        servers = specs
            .into_iter()
            .map(|mut spec| {
                spec.name = format!("{}_{}", manifest.name, spec.name);
                spec
            })
            .collect();
    }
    if root.join("agents").is_dir() {
        notes.push(format!(
            "{origin}: agents/ definitions are not supported yet and were skipped"
        ));
    }
    Ok((
        Plugin {
            name: manifest.name,
            description: manifest.description,
            root: root.to_path_buf(),
            hooks,
            servers,
        },
        notes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_code_plugin_contributes_hooks_servers_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("reviewer");
        for sub in [
            ".claude-plugin",
            "hooks",
            "skills/review",
            "commands",
            "agents",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name": "reviewer", "version": "1.0.0", "description": "Reviews code"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("hooks/hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/fmt.sh"}]}]}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers": {"lint": {"command": "${CLAUDE_PLUGIN_ROOT}/lint-server"}}}"#,
        )
        .unwrap();
        let (plugins, problems) = discover(&[dir.path().to_path_buf()], Duration::from_secs(5));
        assert_eq!(plugins.len(), 1);
        let plugin = &plugins[0];
        assert_eq!(plugin.description, "Reviews code");
        assert_eq!(plugin.hooks.len(), 1);
        assert!(plugin.hooks[0].command.ends_with("fmt.sh"));
        assert_eq!(plugin.servers[0].name, "reviewer_lint");
        assert!(matches!(&plugin.servers[0].launch,
            imperium_mcp::Launch::Stdio { command, .. } if command.contains("reviewer")));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("agents/"));
        assert!(plugin.skills_dir().ends_with("skills"));
    }

    /// A plugin whose `.mcp.json` puts `${var}` into a server's URL and the plugin folder into
    /// a header; `provenance` is written as `.imperium-source.json` when given.
    fn plugin_with_vars(dir: &Path, var: &str, provenance: Option<&str>) -> Vec<Plugin> {
        let root = dir.join("vars");
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name": "vars"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            format!(
                r#"{{"mcpServers": {{"api": {{"type": "http", "url": "https://example.test/${{{var}}}/${{UNSET_VAR_FOR_TEST:-fallback}}", "headers": {{"X-Root": "${{CLAUDE_PLUGIN_ROOT}}"}}}}}}}}"#
            ),
        )
        .unwrap();
        if let Some(provenance) = provenance {
            std::fs::write(root.join(SOURCE_FILE), provenance).unwrap();
        }
        discover(&[dir.to_path_buf()], Duration::from_secs(5)).0
    }

    fn url_and_root(plugin: &Plugin) -> (String, String) {
        match &plugin.servers[0].launch {
            imperium_mcp::Launch::Http { url, headers } => (
                url.clone(),
                headers.iter().map(|(_, value)| value.clone()).collect(),
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_user_plugin_reads_the_daemon_environment_in_its_servers() {
        // PATH is set in every environment the tests run in.
        let dir = tempfile::tempdir().unwrap();
        let plugins = plugin_with_vars(dir.path(), "PATH", None);
        let (url, root) = url_and_root(&plugins[0]);
        assert_eq!(
            url,
            format!(
                "https://example.test/{}/fallback",
                std::env::var("PATH").unwrap()
            )
        );
        assert!(root.ends_with("vars"), "{root}");
    }

    #[test]
    fn an_agent_installed_plugin_expands_only_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provenance = r#"{"kind": "git", "location": "https://example.test/x.git", "installed_at_unix_ms": 1, "session_id": "s1"}"#;
        let plugins = plugin_with_vars(dir.path(), "PATH", Some(provenance));
        let (url, root) = url_and_root(&plugins[0]);
        // The daemon's PATH is not read; the default and the plugin folder still expand.
        assert_eq!(url, "https://example.test//fallback");
        assert!(root.ends_with("vars"), "{root}");
    }

    #[test]
    fn a_damaged_provenance_file_still_restricts_expansion() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = plugin_with_vars(dir.path(), "PATH", Some("not json"));
        assert_eq!(
            url_and_root(&plugins[0]).0,
            "https://example.test//fallback"
        );
    }
}
