use super::*;

impl Connection {
    /// The `environments` methods: `None` for a method of another group.
    pub(super) fn dispatch_environments(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            EnvironmentTestRuntime::NAME => {
                call_later::<EnvironmentTestRuntime, _>(c.params.take(), c.later, |params| {
                    let state = Arc::clone(&self.state);
                    async move {
                        let env = state.environments().find(&params.id).map_err(|error| {
                            ErrorObject::new(codes::INVALID_PARAMS, error.to_string())
                        })?;
                        let runner = state.runner();
                        Ok(
                            crate::runtimes::test(&env.runtime, runner.as_ref(), cfg!(windows))
                                .await,
                        )
                    }
                })
            }
            method if method.starts_with("environments.") => {
                self.environment_call(method, c.params.take())
            }
            _ => return None,
        })
    }
}

impl Connection {
    /// The `environments.*` methods; every change is announced to all clients.
    pub(super) fn environment_call(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ErrorObject> {
        let envs = self.state.environments();
        let failed = |error: EnvironmentError| {
            let code = match error {
                EnvironmentError::Save(_) | EnvironmentError::ReadOnly(_) => codes::INTERNAL_ERROR,
                _ => codes::INVALID_PARAMS,
            };
            ErrorObject::new(code, error.to_string())
        };
        let result = match method {
            EnvironmentList::NAME => {
                return call::<EnvironmentList>(params, |_: EnvironmentListParams| {
                    Ok(self.state.environment_list())
                });
            }
            EnvironmentConfigGet::NAME => {
                return call::<EnvironmentConfigGet>(params, |p: EnvironmentRef| {
                    let env = envs.find(&p.id).map_err(failed)?;
                    Ok(self.state.environment_config(&env.id))
                });
            }
            EnvironmentCreate::NAME => {
                call::<EnvironmentCreate>(params, |p: EnvironmentCreateParams| {
                    let env = envs.create(p).map_err(failed)?;
                    self.state.sync_environment_agent(&env.id);
                    Ok(envs.get(&env.id).unwrap_or(env))
                })
            }
            EnvironmentUpdate::NAME => {
                call::<EnvironmentUpdate>(params, |p: EnvironmentUpdateParams| {
                    let id = p.id.clone();
                    let renamed = p.name.is_some();
                    let runtime_changed = p.runtime.is_some();
                    let permissions_changed = p.permissions.is_some();
                    let env = envs.update(p).map_err(failed)?;
                    if permissions_changed {
                        // Running sessions follow the new rules from their next call.
                        self.state
                            .permissions()
                            .set_environment(&id, &env.permissions.rules);
                    }
                    if runtime_changed {
                        // The agents' tools follow the new runtime from their next start.
                        crate::project_agent::restart_environment_agents(&self.state, &id, None);
                    } else {
                        self.state.sync_environment_agent(&id);
                    }
                    if renamed {
                        crate::project_agent::retitle_environment_agents(&self.state, &id);
                    }
                    Ok(envs.get(&id).unwrap_or(env))
                })
            }
            EnvironmentAgentList::NAME => {
                return call::<EnvironmentAgentList>(params, |p: EnvironmentAgentListParams| {
                    let env = envs.get(&p.environment_id).ok_or_else(|| {
                        failed(EnvironmentError::NotFound(p.environment_id.clone()))
                    })?;
                    Ok(EnvironmentAgentListResult { agents: env.agents })
                });
            }
            EnvironmentAgentCreate::NAME => {
                call::<EnvironmentAgentCreate>(params, |p: EnvironmentAgentCreateParams| {
                    // Checked before the agent is added, so a refused profile adds nothing.
                    let permissions = p
                        .permissions
                        .as_ref()
                        .map(crate::permissions::checked_agent_permissions)
                        .transpose()
                        .map_err(|message| failed(EnvironmentError::Invalid(message)))?;
                    let agent = envs
                        .add_agent(&p.environment_id, &p.name, p.role.as_deref())
                        .map_err(failed)?;
                    if let Some(initiative) = p.initiative {
                        envs.set_agent_initiative(&p.environment_id, &agent.id, initiative)
                            .map_err(failed)?;
                        if initiative == imperium_protocol::initiative::Initiative::Act {
                            self.state
                                .initiative()
                                .postpone_review(&agent.id, imperium_api::time::unix_ms());
                        }
                    }
                    let agent = match &permissions {
                        Some(permissions) => envs
                            .set_agent_permissions(&p.environment_id, &agent.id, permissions)
                            .map_err(failed)?,
                        None => agent,
                    };
                    // Started at once; an environment without folders starts it when one is
                    // added. Whether it did, and if not why, goes back with it.
                    let failed = crate::project_agent::sync_environment_agent_report(
                        &self.state,
                        &p.environment_id,
                    );
                    let agent = self.agent_of(&p.environment_id, &agent.id).unwrap_or(agent);
                    let reason = failed
                        .into_iter()
                        .find(|(id, _)| *id == agent.id)
                        .map(|(_, reason)| reason);
                    Ok(EnvironmentAgentCreated {
                        started: self.state.project_agents().agent(&agent.id).is_some(),
                        reason,
                        agent,
                    })
                })
            }
            EnvironmentAgentUpdate::NAME => {
                call::<EnvironmentAgentUpdate>(params, |p: EnvironmentAgentUpdateParams| {
                    let permissions = p
                        .permissions
                        .as_ref()
                        .map(crate::permissions::checked_agent_permissions)
                        .transpose()
                        .map_err(|message| failed(EnvironmentError::Invalid(message)))?;
                    let before = self.agent_of(&p.environment_id, &p.agent_id);
                    let was_act = before.as_ref().is_some_and(|agent| {
                        agent.initiative == imperium_protocol::initiative::Initiative::Act
                    });
                    let agent = envs
                        .update_agent(
                            &p.environment_id,
                            &p.agent_id,
                            p.name.as_deref(),
                            p.role.as_deref(),
                        )
                        .map_err(failed)?;
                    // A changed profile applies at once to the agent's running sessions and
                    // to the ones it started, without a restart.
                    let agent = match &permissions {
                        Some(permissions) => {
                            let agent = envs
                                .set_agent_permissions(&p.environment_id, &agent.id, permissions)
                                .map_err(failed)?;
                            self.state
                                .permissions()
                                .set_agent(&agent.id, &agent.permissions);
                            self.state
                                .agent_permissions_changed(&agent.id, &agent.permissions);
                            agent
                        }
                        None => agent,
                    };
                    // Only a changed name or role restarts the agent (resuming its
                    // conversation); saving the same values leaves it running untouched.
                    let changed = before.is_none_or(|before| {
                        before.name != agent.name || before.role != agent.role
                    });
                    let could_not_start = if changed {
                        let could_not_start = crate::project_agent::restart_environment_agents(
                            &self.state,
                            &p.environment_id,
                            Some(&agent.id),
                        );
                        crate::project_agent::retitle_environment_agents(
                            &self.state,
                            &p.environment_id,
                        );
                        could_not_start
                    } else {
                        Vec::new()
                    };
                    if let Some(initiative) = p.initiative {
                        // Applies from the agent's next look; nothing restarts.
                        envs.set_agent_initiative(&p.environment_id, &agent.id, initiative)
                            .map_err(failed)?;
                        // The first review after `act` is turned on is a full interval away, so
                        // the user can see what the agent suggests before it starts anything.
                        if initiative == imperium_protocol::initiative::Initiative::Act && !was_act
                        {
                            self.state
                                .initiative()
                                .postpone_review(&agent.id, imperium_api::time::unix_ms());
                        }
                        crate::initiative::announce(&self.state, &agent.id);
                    }
                    let agent = self.agent_of(&p.environment_id, &agent.id).unwrap_or(agent);
                    let reason = could_not_start
                        .into_iter()
                        .find(|(id, _)| *id == agent.id)
                        .map(|(_, reason)| reason);
                    Ok(EnvironmentAgentCreated {
                        started: self.state.project_agents().agent(&agent.id).is_some(),
                        reason,
                        agent,
                    })
                })
            }
            EnvironmentAgentDelete::NAME => {
                call::<EnvironmentAgentDelete>(params, |p: EnvironmentAgentRef| {
                    envs.remove_agent(&p.environment_id, &p.agent_id)
                        .map_err(failed)?;
                    self.state.sync_environment_agent(&p.environment_id);
                    self.state.initiative().forget_agent(&p.agent_id);
                    Ok(Accepted {})
                })
            }
            EnvironmentDelete::NAME => call::<EnvironmentDelete>(params, |p: EnvironmentRef| {
                let agents: Vec<String> = envs
                    .get(&p.id)
                    .map(|env| env.agents.iter().map(|agent| agent.id.clone()).collect())
                    .unwrap_or_default();
                envs.delete(&p.id).map_err(failed)?;
                self.state.sync_environment_agent(&p.id);
                for agent in agents {
                    self.state.initiative().forget_agent(&agent);
                }
                Ok(Accepted {})
            }),
            EnvironmentAddFolder::NAME => {
                call::<EnvironmentAddFolder>(params, |p: EnvironmentFolderParams| {
                    let env = envs.add_folder(&p.id, &p.folder).map_err(failed)?;
                    // An agent waiting for a first folder can start now.
                    self.state.sync_environment_agent(&p.id);
                    Ok(envs.get(&p.id).unwrap_or(env))
                })
            }
            EnvironmentRemoveFolder::NAME => {
                call::<EnvironmentRemoveFolder>(params, |p: EnvironmentFolderParams| {
                    envs.remove_folder(&p.id, &p.folder).map_err(failed)
                })
            }
            other => Err(ErrorObject::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )),
        };
        if result.is_ok() {
            self.state.announce_environments();
        }
        result
    }
}
