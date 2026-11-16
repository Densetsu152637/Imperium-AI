use super::*;

impl DaemonState {
    /// Where UI mods are installed.
    pub fn mods_dir(&self) -> std::path::PathBuf {
        self.paths.config_dir().join("mods")
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
            },
            &|plugin| profile.is_none_or(|profile| profile.allows_plugin(plugin)),
            project_trusted,
        );
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
