//! Plugins in the Claude Code layout: a folder with `.claude-plugin/plugin.json` contributing
//! `skills/`, `commands/`, `hooks/hooks.json`, and `.mcp.json`. `${CLAUDE_PLUGIN_ROOT}` in
//! hook commands and server definitions is the plugin folder. `agents/` is reported as not yet
//! supported.

use std::path::{Path, PathBuf};
use std::time::Duration;

use imperium_mcp::{ServerSpec, mcp_json};
use serde::Deserialize;

use crate::hooks::{Hook, parse_hooks_json};

#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
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
        let specs = mcp_json::parse(&text, root, server_timeout, |name| {
            if name == "CLAUDE_PLUGIN_ROOT" {
                Some(plugin_root.clone())
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
            r#"{"name": "reviewer", "version": "1.0.0"}"#,
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
        assert_eq!(plugin.hooks.len(), 1);
        assert!(plugin.hooks[0].command.ends_with("fmt.sh"));
        assert_eq!(plugin.servers[0].name, "reviewer_lint");
        assert!(matches!(&plugin.servers[0].launch,
            imperium_mcp::Launch::Stdio { command, .. } if command.contains("reviewer")));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("agents/"));
        assert!(plugin.skills_dir().ends_with("skills"));
    }
}
