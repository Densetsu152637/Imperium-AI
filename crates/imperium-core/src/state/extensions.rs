use super::*;

impl DaemonState {
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
    ) -> Extensions {
        let settings = if profile.is_some_and(|profile| profile.local_files_only()) {
            // Not the environment's narrowed settings.
            std::borrow::Cow::Owned((*self.settings()).clone())
        } else {
            std::borrow::Cow::Borrowed(settings)
        };
        // A project's agent files are read only when the user trusts the project, as for its
        // `.mcp.json` servers.
        let project_trusted = self.settings().mcp.trusts(project_dir);
        // Folders an agent installed or a marketplace installed hold downloaded content: no
        // daemon `${VAR}`, no rewriting hooks. The record says which; a folder's provenance file
        // can only add to the set.
        let mut plugin_dirs = vec![self.paths.config_dir().join("plugins")];
        plugin_dirs.extend(settings.extensions.plugin_dirs.iter().cloned());
        let mut downloaded = imperium_extensions::plugins::agent_installed_folders(&plugin_dirs);
        // The load-time gate: a plugin it withholds loads nothing, whatever `disabled_plugins` says.
        let gate =
            crate::plugin_records::gate(&self.paths, crate::plugin_records::production_verifier());
        downloaded.extend(gate.downloaded);
        let mut extensions = imperium_extensions::load_trusting(
            &Sources {
                config_dir: self.paths.config_dir().to_path_buf(),
                project_dir: project_dir.to_path_buf(),
                plugin_dirs: settings.extensions.plugin_dirs.clone(),
                // The user's own folders, never what an environment's configuration merged in.
                user_plugin_dirs: self.settings().extensions.plugin_dirs.clone(),
                disabled_plugins: settings.extensions.disabled_plugins.clone(),
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
            },
            &|plugin| profile.is_none_or(|profile| profile.allows_plugin(plugin)),
            project_trusted,
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
            state.extensions(project.path(), &settings, None)
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
}
