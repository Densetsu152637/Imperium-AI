use super::*;
use imperium_protocol::jsonrpc::server_features;

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
                if params.key == "isolation.tier" && params.value.as_str() == Some("container") {
                    self.refuse_on_server(server_features::CONTAINER_TIER)?;
                }
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
                    let server = self.on_server();
                    async move {
                        // Only a person's own process makes a grant (see `crate::peer`), and
                        // never on a server: there it is refused before any peer is looked at.
                        let mut notes = Vec::new();
                        if server && (params.trusted || params.interactive) {
                            let why =
                                errors::unavailable_on_server(server_features::TERMINAL_GRANT);
                            if params.trusted {
                                // The command-line client installs the mod in its sandbox
                                // instead when the message starts with this.
                                return Err(errors::unavailable_on_server_with(
                                    server_features::TERMINAL_GRANT,
                                    format!(
                                        "{}: {}",
                                        imperium_protocol::errors::TRUST_NOT_GRANTED,
                                        why.message
                                    ),
                                ));
                            }
                            params.interactive = false;
                            notes.push(format!(
                                "the approval was not counted as given at a terminal: {}",
                                why.message
                            ));
                        } else if params.trusted || params.interactive {
                            let may = match peer {
                                Some((pid, accepted_at)) => {
                                    let daemon = Arc::clone(&state);
                                    tokio::task::spawn_blocking(move || {
                                        crate::peer::may_grant(&daemon, pid, accepted_at).is_ok()
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
                call_later::<PluginsMarketplaceRemove, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::marketplaces::remove(&state, params)
                            .await
                            .map_err(refused)
                    }
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
            PluginsInspect::NAME => {
                call_later::<PluginsInspect, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::plugin_install::inspect(&state, params)
                            .await
                            .map_err(refused)
                    }
                })
            }
            // Reads and hashes one folder, so it runs off the connection's thread.
            PluginsInspectFolder::NAME => {
                call_later::<PluginsInspectFolder, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::plugin_install::inspect_folder(&state, &params)
                        })
                        .await
                        .map_err(|error| refused(error.to_string()))?
                        .map_err(refused)
                    }
                })
            }
            PluginsInstall::NAME => {
                call_later::<PluginsInstall, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::plugin_install::install(&state, params)
                            .await
                            .map_err(refused)
                    }
                })
            }
            PluginsList::NAME => {
                call::<PluginsList>(c.params.take(), |params: PluginsListParams| {
                    crate::plugin_install::list(&self.state, params.environment.as_deref())
                        .map_err(refused)
                })
            }
            PluginsSetSecret::NAME => {
                call::<PluginsSetSecret>(c.params.take(), |params: PluginsSetSecretParams| {
                    crate::plugin_install::set_secret(&self.state, params).map_err(refused)
                })
            }
            PluginsDeleteSecret::NAME => {
                call::<PluginsDeleteSecret>(c.params.take(), |params: PluginsDeleteSecretParams| {
                    crate::plugin_install::delete_secret(&self.state, params).map_err(refused)
                })
            }
            PluginsSuggestions::NAME => {
                call::<PluginsSuggestions>(c.params.take(), |params: PluginsSuggestionsParams| {
                    crate::plugin_suggestions::suggestions(&self.state, params).map_err(refused)
                })
            }
            PluginsDismissSuggestion::NAME => call::<PluginsDismissSuggestion>(
                c.params.take(),
                |params: PluginsDismissSuggestionParams| {
                    crate::plugin_suggestions::dismiss(&self.state, params).map_err(refused)
                },
            ),
            PluginsSetEnabled::NAME => {
                call::<PluginsSetEnabled>(c.params.take(), |params: PluginsSetEnabledParams| {
                    crate::plugin_install::set_enabled(&self.state, params).map_err(refused)
                })
            }
            PluginsUpdate::NAME => {
                call_later::<PluginsUpdate, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::plugin_install::update(&state, params)
                            .await
                            .map_err(refused)
                    }
                })
            }
            PluginsUninstall::NAME => {
                call_later::<PluginsUninstall, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        crate::plugin_install::uninstall(&state, params)
                            .await
                            .map_err(refused)
                    }
                })
            }
            // Synchronous: a challenge belongs to this connection, and the hash is cached.
            PluginsApprovalChallenge::NAME => call::<PluginsApprovalChallenge>(
                c.params.take(),
                |params: PluginsApprovalChallengeParams| {
                    self.refuse_on_server(server_features::PLUGIN_APPROVAL)?;
                    crate::app_identity::local_only(self.identity.as_deref()).map_err(refused)?;
                    let subject = crate::plugin_records::Subject::parse(params.kind.as_deref())
                        .map_err(refused)?;
                    let folder =
                        crate::plugin_install::resolve_subject(&self.state, subject, &params.name)
                            .map_err(refused)?;
                    let asked = crate::app_identity::Asked {
                        folder: &folder,
                        hash: &params.sha256,
                        runs: &params.runs_digest,
                        now_ms: imperium_api::time::unix_ms(),
                        subject,
                    };
                    crate::app_identity::challenge(
                        &crate::app_identity::InstalledApp::of(&self.state),
                        self.identity.as_deref(),
                        &mut self.challenges,
                        &asked,
                    )
                    .map_err(refused)
                },
            ),
            // Checks 1 to 5 now, on this connection, which holds the challenge; the presence
            // check can wait minutes for the user, so it and the record run off the connection.
            PluginsApprove::NAME => self.plugins_approve(c),
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

    /// `plugins.approve`: refused at once over anything but the local socket or for a name that
    /// resolves to nothing; otherwise answered with `{approved, kind, reason}` once the presence
    /// check and the record are done.
    fn plugins_approve(&mut self, c: &mut Dispatch<'_>) -> Result<Value, ErrorObject> {
        let params: PluginsApproveParams = decode::<PluginsApprove>(c.params.take())?;
        self.refuse_on_server(server_features::PLUGIN_APPROVAL)?;
        crate::app_identity::local_only(self.identity.as_deref()).map_err(refused)?;
        let subject =
            crate::plugin_records::Subject::parse(params.kind.as_deref()).map_err(refused)?;
        let folder = crate::plugin_install::resolve_subject(&self.state, subject, &params.name)
            .map_err(refused)?;
        let asked = crate::app_identity::Asked {
            folder: &folder,
            hash: &params.sha256,
            runs: &params.runs_digest,
            now_ms: imperium_api::time::unix_ms(),
            subject,
        };
        let app = crate::app_identity::InstalledApp::of(&self.state);
        let redeemed = crate::app_identity::redeem(
            self.state.paths(),
            &app,
            self.identity.as_deref(),
            &mut self.challenges,
            &asked,
            &params.challenge,
        );
        let state = Arc::clone(&self.state);
        let peer = self.identity.clone();
        *c.later = Some(Box::pin(async move {
            let outcome = match redeemed {
                Ok(redeemed) => tokio::task::spawn_blocking(move || {
                    crate::app_identity::conclude(
                        state.paths(),
                        &app,
                        crate::plugin_records::production_verifier(),
                        &crate::app_identity::presence::System,
                        peer.as_deref(),
                        &redeemed,
                    )
                })
                .await
                .map_err(|error| refused(error.to_string()))?,
                Err(reason) => Err(reason),
            };
            Ok(to_value(&crate::app_identity::answer(outcome)))
        }));
        Ok(Value::Null)
    }
}
