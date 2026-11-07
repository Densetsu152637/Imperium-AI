//! Agent extensions: skills, hooks, slash commands, and plugins in the Claude Code layout.

pub mod commands;
pub mod hooks;
mod markdown;
pub mod plugins;
pub mod skills;

use std::path::PathBuf;
use std::time::Duration;

use imperium_mcp::ServerSpec;

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
    /// `[[hooks]]` from the user's configuration.
    pub hooks: Vec<ConfiguredHook>,
    pub server_timeout: Duration,
}

/// Everything extensions contribute to a session.
#[derive(Debug, Clone, Default)]
pub struct Extensions {
    pub skills: Vec<Skill>,
    pub hooks: Hooks,
    pub commands: Vec<SlashCommand>,
    /// MCP servers plugins declare.
    pub servers: Vec<ServerSpec>,
    /// Problems worth telling the user about.
    pub notices: Vec<String>,
}

/// Loads skills, commands, hooks, and plugins. Skills and commands come from the user's
/// folders, then the project's (`.imperium/` and Claude Code's `.claude/`), then plugins; the
/// first of a name wins. Hooks come only from the user's configuration and installed plugins,
/// never from the project, since a repository's hooks would run commands on open.
pub fn load(sources: &Sources) -> Extensions {
    let mut plugin_dirs = vec![sources.config_dir.join("plugins")];
    plugin_dirs.extend(sources.plugin_dirs.iter().cloned());
    let (plugins, mut notices) = plugins::discover(&plugin_dirs, sources.server_timeout);

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
    let mut servers = Vec::new();
    for plugin in plugins {
        skill_dirs.push((plugin.skills_dir(), plugin.origin()));
        command_dirs.push((
            plugin.commands_dir(),
            plugin.origin(),
            Some(plugin.name.clone()),
        ));
        hooks.extend(plugin.hooks);
        servers.extend(plugin.servers);
    }
    let (skills, mut skill_problems) = skills::discover(&skill_dirs);
    notices.append(&mut skill_problems);
    Extensions {
        skills,
        hooks: Hooks::new(hooks),
        commands: commands::discover(&command_dirs),
        servers,
        notices,
    }
}
