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
            ModelRescan::NAME => call_later::<ModelRescan, _>(c.params.take(), c.later, |_| {
                crate::onboarding::rescan_models(Arc::clone(&self.state))
            }),
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
            RuntimeCheckUpdate::NAME => call_later::<RuntimeCheckUpdate, _>(
                c.params.take(),
                c.later,
                |params: RuntimeCheckUpdateParams| {
                    crate::llama_runtime::check_update(Arc::clone(&self.state), params.force)
                },
            ),
            RuntimeUpdate::NAME => call_later::<RuntimeUpdate, _>(c.params.take(), c.later, |_| {
                crate::llama_runtime::update(Arc::clone(&self.state))
            }),
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
