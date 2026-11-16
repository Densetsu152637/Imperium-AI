//! Agent extensions: skills, hooks, slash commands, and plugins in the Claude Code layout.
//!
//! Skills and plugins load lazily: an agent starts with an index of each one's name and
//! description, and loads a skill's instructions (`load_skill`) or starts a plugin's MCP servers
//! (`activate_plugin`) when it first needs them. Hooks are not lazy: a `tool.before` or
//! `prompt.submit` hook may be a security check, so every hook is active from the start (they
//! cost no context and run nothing until their event).

#![warn(unreachable_pub)]

pub mod activation;
pub mod agents;
pub mod commands;
pub mod hooks;
pub mod plugins;
pub mod skills;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use activation::{Activation, LazyPlugin, PluginTools};
pub use agents::{AgentDef, AgentFolder};
pub use commands::SlashCommand;
pub use hooks::{ConfiguredHook, Hook, HookEvent, Hooks, Rewriting, Verdict};
pub use skills::Skill;

/// Where a session's extensions come from.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The user's configuration folder: `skills/`, `commands/`, and `plugins/` inside it.
    pub config_dir: PathBuf,
    pub project_dir: PathBuf,
    /// More plugin folders (each a plugin, or a folder of plugins). These may include folders an
    /// environment's configuration added, so a plugin found only here is not the user's own.
    pub plugin_dirs: Vec<PathBuf>,
    /// The plugin folders from the user's own settings (a subset of `plugin_dirs`, not what an
    /// environment's configuration merged in). Only plugins under these or under
    /// `<config_dir>/plugins` may rewrite tool calls and prompts with their hooks; a plugin from
    /// any other folder may still block.
    pub user_plugin_dirs: Vec<PathBuf>,
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
    /// Named subagents, by name, after precedence (see [`agents`]).
    pub agents: Vec<AgentDef>,
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

    /// The root agent's index of named subagents: one line each, with how to delegate to one.
    /// `None` when there are none.
    pub fn agents_index(&self) -> Option<String> {
        if self.agents.is_empty() {
            return None;
        }
        Some(format!(
            "Named agents are subagents defined by files, each with its own instructions and \
             tools. To delegate a task to one, call spawn_agent with its name as agent; its \
             description says when to use it.\n{}",
            agents::index_lines(&self.agents)
        ))
    }

    /// The named subagent called `name`.
    pub fn agent(&self, name: &str) -> Option<&AgentDef> {
        self.agents.iter().find(|agent| agent.name == name)
    }
}

/// Loads skills, commands, hooks, and plugins. Skills and commands come from the user's
/// folders, then the project's (`.imperium/` and Claude Code's `.claude/`), then plugins; the
/// first of a name wins. Hooks come only from the user's configuration and installed plugins,
/// never from the project, since a repository's hooks would run commands on open. Only skills'
/// front matter is read and no plugin server is started here.
pub fn load(sources: &Sources) -> Extensions {
    load_with(sources, &|_| true)
}

/// [`load`], keeping the hooks only of the plugins `hooks_allowed` says yes to (a persistent
/// agent's permission profile may refuse a plugin; its skills and commands are only text, but a
/// hook runs a command).
pub fn load_with(sources: &Sources, hooks_allowed: &dyn Fn(&str) -> bool) -> Extensions {
    load_trusting(sources, hooks_allowed, false)
}

/// [`load_with`], reading the project's agent files (`.claude/agents`, `.imperium/agents`) only
/// when `project_trusted`: they are repository content that would shape what a subagent is told
/// and may use, so an untrusted project's are reported, not loaded. Skills and commands of a
/// project are loaded either way (they are text an agent chooses to load or the user types).
pub fn load_trusting(
    sources: &Sources,
    hooks_allowed: &dyn Fn(&str) -> bool,
    project_trusted: bool,
) -> Extensions {
    let mut plugin_dirs = vec![sources.config_dir.join("plugins")];
    plugin_dirs.extend(sources.plugin_dirs.iter().cloned());
    let (mut plugins, mut notices) = plugins::discover(&plugin_dirs, sources.server_timeout);
    plugins.retain(|plugin| !sources.disabled_plugins.contains(&plugin.name));
    // A repository can add plugin folders to an environment's configuration, and a plugin there
    // runs commands once trusted; it still must not rewrite what the user or the model asked.
    let user_roots: Vec<PathBuf> = std::iter::once(sources.config_dir.join("plugins"))
        .chain(sources.user_plugin_dirs.iter().cloned())
        .filter_map(|dir| dir.canonicalize().ok())
        .collect();
    for plugin in &mut plugins {
        let mine = plugin
            .root
            .canonicalize()
            .is_ok_and(|root| user_roots.iter().any(|dir| root.starts_with(dir)));
        if !mine {
            for hook in &mut plugin.hooks {
                hook.may_rewrite = false;
            }
        }
    }
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
    // Lowest precedence first: plugins (added below), the user's, the project's.
    let mut agent_folders = Vec::new();
    let mut lazy_plugins = Vec::new();
    for plugin in plugins {
        agent_folders.push(AgentFolder::new(plugin.agents_dir(), plugin.origin()));
        skill_dirs.push((plugin.skills_dir(), plugin.origin()));
        command_dirs.push((
            plugin.commands_dir(),
            plugin.origin(),
            Some(plugin.name.clone()),
        ));
        // Hooks are active from the start: see the module documentation.
        if hooks_allowed(&plugin.name) {
            hooks.extend(plugin.hooks);
        }
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
    agent_folders.push(AgentFolder::new(sources.config_dir.join("agents"), "user"));
    let project_agents = [
        (project.join(".claude/agents"), ".claude/agents"),
        (project.join(".imperium/agents"), ".imperium/agents"),
    ];
    for (dir, shown) in project_agents {
        if project_trusted {
            agent_folders.push(AgentFolder::new(dir, "project"));
        } else {
            let found = agents::count_files(&dir);
            if found > 0 {
                notices.push(format!(
                    "{shown} holds {found} agent file(s) that were not loaded: a repository's \
                     agents shape what a subagent is told and may use, so add this project to \
                     mcp.trusted_projects in your own config to use them"
                ));
            }
        }
    }
    let (agents, mut agent_notices) = agents::discover(&agent_folders);
    notices.append(&mut agent_notices);
    Extensions {
        skills,
        agents,
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
            user_plugin_dirs: Vec::new(),
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

    fn agent_file(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\nSECRET {description} BODY\n")
    }

    #[test]
    fn a_projects_agent_files_load_only_for_a_trusted_project() {
        let (config, project) = fixture();
        write(
            &config.path().join("plugins/lint/agents/linter.md"),
            &agent_file("linter", "from plugin"),
        );
        write(
            &config.path().join("agents/helper.md"),
            &agent_file("helper", "from user"),
        );
        write(
            &project.path().join(".claude/agents/helper.md"),
            &agent_file("helper", "from claude"),
        );
        write(
            &project.path().join(".imperium/agents/helper.md"),
            &agent_file("helper", "from imperium"),
        );
        write(
            &project.path().join(".claude/agents/only-here.md"),
            &agent_file("only-here", "repository"),
        );
        let sources = sources(config.path(), project.path(), true, &[]);
        let description = |extensions: &Extensions, name: &str| {
            extensions
                .agent(name)
                .map(|agent| (agent.description.clone(), agent.origin.clone()))
        };

        // Not trusted (the default): the plugin's and the user's agents, and a notice for the
        // repository's files, which are not read.
        for untrusted in [load(&sources), load_trusting(&sources, &|_| true, false)] {
            assert_eq!(
                description(&untrusted, "helper"),
                Some(("from user".into(), "user".into()))
            );
            assert_eq!(
                description(&untrusted, "linter"),
                Some(("from plugin".into(), "plugin:lint".into()))
            );
            assert!(untrusted.agent("only-here").is_none());
            assert_eq!(untrusted.agents.len(), 2);
            let notices = untrusted.notices.join("\n");
            assert!(
                notices.contains(".claude/agents holds 2 agent file(s)")
                    && notices.contains(".imperium/agents holds 1 agent file(s)")
                    && notices.contains("mcp.trusted_projects"),
                "{notices}"
            );
        }

        // Trusted: the project's replace the user's and the plugin's by name, and
        // `.imperium/agents` replaces `.claude/agents`.
        let trusted = load_trusting(&sources, &|_| true, true);
        assert_eq!(
            description(&trusted, "helper"),
            Some(("from imperium".into(), "project".into()))
        );
        assert_eq!(
            description(&trusted, "only-here"),
            Some(("repository".into(), "project".into()))
        );
        assert_eq!(trusted.agents.len(), 3);
        assert!(trusted.notices.is_empty(), "{:?}", trusted.notices);

        // The root's index names them and says how to delegate, and holds no instructions.
        let index = trusted.agents_index().unwrap();
        assert!(index.contains("spawn_agent") && index.contains("- helper: from imperium"));
        assert!(!index.contains("SECRET"), "{index}");
        assert!(Extensions::default().agents_index().is_none());
    }

    #[test]
    fn a_plugin_the_caller_refuses_brings_no_hooks_but_still_its_text() {
        let (config, project) = fixture();
        let sources = sources(config.path(), project.path(), true, &[]);
        let refused = load_with(&sources, &|plugin| plugin != "lint");
        assert!(refused.hooks.is_empty());
        assert!(refused.index().unwrap().contains("- style: House style"));
        assert!(!load_with(&sources, &|_| true).hooks.is_empty());
    }

    #[test]
    fn only_a_plugin_from_the_users_own_folders_may_rewrite() {
        let (config, project) = fixture();
        let hooks_json =
            r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "x"}]}]}}"#;
        let plugin = |dir: &Path, name: &str| {
            let root = dir.join(name);
            write(
                &root.join(".claude-plugin/plugin.json"),
                &format!(r#"{{"name": "{name}"}}"#),
            );
            write(&root.join("hooks/hooks.json"), hooks_json);
        };
        let env_dir = tempfile::tempdir().unwrap();
        let own_dir = tempfile::tempdir().unwrap();
        plugin(env_dir.path(), "from-env");
        plugin(own_dir.path(), "from-own");
        let mut sources = sources(config.path(), project.path(), true, &[]);
        sources.plugin_dirs = vec![env_dir.path().to_path_buf(), own_dir.path().to_path_buf()];
        sources.user_plugin_dirs = vec![own_dir.path().to_path_buf()];
        let loaded = load(&sources);
        let rewrites = |origin: &str| {
            loaded
                .hooks
                .list()
                .iter()
                .find(|hook| hook.origin == origin)
                .map(|hook| hook.may_rewrite)
        };
        // Not an agent's install, and not from an environment's folder: only the latter is
        // refused, the user's folders and `<config>/plugins` may rewrite.
        assert_eq!(rewrites("plugin:from-env"), Some(false));
        assert_eq!(rewrites("plugin:from-own"), Some(true));
        assert_eq!(rewrites("plugin:lint"), Some(true));
        // Spelling an environment's folder so that it passes through the user's plugin folder
        // (`plugins/../envp`) does not make it the user's: the paths are compared resolved.
        let through = config.path().join("envp");
        plugin(&through, "from-dots");
        std::fs::create_dir_all(config.path().join("plugins")).unwrap();
        let mut tricky = sources.clone();
        tricky.plugin_dirs = vec![config.path().join("plugins/../envp")];
        tricky.user_plugin_dirs = Vec::new();
        let loaded = load(&tricky);
        let dots = loaded
            .hooks
            .list()
            .iter()
            .find(|hook| hook.origin == "plugin:from-dots")
            .expect("the plugin is found");
        assert!(!dots.may_rewrite);
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
