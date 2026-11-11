//! Agent extensions: skills, hooks, slash commands, and plugins in the Claude Code layout.
//!
//! Skills and plugins load lazily: an agent starts with an index of each one's name and
//! description, and loads a skill's instructions (`load_skill`) or starts a plugin's MCP servers
//! (`activate_plugin`) when it first needs them. Hooks are not lazy: a `tool.before` or
//! `prompt.submit` hook may be a security check, so every hook is active from the start (they
//! cost no context and run nothing until their event).

pub mod activation;
pub mod commands;
pub mod hooks;
pub use imperium_api::markdown;
pub mod plugins;
pub mod skills;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use activation::{Activation, LazyPlugin, PluginTools};
pub use commands::SlashCommand;
pub use hooks::{ConfiguredHook, Hook, HookEvent, Hooks, Verdict};
pub use skills::Skill;

/// Where a session's extensions come from.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The user's configuration folder: `skills/`, `commands/`, and `plugins/` inside it.
    pub config_dir: PathBuf,
    pub project_dir: PathBuf,
    /// More plugin folders (each a plugin, or a folder of plugins).
    pub plugin_dirs: Vec<PathBuf>,
    /// Plugins, by name, that are found but not loaded.
    pub disabled_plugins: Vec<String>,
    /// `[[hooks]]` from the user's configuration.
    pub hooks: Vec<ConfiguredHook>,
    pub server_timeout: Duration,
    /// Load skills and plugins on first use; off, everything loads up front.
    pub lazy: bool,
    /// Skills and plugins, by name, that load up front anyway.
    pub eager: Vec<String>,
}

/// Everything extensions contribute to a session.
#[derive(Debug, Clone, Default)]
pub struct Extensions {
    pub skills: Vec<Skill>,
    pub hooks: Hooks,
    pub commands: Vec<SlashCommand>,
    /// Plugins with MCP servers, which start when activated.
    pub plugins: Arc<PluginTools>,
    /// Problems worth telling the user about.
    pub notices: Vec<String>,
}

impl Extensions {
    /// The index for an agent's instructions: one line per skill and plugin loaded on use,
    /// with how to load it. `None` when there is nothing to load.
    pub fn index(&self) -> Option<String> {
        let skills = skills::index_lines(&self.skills);
        let plugins: Vec<&LazyPlugin> = self
            .plugins
            .plugins()
            .iter()
            .filter(|plugin| !plugin.eager)
            .collect();
        if skills.is_empty() && plugins.is_empty() {
            return None;
        }
        let mut text = String::new();
        if !skills.is_empty() {
            text.push_str(
                "Skills are packaged instructions for particular tasks; only their names and \
                 descriptions are listed here. When a task matches a skill, call load_skill \
                 with its name before starting, and follow it.\n",
            );
            text.push_str(&skills);
        }
        if !plugins.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(
                "Plugins with tools that are not loaded yet. When a task needs one, call \
                 activate_plugin with its name; its tools are offered from then on.\n",
            );
            for plugin in plugins {
                let servers: Vec<&str> = plugin
                    .servers
                    .iter()
                    .map(|server| server.name.as_str())
                    .collect();
                let description = if plugin.description.is_empty() {
                    "no description"
                } else {
                    &plugin.description
                };
                text.push_str(&format!(
                    "- {}: {description} (servers: {})\n",
                    plugin.name,
                    servers.join(", ")
                ));
            }
        }
        Some(text)
    }
}

/// Loads skills, commands, hooks, and plugins. Skills and commands come from the user's
/// folders, then the project's (`.imperium/` and Claude Code's `.claude/`), then plugins; the
/// first of a name wins. Hooks come only from the user's configuration and installed plugins,
/// never from the project, since a repository's hooks would run commands on open. Only skills'
/// front matter is read and no plugin server is started here.
pub fn load(sources: &Sources) -> Extensions {
    let mut plugin_dirs = vec![sources.config_dir.join("plugins")];
    plugin_dirs.extend(sources.plugin_dirs.iter().cloned());
    let (mut plugins, mut notices) = plugins::discover(&plugin_dirs, sources.server_timeout);
    plugins.retain(|plugin| !sources.disabled_plugins.contains(&plugin.name));
    let eager = |name: &str| !sources.lazy || sources.eager.iter().any(|eager| eager == name);

    let project = &sources.project_dir;
    let mut skill_dirs = vec![
        (sources.config_dir.join("skills"), "user".to_string()),
        (project.join(".imperium/skills"), "project".to_string()),
        (project.join(".claude/skills"), "project".to_string()),
    ];
    let mut command_dirs = vec![
        (
            sources.config_dir.join("commands"),
            "user".to_string(),
            None,
        ),
        (
            project.join(".imperium/commands"),
            "project".to_string(),
            None,
        ),
        (
            project.join(".claude/commands"),
            "project".to_string(),
            None,
        ),
    ];
    let mut hooks = Vec::new();
    for hook in sources.hooks.iter().cloned() {
        match hook.into_hook("user config") {
            Ok(hook) => hooks.push(hook),
            Err(error) => notices.push(format!("[[hooks]]: {error}")),
        }
    }
    let mut lazy_plugins = Vec::new();
    for plugin in plugins {
        skill_dirs.push((plugin.skills_dir(), plugin.origin()));
        command_dirs.push((
            plugin.commands_dir(),
            plugin.origin(),
            Some(plugin.name.clone()),
        ));
        // Hooks are active from the start: see the module documentation.
        hooks.extend(plugin.hooks);
        if !plugin.servers.is_empty() {
            lazy_plugins.push(LazyPlugin {
                eager: eager(&plugin.name),
                name: plugin.name,
                description: plugin.description,
                servers: plugin.servers,
            });
        }
    }
    let (mut skills, mut skill_problems) = skills::discover(&skill_dirs);
    for skill in &mut skills {
        skill.eager = eager(&skill.name);
    }
    notices.append(&mut skill_problems);
    Extensions {
        skills,
        hooks: Hooks::new(hooks),
        commands: commands::discover(&command_dirs),
        plugins: Arc::new(PluginTools::new(lazy_plugins)),
        notices,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn sources(config: &Path, project: &Path, lazy: bool, eager: &[&str]) -> Sources {
        Sources {
            config_dir: config.to_path_buf(),
            project_dir: project.to_path_buf(),
            plugin_dirs: Vec::new(),
            disabled_plugins: Vec::new(),
            hooks: Vec::new(),
            server_timeout: Duration::from_secs(5),
            lazy,
            eager: eager.iter().map(|name| name.to_string()).collect(),
        }
    }

    /// A user skill, and a plugin with a skill, a hook, and a server.
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(
            &config.path().join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: Fill PDF forms\nwhen_to_use: a form arrives as a PDF\n---\nSECRET PDF STEPS\n",
        );
        let plugin = config.path().join("plugins/lint");
        write(
            &plugin.join(".claude-plugin/plugin.json"),
            r#"{"name": "lint", "description": "Linters"}"#,
        );
        write(
            &plugin.join("skills/style/SKILL.md"),
            "---\nname: style\ndescription: House style\n---\nSECRET STYLE RULES\n",
        );
        write(
            &plugin.join("hooks/hooks.json"),
            r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "check"}]}]}}"#,
        );
        write(
            &plugin.join(".mcp.json"),
            r#"{"mcpServers": {"server": {"command": "lint-server"}}}"#,
        );
        (config, project)
    }

    #[test]
    fn only_an_index_is_loaded_up_front() {
        let (config, project) = fixture();
        let extensions = load(&sources(config.path(), project.path(), true, &[]));
        let index = extensions.index().unwrap();
        assert!(
            index.contains("- pdf: Fill PDF forms (use when: a form arrives as a PDF)"),
            "{index}"
        );
        assert!(index.contains("- style: House style"), "{index}");
        assert!(
            index.contains("- lint: Linters (servers: lint_server)"),
            "{index}"
        );
        assert!(!index.contains("SECRET"), "{index}");
        assert!(skills::eager_instructions(&extensions.skills).is_none());
        // Hooks stay active from the start, whatever loads lazily.
        assert!(!extensions.hooks.is_empty());
        assert!(!extensions.plugins.plugins()[0].eager);
    }

    #[test]
    fn eager_names_and_the_global_switch_load_up_front() {
        let (config, project) = fixture();
        let extensions = load(&sources(
            config.path(),
            project.path(),
            true,
            &["pdf", "lint"],
        ));
        let index = extensions.index().unwrap();
        assert!(!index.contains("- pdf:"), "{index}");
        assert!(!index.contains("- lint:"), "{index}");
        assert!(index.contains("- style:"), "{index}");
        let eager = skills::eager_instructions(&extensions.skills).unwrap();
        assert!(eager.contains("SECRET PDF STEPS"), "{eager}");
        assert!(!eager.contains("SECRET STYLE RULES"), "{eager}");
        assert!(extensions.plugins.plugins()[0].eager);

        let everything = load(&sources(config.path(), project.path(), false, &[]));
        assert!(everything.index().is_none());
        let eager = skills::eager_instructions(&everything.skills).unwrap();
        assert!(eager.contains("SECRET STYLE RULES"), "{eager}");
    }
}
