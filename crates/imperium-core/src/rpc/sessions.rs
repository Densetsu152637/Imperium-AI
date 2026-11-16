use super::*;

impl Connection {
    /// The `sessions` methods: `None` for a method of another group.
    pub(super) fn dispatch_sessions(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        Some(match c.method {
            SessionCreate::NAME => call::<SessionCreate>(c.params.take(), |params| {
                let (result, events) = self.create_session(params)?;
                *c.subscription = Some(Subscription {
                    session_id: result.session_id.clone(),
                    events,
                });
                Ok(result)
            }),
            SessionEvalCreate::NAME => call::<SessionEvalCreate>(c.params.take(), |params| {
                let (result, events) = self.create_eval_session(params)?;
                *c.subscription = Some(Subscription {
                    session_id: result.session_id.clone(),
                    events,
                });
                Ok(result)
            }),
            SessionEvalResult::NAME => {
                call::<SessionEvalResult>(c.params.take(), |params: SessionEvalResultParams| {
                    self.eval_forbidden()?;
                    let run = self.state.eval_run(&params.session_id).ok_or_else(|| {
                        ErrorObject::new(
                            codes::INVALID_PARAMS,
                            format!("{} is not an evaluation session", params.session_id),
                        )
                    })?;
                    let session = self.session(&params.session_id)?;
                    Ok(run.result(session.project_dir()))
                })
            }
            SessionPromptPreview::NAME => call_later::<SessionPromptPreview, _>(
                c.params.take(),
                c.later,
                |params: SessionEvalCreateParams| {
                    let state = Arc::clone(&self.state);
                    let forbidden = self.eval_forbidden().err();
                    async move {
                        if let Some(error) = forbidden {
                            return Err(error);
                        }
                        let prepared = crate::eval::Prepared::new(&params.options, true)
                            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                        state
                            .prompt_preview(
                                params.environment_id.as_deref(),
                                params.project_dir.as_deref(),
                                Arc::new(prepared),
                            )
                            .await
                            .map_err(|message| ErrorObject::new(codes::INTERNAL_ERROR, message))
                    }
                },
            ),
            SessionControl::NAME => {
                call_later::<SessionControl, _>(c.params.take(), c.later, |params| {
                    self.control(params)
                })
            }
            SessionList::NAME => call::<SessionList>(c.params.take(), |_: SessionListParams| {
                // Running sessions, then archived ones (marked `archived`).
                let mut sessions: Vec<_> = self
                    .state
                    .sessions()
                    .iter()
                    .map(|session| self.state.summary(session))
                    .collect();
                sessions.extend(self.state.archived_sessions());
                Ok(SessionListResult { sessions })
            }),
            SessionExport::NAME => call_later::<SessionExport, _>(
                c.params.take(),
                c.later,
                |params: SessionExportParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::export::export(&state, &params)).await }
                },
            ),
            SessionSearch::NAME => call_later::<SessionSearch, _>(
                c.params.take(),
                c.later,
                |params: SessionSearchParams| {
                    let state = Arc::clone(&self.state);
                    async move {
                        state
                            .search_sessions(params)
                            .await
                            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                    }
                },
            ),
            SessionSubscribe::NAME => {
                call::<SessionSubscribe>(c.params.take(), |params: SessionSubscribeParams| {
                    // An archived session's history comes from its journal; nothing follows.
                    if let Some(history) = self.state.archived_history(&params.session_id) {
                        return Ok(SessionSubscribeResult { history });
                    }
                    let (history, events) =
                        self.session(&params.session_id)?.subscribe_with_history();
                    *c.subscription = Some(Subscription {
                        session_id: params.session_id,
                        events,
                    });
                    Ok(SessionSubscribeResult { history })
                })
            }
            SessionSend::NAME => {
                call::<SessionSend>(c.params.take(), |params: SessionSendParams| {
                    let session = self.session(&params.session_id)?;
                    // A message that starts a turn waits on the session's budgets; steering
                    // joins a running turn, which always finishes.
                    let steering = params.mode == Some(imperium_protocol::session::SendMode::Steer)
                        && session.status().0 == imperium_protocol::session::SessionStatus::Working;
                    if !steering {
                        crate::budgets::check(
                            &self.state,
                            &params.session_id,
                            params.confirm_budget == Some(true),
                        )?;
                    }
                    // A person writing to background work wants an answer now: it stops giving
                    // way to the load.
                    if session.is_background() {
                        self.state.release_run_now(&session);
                    }
                    let attachments =
                        crate::attachments::verify(&self.state, &session, &params.attachments)?;
                    let id = session
                        .send_with_attachments(
                            params.text,
                            attachments,
                            params.mode.unwrap_or_default(),
                            params.agent_id.as_deref(),
                        )
                        .map_err(queue_error)?;
                    Ok(SessionSendResult { id })
                })
            }
            SessionAttach::NAME => {
                call::<SessionAttach>(c.params.take(), |params: SessionAttachParams| {
                    let session = self.session(&params.session_id)?;
                    crate::attachments::attach(&self.state, &session, params)
                })
            }
            VoiceTranscribe::NAME => {
                call_later::<VoiceTranscribe, _>(c.params.take(), c.later, |params| {
                    crate::attachments::transcribe(Arc::clone(&self.state), params)
                })
            }
            SessionQueue::NAME => {
                call::<SessionQueue>(c.params.take(), |params: SessionQueueParams| {
                    self.queue(params)
                })
            }
            SessionAgents::NAME => {
                call::<SessionAgents>(c.params.take(), |params: SessionAgentsParams| {
                    Ok(SessionAgentsResult {
                        agents: self.session(&params.session_id)?.agents(),
                    })
                })
            }
            SessionAgentResume::NAME => {
                call::<SessionAgentResume>(c.params.take(), |params: SessionAgentResumeParams| {
                    let model = self
                        .session(&params.session_id)?
                        .resume_agent(&params.agent_id, params.text)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                    Ok(SessionAgentResumeResult {
                        agent_id: params.agent_id,
                        model,
                    })
                })
            }
            SessionCheckpoints::NAME => call_later::<SessionCheckpoints, _>(
                c.params.take(),
                c.later,
                |params: SessionCheckpointsParams| {
                    let state = Arc::clone(&self.state);
                    async move {
                        blocking(move || crate::checkpoints::list(&state, &params.session_id)).await
                    }
                },
            ),
            SessionRewind::NAME => call_later::<SessionRewind, _>(
                c.params.take(),
                c.later,
                |params: SessionRewindParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::checkpoints::rewind(&state, &params)).await }
                },
            ),
            SessionBranch::NAME => call_later::<SessionBranch, _>(
                c.params.take(),
                c.later,
                |params: SessionBranchParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || state.branch(&params)).await }
                },
            ),
            SessionCompare::NAME => call_later::<SessionCompare, _>(
                c.params.take(),
                c.later,
                |params: SessionCompareParams| {
                    let state = Arc::clone(&self.state);
                    async move {
                        state
                            .compare(params)
                            .await
                            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                    }
                },
            ),
            SessionComparePick::NAME => {
                call::<SessionComparePick>(c.params.take(), |params: SessionComparePickParams| {
                    self.state
                        .compare_pick(&params)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                })
            }
            SessionReview::NAME => call_later::<SessionReview, _>(
                c.params.take(),
                c.later,
                |params: SessionReviewParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::review(&state, &params)).await }
                },
            ),
            SessionReviewHunk::NAME => call_later::<SessionReviewHunk, _>(
                c.params.take(),
                c.later,
                |params: SessionReviewHunkParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::act_on_hunk(&state, &params)).await }
                },
            ),
            SessionReviewCommit::NAME => call_later::<SessionReviewCommit, _>(
                c.params.take(),
                c.later,
                |params: SessionReviewCommitParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::commit(&state, &params)).await }
                },
            ),
            SessionReviewPullRequest::NAME => call_later::<SessionReviewPullRequest, _>(
                c.params.take(),
                c.later,
                |params: SessionReviewPullRequestParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::pull_request(&state, &params)).await }
                },
            ),
            SessionAgentAsk::NAME => call_later::<SessionAgentAsk, _>(
                c.params.take(),
                c.later,
                |params: SessionAgentAskParams| {
                    let session = self.session(&params.session_id);
                    async move {
                        let answer = session?
                            .ask_knowledge(&params.agent_id, &params.question, params.timeout_s)
                            .await
                            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                        Ok(SessionAgentAskResult {
                            question_id: answer.question_id,
                            outcome: answer.outcome,
                            answer: answer.answer,
                            degraded: answer.degraded,
                            untrusted_web: answer.untrusted_web,
                            untrusted_tools: answer.untrusted_tools,
                        })
                    }
                },
            ),
            SessionAgentKnowledge::NAME => call::<SessionAgentKnowledge>(
                c.params.take(),
                |params: SessionAgentKnowledgeParams| {
                    let card = self
                        .session(&params.session_id)?
                        .agent_knowledge(&params.agent_id, params.topic.as_deref(), params.retire)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                    Ok(SessionAgentKnowledgeResult {
                        agent_id: params.agent_id,
                        card,
                    })
                },
            ),
            SessionAgentCancel::NAME => {
                call::<SessionAgentCancel>(c.params.take(), |params: SessionAgentCancelParams| {
                    self.session(&params.session_id)?
                        .cancel_agent(&params.agent_id)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                    Ok(SessionAgentCancelResult {})
                })
            }
            SessionBrowserControl::NAME => {
                call_later::<SessionBrowserControl, _>(c.params.take(), c.later, |params| {
                    crate::browser_control::control(Arc::clone(&self.state), params)
                })
            }
            SessionBrowserAct::NAME => {
                call_later::<SessionBrowserAct, _>(c.params.take(), c.later, |params| {
                    crate::browser_control::act(Arc::clone(&self.state), params)
                })
            }
            SessionSuggestionRespond::NAME => {
                call_later::<SessionSuggestionRespond, _>(c.params.take(), c.later, |params| {
                    crate::suggestions::respond(Arc::clone(&self.state), params)
                })
            }
            SessionDelete::NAME => {
                call::<SessionDelete>(c.params.take(), |params: SessionDeleteParams| {
                    self.state
                        .delete_session(&params.session_id, params.remove_worktrees)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                })
            }
            SessionAnswer::NAME => {
                call::<SessionAnswer>(c.params.take(), |params: SessionAnswerParams| {
                    self.session(&params.session_id)?
                        .answer(params.request_id, params.answers)
                        .map_err(stopped)?;
                    Ok(Accepted {})
                })
            }
            SessionTools::NAME => call::<SessionTools>(c.params.take(), |_: SessionToolsParams| {
                Ok(SessionToolsResult {
                    tools: imperium_engine::bridge::tools()
                        .into_iter()
                        .map(|tool| BridgedToolInfo {
                            name: tool.name,
                            description: tool.description,
                            parameters: tool.parameters,
                            writes: tool.writes,
                        })
                        .collect(),
                })
            }),
            SessionRunTool::NAME => {
                call::<SessionRunTool>(c.params.take(), |params: SessionRunToolParams| {
                    let arguments = match params.arguments {
                        Value::Null => "{}".to_string(),
                        arguments @ Value::Object(_) => arguments.to_string(),
                        _ => {
                            return Err(ErrorObject::new(
                                codes::INVALID_PARAMS,
                                "arguments must be an object",
                            ));
                        }
                    };
                    let call_id = self
                        .session(&params.session_id)?
                        .start_tool(&params.tool, arguments, params.allow_write)
                        .map_err(|error| {
                            ErrorObject::new(codes::INVALID_PARAMS, error.to_string())
                        })?;
                    Ok(SessionRunToolResult { call_id })
                })
            }
            SessionToolResult::NAME => call_later::<SessionToolResult, _>(
                c.params.take(),
                c.later,
                |params: SessionToolResultParams| {
                    let session = self.session(&params.session_id);
                    async move {
                        let wait = std::time::Duration::from_millis(
                            params.wait_ms.unwrap_or(0).min(TOOL_RESULT_WAIT_MS),
                        );
                        match session?.tool_result(&params.call_id, wait).await {
                            imperium_engine::bridge::BridgeWait::Done(outcome) => {
                                Ok(SessionToolResultResult {
                                    done: true,
                                    ok: outcome.ok,
                                    output: outcome.output,
                                })
                            }
                            imperium_engine::bridge::BridgeWait::Pending => {
                                Ok(SessionToolResultResult {
                                    done: false,
                                    ok: false,
                                    output: String::new(),
                                })
                            }
                            imperium_engine::bridge::BridgeWait::Unknown => Err(ErrorObject::new(
                                codes::INVALID_PARAMS,
                                format!(
                                    "no tool call {:?} in this session (or its result was                                          already collected)",
                                    params.call_id
                                ),
                            )),
                        }
                    }
                },
            ),
            SessionRespond::NAME => {
                call::<SessionRespond>(c.params.take(), |params: SessionRespondParams| {
                    self.session(&params.session_id)?
                        .respond(params.request_id, params.decision)
                        .map_err(stopped)?;
                    Ok(Accepted {})
                })
            }
            _ => return None,
        })
    }
}

impl Connection {
    /// An error unless this connection may start evaluation sessions.
    pub(super) fn eval_forbidden(&self) -> Result<(), ErrorObject> {
        if self.may_eval() {
            Ok(())
        } else {
            Err(ErrorObject::new(
                codes::FORBIDDEN,
                "evaluation sessions are for the imperium command on this machine, not for \
                 phones, mods or the API",
            ))
        }
    }

    /// `session.eval_create`: a session with the options of an evaluation run.
    pub(super) fn create_eval_session(
        &self,
        params: SessionEvalCreateParams,
    ) -> Result<(SessionCreateResult, broadcast::Receiver<AgentEvent>), ErrorObject> {
        self.eval_forbidden()?;
        let prepared = crate::eval::Prepared::new(&params.options, false)
            .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
        let (session, events) = self
            .state
            .create_eval(
                params.environment_id.as_deref(),
                params.project_dir.as_deref(),
                Arc::new(prepared),
            )
            .map_err(|error| {
                let code = match &error {
                    CreateError::Project(..)
                    | CreateError::Environment(_)
                    | CreateError::Runtime(_)
                    | CreateError::Worktree(_) => codes::INVALID_PARAMS,
                    CreateError::Model(_) => codes::MODEL_UNAVAILABLE,
                    CreateError::Start(_) => codes::INTERNAL_ERROR,
                };
                ErrorObject::new(code, error.to_string())
            })?;
        let summary = self.state.summary(&session);
        Ok((
            SessionCreateResult {
                session_id: session.id().to_string(),
                model: session.model(),
                isolation: session.isolation().to_string(),
                environment_id: summary.environment_id,
                folders: summary.folders,
                worktrees: summary.worktrees,
            },
            events,
        ))
    }

    pub(super) fn create_session(
        &self,
        params: SessionCreateParams,
    ) -> Result<(SessionCreateResult, broadcast::Receiver<AgentEvent>), ErrorObject> {
        let model = params.model.as_deref();
        // A model picked for the session is checked without loading it; its first turn loads it.
        if model.is_some()
            && let Ok(resolved) = self.state.resolve_model(model)
        {
            self.state
                .check_selectable(&resolved)
                .map_err(|error| ErrorObject::new(codes::MODEL_UNAVAILABLE, error))?;
        }
        // Without a model (and no models.default) the session still starts; it waits for one.
        let options = || SessionOptions {
            may_wait_for_model: true,
            worktree: params.worktree.unwrap_or(false),
            worktree_branch: params.worktree_branch.clone(),
            subfolder: params.subfolder.clone(),
            read_only: params.read_only.unwrap_or(false),
            ..SessionOptions::default()
        };
        let created = match (&params.environment_id, &params.project_dir) {
            (Some(env), _) => self.state.create_in_environment(env, model, options()),
            // A bare folder: the environment it is the working directory of, made if needed.
            (None, Some(dir)) => match self.state.environments().for_folder(dir) {
                Ok(env) => {
                    let created = self.state.create_in_environment(&env.id, model, options());
                    self.state.announce_environments();
                    created
                }
                Err(error) => Err(error.into()),
            },
            // No environment or project chosen: the built-in Home environment.
            (None, None) => {
                self.state
                    .create_in_environment(crate::environments::HOME_ID, model, options())
            }
        };
        let (session, events) = created.map_err(|error| {
            let code = match &error {
                CreateError::Project(..)
                | CreateError::Environment(_)
                | CreateError::Runtime(_)
                | CreateError::Worktree(_) => codes::INVALID_PARAMS,
                CreateError::Model(ModelError::NoDefault) => {
                    return ErrorObject::new(
                        codes::MODEL_UNAVAILABLE,
                        "No model chosen and no default model is configured. Pick a model for \
                         the session, or set models.default in Settings → Models.",
                    );
                }
                CreateError::Model(_) => codes::MODEL_UNAVAILABLE,
                CreateError::Start(_) => codes::INTERNAL_ERROR,
            };
            ErrorObject::new(code, error.to_string())
        })?;
        let summary = self.state.summary(&session);
        Ok((
            SessionCreateResult {
                session_id: session.id().to_string(),
                model: session.model(),
                isolation: session.isolation().to_string(),
                environment_id: summary.environment_id,
                folders: summary.folders,
                worktrees: summary.worktrees,
            },
            events,
        ))
    }

    pub(super) fn agent_of(
        &self,
        environment_id: &str,
        agent_id: &str,
    ) -> Option<imperium_protocol::agents::EnvironmentAgent> {
        self.state
            .environments()
            .get(environment_id)?
            .agents
            .into_iter()
            .find(|agent| agent.id == agent_id)
    }

    /// `session.control`. Most actions apply at once; switching models and compacting wait for
    /// the session (compaction asks a model for a summary).
    pub(super) fn control(
        &self,
        params: SessionControlParams,
    ) -> impl Future<Output = Result<SessionControlResult, ErrorObject>> + Send + 'static {
        let state = Arc::clone(&self.state);
        async move {
            // Archiving works on archived sessions too (which are not running).
            match params.action.as_str() {
                "archive" => {
                    state.archive(&params.session_id).map_err(archive_error)?;
                    return Ok(SessionControlResult::default());
                }
                "unarchive" => {
                    state.unarchive(&params.session_id).map_err(archive_error)?;
                    return Ok(SessionControlResult::default());
                }
                // Archived sessions can be renamed too.
                "rename" => {
                    let title = params.title.as_deref().ok_or_else(|| {
                        ErrorObject::new(codes::INVALID_PARAMS, "rename needs title")
                    })?;
                    let renamed =
                        state
                            .set_title(&params.session_id, title, true)
                            .map_err(|error| {
                                ErrorObject::new(
                                    codes::INTERNAL_ERROR,
                                    format!("could not record the title: {error}"),
                                )
                            })?;
                    if !renamed {
                        return Err(ErrorObject::new(
                            codes::SESSION_NOT_FOUND,
                            "no such session",
                        ));
                    }
                    return Ok(SessionControlResult::default());
                }
                _ => {}
            }
            let session = state
                .session(&params.session_id)
                .ok_or_else(|| ErrorObject::new(codes::SESSION_NOT_FOUND, "no such session"))?;
            let fitted = match params.action.as_str() {
                "set_resume_policy" => {
                    session.set_resume_policy(params.resume_policy);
                    None
                }
                "stop" => {
                    session.stop();
                    None
                }
                "resume" => {
                    session.resume_work();
                    None
                }
                "resume_now" => {
                    session.resume_now();
                    if session.run_now().released() {
                        state.release_run_now(&session);
                    }
                    None
                }
                "watch" => {
                    let (Some(repo), Some(number)) = (params.repo, params.pull_request) else {
                        return Err(ErrorObject::new(
                            codes::INVALID_PARAMS,
                            "watch needs repo and pull_request",
                        ));
                    };
                    state.watch(&session, Some((repo, number)));
                    None
                }
                "unwatch" => {
                    state.watch(&session, None);
                    None
                }
                "set_model" => {
                    let model = state
                        .resolve_model(params.model.as_deref())
                        .map_err(|error| {
                            ErrorObject::new(codes::MODEL_UNAVAILABLE, error.to_string())
                        })?;
                    // Checked, not loaded: the session's next turn loads it.
                    state
                        .check_selectable(&model)
                        .map_err(|error| ErrorObject::new(codes::MODEL_UNAVAILABLE, error))?;
                    Some(session.set_model(model).await.map_err(switch_error)?)
                }
                "compact" => Some(session.compact().await.map_err(switch_error)?),
                "set_effort" => {
                    let effort = params
                        .effort
                        .as_deref()
                        .ok_or_else(|| {
                            ErrorObject::new(codes::INVALID_PARAMS, "set_effort needs effort")
                        })?
                        .parse::<imperium_api::Effort>()
                        .map_err(|error| {
                            ErrorObject::new(codes::INVALID_PARAMS, error.to_string())
                        })?;
                    state.set_effort(&session, effort);
                    return Ok(SessionControlResult {
                        effort: Some(session.effort().as_str().to_string()),
                        reasoning: Some(
                            session
                                .model()
                                .is_some_and(|model| state.model_reasons(&model)),
                        ),
                        ..SessionControlResult::default()
                    });
                }
                "set_permission_mode" => {
                    let mode = params
                        .permission_mode
                        .as_deref()
                        .ok_or_else(|| {
                            ErrorObject::new(
                                codes::INVALID_PARAMS,
                                "set_permission_mode needs permission_mode",
                            )
                        })?
                        .parse::<imperium_api::PermissionMode>()
                        .map_err(|error| {
                            ErrorObject::new(codes::INVALID_PARAMS, error.to_string())
                        })?;
                    state.set_permission_mode(&session, mode);
                    return Ok(SessionControlResult {
                        permission_mode: Some(session.permission_mode().as_str().to_string()),
                        ..SessionControlResult::default()
                    });
                }
                "set_pinned" => {
                    let pinned = params.pinned.ok_or_else(|| {
                        ErrorObject::new(codes::INVALID_PARAMS, "set_pinned needs pinned")
                    })?;
                    state.set_pinned(&session, pinned).map_err(|error| {
                        ErrorObject::new(
                            codes::INTERNAL_ERROR,
                            format!("could not record the pin: {error}"),
                        )
                    })?;
                    None
                }
                other => {
                    return Err(ErrorObject::new(
                        codes::INVALID_PARAMS,
                        format!("unknown action {other:?}"),
                    ));
                }
            };
            Ok(match fitted {
                Some(context) => SessionControlResult {
                    reasoning: session.model().map(|model| state.model_reasons(&model)),
                    model: session.model(),
                    context: Some(context),
                    ..SessionControlResult::default()
                },
                None => SessionControlResult::default(),
            })
        }
    }

    /// `session.queue`: lists, edits, removes, or steers an agent's queued messages.
    pub(super) fn queue(
        &self,
        params: SessionQueueParams,
    ) -> Result<SessionQueueResult, ErrorObject> {
        let session = self.session(&params.session_id)?;
        let agent = params.agent_id.as_deref();
        let id = || {
            params
                .id
                .as_deref()
                .ok_or_else(|| ErrorObject::new(codes::INVALID_PARAMS, "this action needs id"))
        };
        match params.action.as_str() {
            "list" => {}
            "remove" => session.remove_queued(agent, id()?).map_err(queue_error)?,
            "steer" => session.steer_queued(agent, id()?).map_err(queue_error)?,
            "edit" => {
                let text = params
                    .text
                    .clone()
                    .ok_or_else(|| ErrorObject::new(codes::INVALID_PARAMS, "edit needs text"))?;
                session
                    .edit_queued(agent, id()?, text)
                    .map_err(queue_error)?;
            }
            other => {
                return Err(ErrorObject::new(
                    codes::INVALID_PARAMS,
                    format!("unknown queue action {other:?}; use list, edit, remove, or steer"),
                ));
            }
        }
        Ok(SessionQueueResult {
            messages: session.queue(agent).map_err(queue_error)?,
        })
    }

    pub(super) fn session(&self, id: &str) -> Result<Arc<Session>, ErrorObject> {
        self.state
            .session(id)
            .ok_or_else(|| ErrorObject::new(codes::SESSION_NOT_FOUND, format!("no session {id:?}")))
    }
}
