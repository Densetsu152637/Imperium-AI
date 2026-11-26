use super::*;

/// Whether a plugin is on, and what decided it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Switch {
    pub(crate) on: bool,
    /// `default`, `user`, `environment` or `repository`.
    pub(crate) decided_by: &'static str,
}

/// The on/off state of the plugin `name` for a session, before the load-time gate (which can
/// still withhold a plugin that is on). In order: a trusted repository's file that disables it
/// (`repository`, which an environment's `enabled` cannot override), the environment's
/// `disabled`, the user's `extensions.disabled_plugins` unless the environment's `enabled` lists
/// it (`user`, `environment`), else on.
pub(crate) fn plugin_switch(
    name: &str,
    user: &[String],
    repository: &[String],
    environment: &imperium_protocol::methods::EnvironmentPlugins,
) -> Switch {
    let has = |list: &[String]| list.iter().any(|entry| entry == name);
    let (on, decided_by) = if has(repository) {
        (false, "repository")
    } else if has(&environment.disabled) {
        (false, "environment")
    } else if has(user) {
        if has(&environment.enabled) {
            (true, "environment")
        } else {
            (false, "user")
        }
    } else {
        (true, "default")
    };
    Switch { on, decided_by }
}

impl DaemonState {
    /// What the user's own list, a trusted repository's file and the environment's lists say
    /// about plugins: `(user, repository, environment)`. `merged` is the session's
    /// `extensions.disabled_plugins` (the user's list plus the repository's additions).
    pub(crate) fn plugin_lists(
        &self,
        merged: &[String],
        repository: &[String],
        environment_id: Option<&str>,
    ) -> (
        Vec<String>,
        Vec<String>,
        imperium_protocol::methods::EnvironmentPlugins,
    ) {
        let user = self.settings().extensions.disabled_plugins.clone();
        // Anything in the merged list that is not the user's came from a repository's file.
        let mut repository = repository.to_vec();
        repository.extend(merged.iter().filter(|name| !user.contains(name)).cloned());
        let environment = environment_id
            .and_then(|id| self.environments.get(id))
            .map(|env| env.plugins)
            .unwrap_or_default();
        (user, repository, environment)
    }

    /// Where UI mods are installed.
    pub fn mods_dir(&self) -> std::path::PathBuf {
        self.paths.config_dir().join("mods")
    }

    /// The first-start plugin migration (`plugin_records::migrate`). Run when the daemon starts,
    /// before any session loads plugins; it does nothing once its marker exists.
    pub fn migrate_plugins(&self) {
        let disabled = self.settings().extensions.disabled_plugins.clone();
        match crate::plugin_records::migrate(&self.paths, &disabled) {
            Ok(crate::plugin_records::Migration::Recorded { count, skipped }) => {
                tracing::info!(plugins = count, "recorded the installed plugins");
                for report in skipped {
                    tracing::warn!("plugin not carried over, so it stays withheld: {report}");
                }
            }
            Ok(crate::plugin_records::Migration::Done) => {}
            Ok(crate::plugin_records::Migration::Damaged) => tracing::warn!(
                "the plugin record file is unreadable; plugins with code stay withheld"
            ),
            Err(error) => tracing::warn!(%error, "could not record the installed plugins"),
        }
    }

    /// Workflow packs and the project's instructions and documentation, unless the workflow is
    /// turned off. Packs that cannot be read are reported in `notices`.
    pub(super) fn context(
        &self,
        project_dir: &Path,
        notices: &mut Vec<String>,
    ) -> Option<Arc<ContextIndex>> {
        let settings = self.settings();
        if !settings.workflow.enabled {
            return None;
        }
        let resolved = packs::resolve(&self.pack_inputs(&settings.workflow), project_dir);
        notices.extend(resolved.notices);
        resolved.index
    }

    /// Where personalities are looked up: `<config>/personalities/`, the workflow folders,
    /// then the bundled pack (with the default compiled in as the last resort).
    pub(crate) fn personality_sources(&self) -> personality::Sources {
        packs::personality_sources(&self.pack_inputs(&self.settings().workflow))
    }

    pub(super) fn pack_inputs<'a>(
        &'a self,
        workflow: &'a imperium_config::WorkflowSettings,
    ) -> packs::PackInputs<'a> {
        packs::PackInputs {
            enabled: workflow.enabled,
            listed: &workflow.packs,
            default_pack: workflow.default_pack,
            data_dir: self.paths.data_dir(),
            config_dir: self.paths.config_dir(),
            bundled: bundled_pack(),
        }
    }

    /// The root agent's personality block for a new session, from `policy.personality` (`id`).
    /// An id that is not found falls back to the default, with a notice in `notices`.
    pub(super) fn personality(&self, id: &str, notices: &mut Vec<String>) -> Option<String> {
        let selection = self.personality_sources().select(id);
        if let Some(warning) = selection.warning {
            tracing::warn!("{warning}");
            notices.push(warning);
        }
        selection
            .personality
            .map(|personality| personality.instructions())
    }

    /// Skills, hooks, slash commands, and plugins for a session in `project_dir`, with the
    /// plugin folders and disabled plugins of `settings` (the environment's narrowed ones).
    ///
    /// A session bound to an agent whose profile is `profile`: plugins the profile refuses bring
    /// no hooks, and under local files only the environment's own file cannot add plugin folders
    /// or disable plugins (the daemon's settings decide those), since the agent may have written it.
    pub(super) fn extensions(
        &self,
        project_dir: &Path,
        settings: &Settings,
        profile: Option<&imperium_policy::AgentProfile>,
        environment_id: Option<&str>,
        repository_disabled: &[String],
    ) -> Extensions {
        let local_only = profile.is_some_and(|profile| profile.local_files_only());
        let settings = if local_only {
            // Not the environment's narrowed settings.
            std::borrow::Cow::Owned((*self.settings()).clone())
        } else {
            std::borrow::Cow::Borrowed(settings)
        };
        // The effective on/off switch: the user's list, the environment's lists (the user's own
        // file, so they apply to a locked-down profile too) and a trusted repository's file.
        let repository_disabled: &[String] = if local_only { &[] } else { repository_disabled };
        let (user, repository, environment) = self.plugin_lists(
            &settings.extensions.disabled_plugins,
            repository_disabled,
            environment_id,
        );
        let mut candidates = settings.extensions.disabled_plugins.clone();
        candidates.extend(environment.disabled.iter().cloned());
        candidates.extend(repository.iter().cloned());
        candidates.sort();
        candidates.dedup();
        let disabled_plugins: Vec<String> = candidates
            .into_iter()
            .filter(|name| !plugin_switch(name, &user, &repository, &environment).on)
            .collect();
        // A project's agent files are read only when the user trusts the project, as for its
        // `.mcp.json` servers.
        let project_trusted = self.settings().mcp.trusts(project_dir);
        // Folders an agent installed or a marketplace installed hold downloaded content: no
        // daemon `${VAR}`, no rewriting hooks. The record says which; a folder's provenance file
        // can only add to the set.
        let mut plugin_dirs = vec![self.paths.config_dir().join("plugins")];
        plugin_dirs.extend(settings.extensions.plugin_dirs.iter().cloned());
        // Plugins in `extensions.plugin_dirs` skip the load-time gate, and a trusted project's own
        // `.imperium/config.toml` can add folders to an environment's list, which only this merge
        // sees: no file tool writes into any of them (a hook there would run next session unasked).
        for dir in &settings.extensions.plugin_dirs {
            imperium_policy::permissions::protect_dir(dir);
            imperium_policy::permissions::refuse_writes_under(dir);
        }
        let mut downloaded = imperium_extensions::plugins::agent_installed_folders(&plugin_dirs);
        // The load-time gate: a plugin it withholds loads nothing, whatever `disabled_plugins` says.
        let gate =
            crate::plugin_records::gate(&self.paths, crate::plugin_records::production_verifier());
        downloaded.extend(gate.downloaded);
        // The same for the skills an agent installed into the user's own skills folder: those the
        // user has not approved are not loaded, and approved ones rank after project and plugin
        // skills, so they can never shadow one.
        let skill_gate =
            crate::skill_records::gate(&self.paths, crate::plugin_records::production_verifier());
        let plugin_secrets = crate::plugin_install::plugin_secrets(Arc::clone(&self.secrets));
        let mut extensions = imperium_extensions::load_with_secrets(
            &Sources {
                config_dir: self.paths.config_dir().to_path_buf(),
                project_dir: project_dir.to_path_buf(),
                plugin_dirs: settings.extensions.plugin_dirs.clone(),
                // The user's own folders, never what an environment's configuration merged in.
                user_plugin_dirs: self.settings().extensions.plugin_dirs.clone(),
                disabled_plugins,
                hooks: self
                    .settings()
                    .hooks
                    .iter()
                    .map(|hook| ConfiguredHook {
                        event: hook.event.clone(),
                        matcher: hook.matcher.clone(),
                        command: hook.command.clone(),
                        url: hook.url.clone(),
                        headers: hook.headers.clone(),
                        timeout_s: hook.timeout_s,
                    })
                    .collect(),
                server_timeout: Duration::from_secs(60),
                lazy: self.settings().extensions.lazy,
                eager: self.settings().extensions.eager.clone(),
                downloaded,
                allowed: Some(gate.allowed),
                skill_gate: imperium_extensions::SkillGate {
                    withheld: skill_gate
                        .withheld
                        .iter()
                        .map(|withheld| withheld.folder.clone())
                        .collect(),
                    agent: skill_gate.agent.clone(),
                },
            },
            &|plugin| profile.is_none_or(|profile| profile.allows_plugin(plugin)),
            project_trusted,
            Some(&plugin_secrets),
        );
        for withheld in &gate.withheld {
            let name = withheld.folder.file_name().map_or_else(
                || withheld.folder.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            extensions
                .notices
                .push(format!("plugin {name} is withheld: {}", withheld.reason));
        }
        for withheld in &skill_gate.withheld {
            let name = withheld.folder.file_name().map_or_else(
                || withheld.folder.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            extensions
                .notices
                .push(format!("skill {name} is withheld: {}", withheld.reason));
        }
        // HTTP hooks' `${secret:name}` headers read the keychain, nothing else.
        let secrets = Arc::clone(&self.secrets);
        extensions.hooks = std::mem::take(&mut extensions.hooks).with_secrets(
            imperium_extensions::hooks::Secrets::new(move |name| {
                let name: imperium_config::SecretName = name.parse().ok()?;
                secrets.load(&name).ok().flatten()
            }),
        );
        extensions
    }
}

#[cfg(test)]
mod tests {
    use imperium_config::{Config, Layer, Paths};
    use imperium_protocol::methods::{
        EnvironmentCreateParams, EnvironmentPlugins, PluginsSetEnabledParams,
    };

    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn the_load_time_gate_decides_what_a_session_loads_from_a_plugin() {
        let home = tempfile::tempdir().unwrap();
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let root = state.paths().config_dir().join("plugins/lint");
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "lint"}"#,
        );
        write(
            &root.join("skills/style/SKILL.md"),
            "---\nname: style\ndescription: House style\n---\nbody\n",
        );
        let hook = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "check"}]}]}}"#;
        write(&root.join("hooks/hooks.json"), hook);
        let project = tempfile::tempdir().unwrap();
        let load = || {
            let settings = state.settings();
            state.extensions(project.path(), &settings, None, None, &[])
        };

        // Not recorded: the whole plugin is withheld, skills included, and the user is told.
        let before = load();
        assert!(before.hooks.is_empty());
        assert!(before.skills.is_empty());
        assert!(
            before
                .notices
                .iter()
                .any(|notice| notice.contains("not installed by Imperium: approve to load"))
        );

        // Migration records it with a carried-over approval.
        state.migrate_plugins();
        let after = load();
        assert!(!after.hooks.is_empty());
        assert!(after.skills.iter().any(|skill| skill.name == "style"));

        // A second plugin with a hook, not in `disabled_plugins` and never recorded, stays out
        // while the first loads.
        assert!(state.settings().extensions.disabled_plugins.is_empty());
        let later = state.paths().config_dir().join("plugins/later");
        write(
            &later.join(".claude-plugin/plugin.json"),
            r#"{"name": "later"}"#,
        );
        write(&later.join("hooks/hooks.json"), hook);
        let with_later = load();
        assert!(!with_later.hooks.is_empty());
        assert!(
            with_later
                .notices
                .iter()
                .any(|notice| notice.contains("plugin later is withheld"))
        );

        // A change to any file withholds the whole plugin again.
        write(
            &root.join("skills/style/SKILL.md"),
            "---\nname: style\ndescription: House style\n---\nedited\n",
        );
        let changed = load();
        assert!(changed.hooks.is_empty());
        assert!(changed.skills.is_empty());
    }

    #[test]
    fn the_switch_follows_the_repository_then_the_environment_then_the_user() {
        let list = |names: &[&str]| {
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        };
        let environment = EnvironmentPlugins {
            enabled: list(&["a", "b"]),
            disabled: list(&["c"]),
            ..Default::default()
        };
        let user = list(&["a", "c", "d"]);
        let repository = list(&["b"]);
        let decide = |name: &str| plugin_switch(name, &user, &repository, &environment);
        // The repository wins over the environment's `enabled`.
        assert_eq!(
            (decide("b").on, decide("b").decided_by),
            (false, "repository")
        );
        // The environment's `enabled` overrides the user's list.
        assert_eq!(
            (decide("a").on, decide("a").decided_by),
            (true, "environment")
        );
        // The environment's `disabled` turns a plugin off for it.
        assert_eq!(
            (decide("c").on, decide("c").decided_by),
            (false, "environment")
        );
        assert_eq!((decide("d").on, decide("d").decided_by), (false, "user"));
        assert_eq!((decide("e").on, decide("e").decided_by), (true, "default"));
    }

    #[test]
    fn plugins_turn_on_and_off_for_the_user_and_for_one_environment() {
        let home = tempfile::tempdir().unwrap();
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let plugins_dir = state.paths().config_dir().join("plugins");
        let hook = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "check"}]}]}}"#;
        for name in ["lint", "guard"] {
            write(
                &plugins_dir.join(name).join(".claude-plugin/plugin.json"),
                &format!(r#"{{"name": "{name}"}}"#),
            );
        }
        write(
            &plugins_dir.join("lint/skills/style/SKILL.md"),
            "---\nname: style\ndescription: House style\n---\nbody\n",
        );
        write(&plugins_dir.join("guard/hooks/hooks.json"), hook);
        // Both are recorded, with a carried-over approval.
        state.migrate_plugins();
        let project = tempfile::tempdir().unwrap();
        let folder_a = tempfile::tempdir().unwrap();
        let folder_b = tempfile::tempdir().unwrap();
        let make = |name: &str, folder: &Path| {
            state
                .environments()
                .create(EnvironmentCreateParams {
                    name: name.into(),
                    folders: vec![folder.display().to_string()],
                    ..EnvironmentCreateParams::default()
                })
                .unwrap()
                .id
        };
        let (a, b) = (make("A", folder_a.path()), make("B", folder_b.path()));
        let load = |environment: Option<&str>, repository: &[String]| {
            let settings = state.settings();
            state.extensions(project.path(), &settings, None, environment, repository)
        };
        let set = |name: &str, enabled: bool, environment: Option<&str>| {
            crate::plugin_install::set_enabled(
                &state,
                PluginsSetEnabledParams {
                    name: name.into(),
                    enabled,
                    environment: environment.map(str::to_string),
                },
            )
        };
        let has_skill =
            |extensions: &Extensions| extensions.skills.iter().any(|skill| skill.name == "style");
        assert!(has_skill(&load(None, &[])));

        // Off for the user: not loaded in a new session, in any environment.
        let done = set("LINT", false, None).unwrap();
        assert!(!done.enabled && done.decided_by == "user");
        assert!(!has_skill(&load(None, &[])));
        assert!(!has_skill(&load(Some(&a), &[])));

        // On for environment A only.
        let done = set("lint", true, Some("A")).unwrap();
        assert!(done.enabled && done.decided_by == "environment");
        assert!(has_skill(&load(Some(&a), &[])));
        assert!(!has_skill(&load(Some(&b), &[])));
        assert!(!has_skill(&load(None, &[])));
        let listed = |environment: Option<&str>| {
            let listed = crate::plugin_install::list(&state, environment).unwrap();
            let lint = listed
                .plugins
                .into_iter()
                .find(|p| p.name == "lint")
                .unwrap();
            (lint.enabled, lint.decided_by)
        };
        assert_eq!(listed(Some(&a)), (true, "environment".to_string()));
        assert_eq!(listed(None), (false, "user".to_string()));

        // A trusted repository's file that disables it wins over the environment.
        assert!(!has_skill(&load(Some(&a), &["lint".to_string()])));

        // Turned off for A it is off there; turned on for everyone it is on in B too.
        set("lint", false, Some("A")).unwrap();
        assert!(!has_skill(&load(Some(&a), &[])));
        set("lint", true, None).unwrap();
        assert!(has_skill(&load(Some(&b), &[])));
        assert!(!has_skill(&load(Some(&a), &[])));
        let done = set("lint", true, Some("A")).unwrap();
        assert!(done.enabled);

        // Off then on again needs no new approval: the hooks load again.
        assert!(!load(None, &[]).hooks.is_empty());
        set("guard", false, None).unwrap();
        assert!(load(None, &[]).hooks.is_empty());
        set("guard", true, None).unwrap();
        assert!(!load(None, &[]).hooks.is_empty());

        // A plugin the gate withholds cannot be turned on.
        let stray = plugins_dir.join("stray");
        write(
            &stray.join(".claude-plugin/plugin.json"),
            r#"{"name": "stray"}"#,
        );
        write(&stray.join("hooks/hooks.json"), hook);
        let error = set("stray", true, None).unwrap_err();
        assert!(error.contains("withheld"), "{error}");
        assert!(set("stray", false, None).is_ok());
    }

    #[test]
    fn what_an_agent_installed_adds_nothing_until_the_user_approves_it() {
        use crate::plugin_records::{self, ApprovalKind, Origin, Record, TestVerifier};
        let home = tempfile::tempdir().unwrap();
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        // Writes the marker, so records can be written.
        state.migrate_plugins();
        let config = state.paths().config_dir().to_path_buf();
        // A plugin an agent installed: agents, a skill and a command, and no hooks or servers.
        let root = config.join("plugins/helper");
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "helper"}"#,
        );
        write(
            &root.join("agents/x.md"),
            "---
name: x
description: Does x.
---
You do x.
",
        );
        write(
            &root.join("skills/style/SKILL.md"),
            "---
name: style
description: House style
---
body
",
        );
        write(
            &root.join("commands/go.md"),
            "---
description: Go
---
Go.
",
        );
        let seen = plugin_records::measure(&root).unwrap();
        let plugin = plugin_records::parse(&root).unwrap();
        plugin_records::add_record(
            state.paths(),
            &root,
            Record {
                name: plugin.name.clone(),
                origin: Origin::Agent,
                id: None,
                source: None,
                commit: None,
                hash: seen.hash,
                runs: imperium_extensions::plugins::runs(&plugin).digest(),
                approval: None,
            },
        )
        .unwrap();
        // Two skills an agent installed: one named like the plugin's skill, one of its own.
        let same = config.join("skills/agent-style");
        write(
            &same.join("SKILL.md"),
            "---
name: style
description: Shadow
---
body
",
        );
        let solo = config.join("skills/solo");
        write(
            &solo.join("SKILL.md"),
            "---
name: solo
description: On its own
---
body
",
        );
        for dir in [&same, &solo] {
            crate::skill_records::record_install(state.paths(), dir, "x", None, None).unwrap();
        }
        // The user's own skill is never gated.
        write(
            &config.join("skills/mine/SKILL.md"),
            "---
name: mine
description: Hand-made
---
body
",
        );
        let project = tempfile::tempdir().unwrap();
        let load = || {
            let settings = state.settings();
            state.extensions(project.path(), &settings, None, None, &[])
        };

        // Nothing it brings is loaded or listed before the approval.
        let before = load();
        assert!(before.agent("x").is_none());
        assert!(before.agents_index().is_none());
        assert!(
            before
                .commands
                .iter()
                .all(|command| !command.name.ends_with("go"))
        );
        let names: Vec<&str> = before.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["mine"], "{names:?}");
        assert!(
            before
                .notices
                .iter()
                .any(|notice| notice.contains("plugin helper is withheld")
                    && notice.contains("desktop app")),
            "{:?}",
            before.notices
        );
        assert!(
            before
                .notices
                .iter()
                .any(|notice| notice.contains("skill solo is withheld")),
            "{:?}",
            before.notices
        );

        // Approved (the app's identity checks decide that elsewhere), they load.
        let (_, hash, runs, _) = plugin_records::content(&root).unwrap();
        plugin_records::record_approval(
            state.paths(),
            &TestVerifier,
            &root,
            ApprovalKind::Counted,
            &hash,
            &runs,
        )
        .unwrap();
        for dir in [&same, &solo] {
            let (_, hash, runs, _) = crate::skill_records::content(dir).unwrap();
            crate::skill_records::record_approval(
                state.paths(),
                &TestVerifier,
                dir,
                ApprovalKind::Counted,
                &hash,
                &runs,
            )
            .unwrap();
        }
        let after = load();
        assert!(after.agent("x").is_some());
        assert!(after.agents_index().is_some());
        assert!(
            after
                .commands
                .iter()
                .any(|command| command.name.ends_with("go"))
        );
        assert!(after.skills.iter().any(|skill| skill.name == "solo"));
        // The agent's skill never shadows the plugin's skill of the same name.
        let style: Vec<_> = after.skills.iter().filter(|s| s.name == "style").collect();
        assert_eq!(style.len(), 1);
        assert!(style[0].dir.starts_with(&root), "{:?}", style[0].dir);
    }
}

#[cfg(test)]
mod environment_plugin_folder_tests {
    use imperium_config::{Config, Layer, Paths};

    use super::*;

    #[test]
    fn a_plugin_folder_an_environment_adds_is_refused_to_file_tools() {
        let home = tempfile::tempdir().unwrap();
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let tools = tempfile::tempdir().unwrap();
        let folder = tools.path().join("plugins");
        let hook = folder.join("p/hooks/hooks.json");
        let shown = hook.to_string_lossy().replace('\\', "/");
        assert!(!imperium_policy::permissions::refused_write(&shown));
        // The settings of a session in an environment whose trusted project added the folder.
        let mut merged = (*state.settings()).clone();
        merged.extensions.plugin_dirs = vec![folder.clone()];
        let project = tempfile::tempdir().unwrap();
        let _ = state.extensions(project.path(), &merged, None, None, &[]);
        assert!(imperium_policy::permissions::refused_write(&shown));
        // Only that folder: a neighbour is untouched.
        let beside = tools
            .path()
            .join("src/main.rs")
            .to_string_lossy()
            .replace('\\', "/");
        assert!(!imperium_policy::permissions::refused_write(&beside));
    }

    fn project_adding_plugin_folder() -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join(".imperium/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[extensions]\nplugin_dirs = [\"tools/plugins\"]\n").unwrap();
        project
    }

    /// Creates the environment; returns a hook file under the plugin folder as the daemon spells
    /// it (it may store the folder in another form than the temp path).
    fn create_environment(state: &DaemonState, project: &Path) -> String {
        let info = state
            .environments()
            .create(imperium_protocol::methods::EnvironmentCreateParams {
                name: "Work".into(),
                folders: vec![project.display().to_string()],
                ..imperium_protocol::methods::EnvironmentCreateParams::default()
            })
            .unwrap();
        let folder = std::path::PathBuf::from(&info.folders[0]);
        let hook = folder.join("tools/plugins/p/hooks/hooks.json");
        hook.to_string_lossy().replace('\\', "/")
    }

    fn trusting(project: &Path) -> imperium_config::Settings {
        let user = format!(
            "[mcp]\ntrusted_projects = [\"{}\"]",
            project.display().to_string().replace('\\', "/")
        );
        Config::from_layers([
            Layer::defaults(),
            Layer::new(
                imperium_config::Source::User("test".into()),
                user.parse().unwrap(),
            ),
        ])
        .settings()
        .unwrap()
    }

    #[test]
    fn the_daemon_start_guards_plugin_folders_of_existing_environments() {
        let home = tempfile::tempdir().unwrap();
        let project = project_adding_plugin_folder();
        let settings = trusting(project.path());
        // A previous run created the environment.
        let first = DaemonState::new(Paths::under(home.path()), settings.clone(), None);
        let hook = create_environment(&first, project.path());
        drop(first);
        assert!(!imperium_policy::permissions::refused_write(&hook));
        // Starting the daemon guards the folder before any session loads.
        let _state = DaemonState::new(Paths::under(home.path()), settings, None);
        assert!(imperium_policy::permissions::refused_write(&hook));
    }

    #[test]
    fn newly_trusting_a_project_guards_its_plugin_folders_at_once() {
        let home = tempfile::tempdir().unwrap();
        let project = project_adding_plugin_folder();
        let untrusted = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), untrusted, None);
        let hook = create_environment(&state, project.path());
        state.announce_environments();
        // Not trusted: the folder is refused by the environment's config, so not guarded.
        assert!(!imperium_policy::permissions::refused_write(&hook));
        // The user's settings change to trust the project; no session has loaded.
        state.replace_settings(trusting(project.path()));
        assert!(imperium_policy::permissions::refused_write(&hook));
    }
}
