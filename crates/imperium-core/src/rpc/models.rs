use super::*;

impl Connection {
    /// The `models` methods: `None` for a method of another group.
    pub(super) fn dispatch_models(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            ModelUnload::NAME => call_later::<ModelUnload, _>(
                c.params.take(),
                c.later,
                |params: ModelUnloadParams| {
                    let state = Arc::clone(&self.state);
                    async move {
                        state
                            .unload_local_model(&params.model)
                            .await
                            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                        Ok(ModelUnloadResult { unloaded: true })
                    }
                },
            ),
            ModelList::NAME => call::<ModelList>(c.params.take(), |_: ModelListParams| {
                Ok(self.state.model_table())
            }),
            ModelConfig::NAME => call::<ModelConfig>(c.params.take(), |_: ModelConfigParams| {
                Ok(crate::model_config::config(&self.state))
            }),
            // A saved `extra_args` is a command-line tail the daemon starts a model server with
            // (models.toml): only a person's own process on the local socket may change it.
            ModelSetConfig::NAME
                if c.params.as_ref().is_some_and(|params| {
                    serde_json::from_value::<ModelSetConfigParams>(params.clone()).is_ok_and(
                        |params| {
                            !params.dry_run
                                && params
                                    .local
                                    .iter()
                                    .any(|update| update.settings.extra_args.is_some())
                        },
                    )
                }) =>
            {
                match self.peer {
                    None => call::<ModelSetConfig>(c.params.take(), |_: ModelSetConfigParams| {
                        Err(ErrorObject::new(
                            codes::INVALID_PARAMS,
                            super::config::USER_ONLY,
                        ))
                    }),
                    peer => call_later::<ModelSetConfig, _>(
                        c.params.take(),
                        c.later,
                        |params: ModelSetConfigParams| {
                            let state = Arc::clone(&self.state);
                            let daemon_pid = self.daemon_pid;
                            async move {
                                if !super::config::local_user(Arc::clone(&state), peer, daemon_pid)
                                    .await
                                {
                                    return Err(ErrorObject::new(
                                        codes::INVALID_PARAMS,
                                        super::config::USER_ONLY,
                                    ));
                                }
                                tokio::task::spawn_blocking(move || {
                                    crate::model_config::set_config(&state, params).map_err(
                                        |message| ErrorObject::new(codes::INVALID_PARAMS, message),
                                    )
                                })
                                .await
                                .map_err(|error| {
                                    ErrorObject::new(codes::INTERNAL_ERROR, error.to_string())
                                })?
                            }
                        },
                    ),
                }
            }
            ModelSetConfig::NAME => {
                call::<ModelSetConfig>(c.params.take(), |params: ModelSetConfigParams| {
                    crate::model_config::set_config(&self.state, params)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                })
            }
            LocalModelList::NAME => {
                call::<LocalModelList>(c.params.take(), |_: LocalModelListParams| {
                    Ok(self.state.local_models())
                })
            }
            ModelFolderAdd::NAME => {
                call_later::<ModelFolderAdd, _>(c.params.take(), c.later, |params| {
                    crate::onboarding::add_folder(Arc::clone(&self.state), params)
                })
            }
            ModelFolderRemove::NAME => {
                call_later::<ModelFolderRemove, _>(c.params.take(), c.later, |params| {
                    crate::onboarding::remove_folder(Arc::clone(&self.state), params)
                })
            }
            ModelUse::NAME => call::<ModelUse>(c.params.take(), |params: ModelUseParams| {
                crate::onboarding::use_model(&self.state, params)
            }),
            ModelAddEndpoint::NAME => {
                call::<ModelAddEndpoint>(c.params.take(), |params: ModelAddEndpointParams| {
                    crate::onboarding::add_endpoint(&self.state, params)
                })
            }
            ModelTest::NAME => call_later::<ModelTest, _>(c.params.take(), c.later, |params| {
                crate::onboarding::test_model(Arc::clone(&self.state), params)
            }),
            RuntimeStatus::NAME => call_later::<RuntimeStatus, _>(c.params.take(), c.later, |_| {
                let state = Arc::clone(&self.state);
                async move { Ok(crate::onboarding::runtime_status(state).await) }
            }),
            // Naming a `llama-server` to launch is the user's own call; going back to the
            // installed runtime (no path) is not.
            RuntimeSetServer::NAME
                if c.params
                    .as_ref()
                    .and_then(|params| params.get("path"))
                    .is_some_and(|path| !path.is_null()) =>
            {
                match self.peer {
                    None => call::<RuntimeSetServer>(c.params.take(), |_| {
                        Err(ErrorObject::new(
                            codes::INVALID_PARAMS,
                            super::config::USER_ONLY,
                        ))
                    }),
                    peer => call_later::<RuntimeSetServer, _>(c.params.take(), c.later, |params| {
                        let state = Arc::clone(&self.state);
                        let daemon_pid = self.daemon_pid;
                        async move {
                            if !super::config::local_user(Arc::clone(&state), peer, daemon_pid)
                                .await
                            {
                                return Err(ErrorObject::new(
                                    codes::INVALID_PARAMS,
                                    super::config::USER_ONLY,
                                ));
                            }
                            crate::onboarding::set_server(state, params).await
                        }
                    }),
                }
            }
            RuntimeSetServer::NAME => {
                call_later::<RuntimeSetServer, _>(c.params.take(), c.later, |params| {
                    crate::onboarding::set_server(Arc::clone(&self.state), params)
                })
            }
            RuntimeInstall::NAME => {
                call_later::<RuntimeInstall, _>(c.params.take(), c.later, |_| {
                    crate::onboarding::install_runtime(Arc::clone(&self.state))
                })
            }
            RuntimeUpdateExternal::NAME => {
                call_later::<RuntimeUpdateExternal, _>(c.params.take(), c.later, |_| {
                    crate::llama_runtime::update_external(Arc::clone(&self.state))
                })
            }
            HfFiles::NAME => call_later::<HfFiles, _>(c.params.take(), c.later, |params| {
                crate::downloads::files(Arc::clone(&self.state), params)
            }),
            ModelDownload::NAME => {
                call_later::<ModelDownload, _>(c.params.take(), c.later, |params| {
                    crate::downloads::start(Arc::clone(&self.state), params)
                })
            }
            ModelDownloads::NAME => {
                call::<ModelDownloads>(c.params.take(), |_: ModelDownloadsParams| {
                    Ok(ModelDownloadsResult {
                        jobs: self.state.downloads().list(),
                    })
                })
            }
            ModelDownloadCancel::NAME => {
                call::<ModelDownloadCancel>(c.params.take(), |params: ModelDownloadCancelParams| {
                    Ok(ModelDownloadCancelResult {
                        found: self.state.downloads().cancel(&self.state, &params.job_id),
                    })
                })
            }
            ModelBench::NAME => call::<ModelBench>(c.params.take(), |params| {
                crate::bench::start(Arc::clone(&self.state), params)
            }),
            ModelBenchResults::NAME => {
                call::<ModelBenchResults>(c.params.take(), |params: ModelBenchResultsParams| {
                    Ok(crate::bench::results(&self.state, params))
                })
            }
            ModelBenchCancel::NAME => {
                call::<ModelBenchCancel>(c.params.take(), |params: ModelBenchCancelParams| {
                    Ok(ModelBenchCancelResult {
                        found: self.state.bench().cancel(&self.state, &params.run_id),
                    })
                })
            }
            _ => return None,
        })
    }
}
