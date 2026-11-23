use super::*;

impl Connection {
    /// The `config` methods: `None` for a method of another group.
    pub(super) fn dispatch_config(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            ConfigDescribe::NAME => {
                call::<ConfigDescribe>(c.params.take(), |_: ConfigDescribeParams| {
                    crate::config::describe(&self.state)
                })
            }
            WorkspacesWhere::NAME => {
                call::<WorkspacesWhere>(c.params.take(), |_: WorkspacesWhereParams| {
                    Ok(self.state.workspaces_where())
                })
            }
            WorkspacesMove::NAME => {
                call_later::<WorkspacesMove, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        tokio::task::spawn_blocking(move || state.move_workspaces(&params.path))
                            .await
                            .map_err(|error| refused(error.to_string()))?
                            .map_err(refused)
                    }
                })
            }
            WorkspacesOpen::NAME => call::<WorkspacesOpen>(c.params.take(), |p| {
                self.state
                    .open_workspace(&p.environment_id, &p.target)
                    .map_err(refused)?;
                Ok(Accepted {})
            }),
            // Changing the working folders' place can move a lot of files: it runs off the
            // connection, so progress notifications go out while it works and other calls are
            // answered meanwhile.
            ConfigSet::NAME
                if c.params
                    .as_ref()
                    .and_then(|params| params.get("key"))
                    .and_then(Value::as_str)
                    == Some(crate::state::WORKSPACES_ROOT_SETTING) =>
            {
                call_later::<ConfigSet, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        tokio::task::spawn_blocking(move || crate::config::set(&state, params))
                            .await
                            .map_err(|error| refused(error.to_string()))?
                    }
                })
            }
            ConfigSet::NAME => call::<ConfigSet>(c.params.take(), |params: ConfigSetParams| {
                crate::config::set(&self.state, params)
            }),
            PersonalitiesList::NAME => {
                call::<PersonalitiesList>(c.params.take(), |_: PersonalitiesListParams| {
                    Ok(crate::config::personalities(&self.state))
                })
            }
            ModList::NAME => call::<ModList>(c.params.take(), |_: ModListParams| {
                let mut listing =
                    crate::mods::list(&self.state.mods_dir(), &crate::mods::bundled_roots());
                crate::mod_install::apply_approvals(&self.state, &mut listing);
                Ok(listing)
            }),
            ModInspect::NAME => {
                call_later::<ModInspect, _>(c.params.take(), c.later, |params: ModInspectParams| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::mod_install::inspect(&state, params)
                            .await
                            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                    }
                })
            }
            ModInstall::NAME => call_later::<ModInstall, _>(
                c.params.take(),
                c.later,
                |mut params: ModInstallParams| {
                    let state = Arc::clone(&self.state);
                    let peer = self.peer;
                    async move {
                        // Only a person's own process makes a grant (see `crate::peer`).
                        let mut notes = Vec::new();
                        if params.trusted || params.interactive {
                            let may = match peer {
                                Some((pid, accepted_at)) => {
                                    tokio::task::spawn_blocking(move || {
                                        crate::peer::may_grant(pid, accepted_at)
                                    })
                                    .await
                                    .unwrap_or(false)
                                }
                                None => false,
                            };
                            if !may {
                                let why = "a grant needs a process of your own on the local socket (not one an agent ran, not over TCP or SSH): run `imperium mods install` or `update` from your own terminal";
                                if params.trusted {
                                    return Err(ErrorObject::new(
                                        codes::INVALID_PARAMS,
                                        format!(
                                            "{}: {why}",
                                            imperium_protocol::errors::TRUST_NOT_GRANTED
                                        ),
                                    ));
                                }
                                params.interactive = false;
                                notes.push(format!(
                                    "the approval was not counted as given at a terminal: {why}"
                                ));
                            }
                        }
                        let mut done = crate::mod_install::install(&state, params)
                            .await
                            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                        done.notes.extend(notes);
                        Ok(done)
                    }
                },
            ),
            PluginsMarketplaceAdd::NAME => {
                call_later::<PluginsMarketplaceAdd, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::marketplaces::add(&state, params)
                            .await
                            .map_err(refused)
                    }
                })
            }
            PluginsMarketplaceList::NAME => call::<PluginsMarketplaceList>(
                c.params.take(),
                |_: PluginsMarketplaceListParams| {
                    crate::marketplaces::list(&self.state).map_err(refused)
                },
            ),
            PluginsMarketplaceRefresh::NAME => {
                call_later::<PluginsMarketplaceRefresh, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::marketplaces::refresh(&state, params)
                            .await
                            .map_err(refused)
                    }
                })
            }
            PluginsMarketplaceRemove::NAME => {
                call::<PluginsMarketplaceRemove>(c.params.take(), |params| {
                    crate::marketplaces::remove(&self.state, params).map_err(refused)
                })
            }
            PluginsBrowse::NAME => {
                call_later::<PluginsBrowse, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::marketplaces::browse(&state, params)
                            .await
                            .map_err(refused)
                    }
                })
            }
            ConfigChanges::NAME => {
                call::<ConfigChanges>(c.params.take(), |_: ConfigChangesParams| {
                    Ok(ConfigChangesResult {
                        changes: crate::config_changes::list(&self.state),
                    })
                })
            }
            ConfigUndo::NAME => call::<ConfigUndo>(c.params.take(), |params: ConfigUndoParams| {
                crate::config_changes::undo(&self.state, params.id)
                    .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
            }),
            _ => return None,
        })
    }
}
