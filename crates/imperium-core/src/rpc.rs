//! Request handling for one client connection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine;
use imperium_engine::{QueueError, Session, SwitchError};
use imperium_protocol::budgets::{BudgetsGet, BudgetsGetParams, BudgetsSet, BudgetsUsage};
use imperium_protocol::jsonrpc::{ErrorObject, Request, RequestId, Response, codes};
use imperium_protocol::methods::{
    Accepted, ApprovalList, ApprovalListParams, ApprovalListResult, ApprovalRespond,
    ApprovalRespondParams, ApprovalRespondResult, Handshake, HandshakeParams, HandshakeResult,
    LocalModelList, LocalModelListParams, Method, ModList, ModListParams, ModelList,
    ModelListParams, Peer, PetInteract, PetInteractParams, PetInteractResult, PetList,
    PetListParams, PetListResult, PetStrip, PetStripParams, PetStripResult, PhoneDevices,
    PhoneDevicesParams, PhoneDevicesResult, PhonePair, PhonePairParams, PhonePairResult,
    PhoneRevoke, PhoneRevokeParams, PhoneRevokeResult, Ping, PingParams, PingResult,
    ProjectAgentRow, ProjectList, ProjectListParams, ProjectListResult, ProjectMessage,
    ProjectMessageParams, ProjectMessageResult, ScheduleList, ScheduleListParams,
    ScheduleListResult, ScheduleRun, ScheduleRunParams, ScheduleRunResult, SessionAnswer,
    SessionAnswerParams, SessionCreate, SessionCreateParams, SessionCreateResult, SessionDelete,
    SessionDeleteParams, SessionList, SessionListParams, SessionListResult, SessionRespond,
    SessionRespondParams, SessionSearch, SessionSearchParams, SessionSend, SessionSendParams,
    SessionSubscribe, SessionSubscribeParams, SessionSubscribeResult, Shutdown, ShutdownParams,
    ShutdownResult,
};
use imperium_protocol::permissions::{
    PermissionsAddRule, PermissionsGet, PermissionsGetParams, PermissionsSet,
};
use imperium_protocol::session::AgentEvent;
use imperium_protocol::{PROTOCOL_VERSION, ProtocolVersion};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::environments::EnvironmentError;
use crate::state::{CreateError, DaemonState, SessionOptions};
use imperium_engine::ModelError;
use imperium_protocol::methods::SessionSuggestionRespond;
use imperium_protocol::methods::{
    ConfigChanges, ConfigChangesParams, ConfigChangesResult, ConfigDescribe, ConfigDescribeParams,
    ConfigSet, ConfigSetParams, ConfigUndo, ConfigUndoParams,
};
use imperium_protocol::methods::{
    EnvironmentAddFolder, EnvironmentCreate, EnvironmentCreateParams, EnvironmentDelete,
    EnvironmentFolderParams, EnvironmentList, EnvironmentListParams, EnvironmentRef,
    EnvironmentRemoveFolder, EnvironmentTestRuntime, EnvironmentUpdate, EnvironmentUpdateParams,
};
use imperium_protocol::methods::{
    HfFiles, ModelDownload, ModelDownloadCancel, ModelDownloadCancelParams,
    ModelDownloadCancelResult, ModelDownloads, ModelDownloadsParams, ModelDownloadsResult,
};
use imperium_protocol::methods::{
    McpCatalog, McpCatalogParams, McpInstall, McpList, McpListParams, McpRemove, McpSave,
    McpSetEnabled, McpTest,
};
use imperium_protocol::methods::{
    ModelAddEndpoint, ModelAddEndpointParams, ModelFolderAdd, ModelFolderRemove, ModelTest,
    ModelUse, ModelUseParams, RuntimeInstall, RuntimeSetServer, RuntimeStatus,
    RuntimeUpdateExternal,
};
use imperium_protocol::methods::{
    ModelConfig, ModelConfigParams, ModelSetConfig, ModelSetConfigParams,
};
use imperium_protocol::methods::{
    ModelUnload, ModelUnloadParams, ModelUnloadResult, SystemResources, SystemResourcesParams,
    SystemResourcesSubscribe, SystemResourcesSubscribeParams, SystemResourcesSubscribeResult,
};
use imperium_protocol::methods::{
    SessionAgentCancel, SessionAgentCancelParams, SessionAgentCancelResult, SessionAgentResume,
    SessionAgentResumeParams, SessionAgentResumeResult, SessionAgents, SessionAgentsParams,
    SessionAgentsResult,
};
use imperium_protocol::methods::{SessionAttach, SessionAttachParams, VoiceTranscribe};
use imperium_protocol::methods::{
    SessionCheckpoints, SessionCheckpointsParams, SessionReview, SessionReviewCommit,
    SessionReviewCommitParams, SessionReviewHunk, SessionReviewHunkParams, SessionReviewParams,
    SessionReviewPullRequest, SessionReviewPullRequestParams, SessionRewind, SessionRewindParams,
};
use imperium_protocol::methods::{SessionControl, SessionControlParams, SessionControlResult};
use imperium_protocol::methods::{
    SessionQueue, SessionQueueParams, SessionQueueResult, SessionSendResult,
};

/// A session's events, to be forwarded to this connection as notifications.
#[derive(Debug)]
pub struct Subscription {
    pub session_id: String,
    pub events: broadcast::Receiver<AgentEvent>,
}

/// A reply that waits on something slow, such as the network.
pub type Deferred = Pin<Box<dyn Future<Output = Response> + Send>>;

/// A method's result that is still being worked out.
type Later = Pin<Box<dyn Future<Output = Result<Value, ErrorObject>> + Send>>;

/// The reply to one frame, plus any subscription the call started.
pub struct Handled {
    /// The reply, or `None` when `deferred` produces it.
    pub response: Option<Response>,
    pub subscription: Option<Subscription>,
    /// Sent when it resolves; the connection serves other calls meanwhile.
    pub deferred: Option<Deferred>,
}

impl std::fmt::Debug for Handled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handled")
            .field("response", &self.response)
            .field("subscription", &self.subscription)
            .field("deferred", &self.deferred.is_some())
            .finish()
    }
}

/// Per-connection protocol state. The first call must be `system.handshake`.
#[derive(Debug)]
pub struct Connection {
    state: Arc<DaemonState>,
    handshake_done: bool,
    /// For a paired phone: the only methods it may call.
    allowed: Option<Vec<&'static str>>,
    /// Set while this connection watches resources (`system.resources_subscribe`).
    resources: Option<crate::resources::Watch>,
}

impl Connection {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Connection {
            state,
            handshake_done: false,
            allowed: None,
            resources: None,
        }
    }

    /// A connection limited to `allowed` methods (a paired phone's scopes).
    pub fn restricted(state: Arc<DaemonState>, allowed: Vec<&'static str>) -> Self {
        Connection {
            state,
            handshake_done: false,
            allowed: Some(allowed),
            resources: None,
        }
    }

    /// Whether this connection wants `system.resources_changed` notifications.
    pub fn watches_resources(&self) -> bool {
        self.resources.is_some()
    }

    /// Handles one text frame.
    pub fn handle_text(&mut self, text: &str) -> Handled {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                return reply(Response::failure(
                    None,
                    ErrorObject::new(codes::PARSE_ERROR, format!("invalid JSON: {error}")),
                ));
            }
        };
        // Recover the id, if readable, so even an invalid request gets a matched error.
        let id = value
            .get("id")
            .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok());
        match serde_json::from_value::<Request>(value) {
            Ok(request) => self.handle(request),
            Err(error) => reply(Response::failure(
                id,
                ErrorObject::new(codes::INVALID_REQUEST, format!("invalid request: {error}")),
            )),
        }
    }

    fn handle(&mut self, request: Request) -> Handled {
        let mut subscription = None;
        let mut later = None;
        let outcome = match request.method.as_str() {
            Handshake::NAME => call::<Handshake>(request.params, |params| self.handshake(params)),
            _ if !self.handshake_done => Err(ErrorObject::new(
                codes::HANDSHAKE_REQUIRED,
                "call system.handshake first",
            )),
            method
                if self
                    .allowed
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&method)) =>
            {
                Err(ErrorObject::new(
                    codes::FORBIDDEN,
                    format!("{method} is not allowed for this device"),
                ))
            }
            PhonePair::NAME => call::<PhonePair>(request.params, |_: PhonePairParams| {
                Ok(PhonePairResult {
                    pairing: self.state.devices().new_code(),
                    url: self.state.phone_url(),
                })
            }),
            PhoneDevices::NAME => call::<PhoneDevices>(request.params, |_: PhoneDevicesParams| {
                Ok(PhoneDevicesResult {
                    devices: self.state.devices().list(),
                })
            }),
            PhoneRevoke::NAME => {
                call::<PhoneRevoke>(request.params, |params: PhoneRevokeParams| {
                    let found = self
                        .state
                        .devices()
                        .revoke(&params.id)
                        .map_err(|message| ErrorObject::new(codes::INTERNAL_ERROR, message))?;
                    Ok(PhoneRevokeResult { found })
                })
            }
            SystemResources::NAME => call_later::<SystemResources, _>(
                request.params,
                &mut later,
                |_: SystemResourcesParams| {
                    let state = Arc::clone(&self.state);
                    async move { Ok(crate::resources::snapshot(&state).await) }
                },
            ),
            SystemResourcesSubscribe::NAME => call::<SystemResourcesSubscribe>(
                request.params,
                |params: SystemResourcesSubscribeParams| {
                    let monitor = Arc::clone(self.state.resource_monitor());
                    if !params.on {
                        self.resources = None;
                    } else if self.resources.is_none() {
                        self.resources = Some(monitor.watch(&self.state));
                    }
                    Ok(SystemResourcesSubscribeResult {
                        on: params.on,
                        interval_ms: u32::try_from(monitor.interval().as_millis())
                            .unwrap_or(u32::MAX),
                    })
                },
            ),
            ModelUnload::NAME => call_later::<ModelUnload, _>(
                request.params,
                &mut later,
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
            Shutdown::NAME => call::<Shutdown>(request.params, |_: ShutdownParams| {
                tracing::info!("a client asked imperium-core to stop");
                self.state.request_shutdown();
                Ok(ShutdownResult {
                    pid: std::process::id(),
                })
            }),
            ConfigDescribe::NAME => {
                call::<ConfigDescribe>(request.params, |_: ConfigDescribeParams| {
                    crate::config::describe(&self.state)
                })
            }
            ConfigSet::NAME => call::<ConfigSet>(request.params, |params: ConfigSetParams| {
                crate::config::set(&self.state, params)
            }),
            McpList::NAME => call::<McpList>(request.params, |_: McpListParams| {
                Ok(crate::connectors::list(&self.state))
            }),
            McpCatalog::NAME => call::<McpCatalog>(request.params, |_: McpCatalogParams| {
                Ok(crate::connectors::catalog(&self.state))
            }),
            McpSave::NAME => call::<McpSave>(request.params, |params| {
                crate::connectors::save(&self.state, params)
            }),
            McpInstall::NAME => call::<McpInstall>(request.params, |params| {
                crate::connectors::install(&self.state, params)
            }),
            McpSetEnabled::NAME => call::<McpSetEnabled>(request.params, |params| {
                crate::connectors::set_enabled(&self.state, params)
            }),
            McpRemove::NAME => call::<McpRemove>(request.params, |params| {
                crate::connectors::remove(&self.state, params)
            }),
            BudgetsGet::NAME => call::<BudgetsGet>(request.params, |_: BudgetsGetParams| {
                crate::budgets::get(&self.state)
            }),
            BudgetsSet::NAME => call::<BudgetsSet>(request.params, |params| {
                crate::budgets::set(&self.state, params)
            }),
            BudgetsUsage::NAME => call::<BudgetsUsage>(request.params, |params| {
                crate::budgets::usage(&self.state, params)
            }),
            PermissionsGet::NAME => {
                call::<PermissionsGet>(request.params, |_: PermissionsGetParams| {
                    crate::permissions::get(&self.state)
                })
            }
            PermissionsSet::NAME => call::<PermissionsSet>(request.params, |params| {
                crate::permissions::set(&self.state, params)
            }),
            PermissionsAddRule::NAME => call::<PermissionsAddRule>(request.params, |params| {
                crate::permissions::add_rule(&self.state, params)
            }),
            McpTest::NAME => call_later::<McpTest, _>(request.params, &mut later, |params| {
                crate::connectors::test(Arc::clone(&self.state), params)
            }),
            Ping::NAME => call::<Ping>(request.params, |_: PingParams| {
                Ok(PingResult {
                    uptime_ms: self.state.uptime_ms(),
                })
            }),
            SessionCreate::NAME => call::<SessionCreate>(request.params, |params| {
                let (result, events) = self.create_session(params)?;
                subscription = Some(Subscription {
                    session_id: result.session_id.clone(),
                    events,
                });
                Ok(result)
            }),
            EnvironmentTestRuntime::NAME => {
                call_later::<EnvironmentTestRuntime, _>(request.params, &mut later, |params| {
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
                self.environment_call(method, request.params)
            }
            ModelList::NAME => call::<ModelList>(request.params, |_: ModelListParams| {
                Ok(self.state.model_table())
            }),
            PetList::NAME => call::<PetList>(request.params, |_: PetListParams| {
                Ok(match self.state.pets() {
                    Some(pets) => PetListResult {
                        pets: pets.pets(),
                        selected: Some(pets.selected().to_string()),
                        state: pets.state(),
                    },
                    // Pets are off (or none loaded): still list what is installed.
                    None => PetListResult {
                        pets: crate::pets::discover_installed(&self.state)
                            .0
                            .iter()
                            .map(crate::pets::pet_info)
                            .collect(),
                        selected: None,
                        state: None,
                    },
                })
            }),
            PetStrip::NAME => call::<PetStrip>(request.params, |params: PetStripParams| {
                let png = match self.state.pets() {
                    Some(pets) => pets.strip(params.pet_id.as_deref(), &params.animation),
                    // Without the engine there is no selected pet, so one must be named.
                    None => params.pet_id.as_deref().and_then(|id| {
                        crate::pets::strip_of(
                            &crate::pets::discover_installed(&self.state).0,
                            id,
                            &params.animation,
                        )
                    }),
                }
                .ok_or_else(|| {
                    ErrorObject::new(
                        codes::INVALID_PARAMS,
                        format!("the pet does not draw {}", params.animation),
                    )
                })?;
                Ok(PetStripResult {
                    png_base64: base64::engine::general_purpose::STANDARD.encode(png),
                })
            }),
            PetInteract::NAME => {
                call::<PetInteract>(request.params, |params: PetInteractParams| {
                    if let Some(pets) = self.state.pets() {
                        let animation = match params.kind.as_str() {
                            "hide" => "duck-in",
                            "show" => "pop-out",
                            _ => "petted",
                        };
                        pets.report(
                            "user".into(),
                            imperium_pets::PetEvent::Once(animation.into()),
                        );
                    }
                    Ok(PetInteractResult {})
                })
            }
            ProjectList::NAME => call::<ProjectList>(request.params, |_: ProjectListParams| {
                Ok(ProjectListResult {
                    agents: self
                        .state
                        .project_agents()
                        .list()
                        .into_iter()
                        .map(|(project, session_id)| ProjectAgentRow {
                            project: project.display().to_string(),
                            session_id,
                        })
                        .collect(),
                })
            }),
            ProjectMessage::NAME => {
                call::<ProjectMessage>(request.params, |params: ProjectMessageParams| {
                    Ok(ProjectMessageResult {
                        delivered: self
                            .state
                            .tell_project_agent(std::path::Path::new(&params.project), params.text),
                    })
                })
            }
            SessionControl::NAME => {
                call_later::<SessionControl, _>(request.params, &mut later, |params| {
                    self.control(params)
                })
            }
            ApprovalList::NAME => call::<ApprovalList>(request.params, |_: ApprovalListParams| {
                Ok(ApprovalListResult {
                    approvals: self.state.approvals().list(),
                })
            }),
            ApprovalRespond::NAME => {
                call::<ApprovalRespond>(request.params, |params: ApprovalRespondParams| {
                    Ok(ApprovalRespondResult {
                        found: self.state.approvals().respond(params.id, params.approve),
                    })
                })
            }
            ScheduleList::NAME => call::<ScheduleList>(request.params, |_: ScheduleListParams| {
                Ok(self
                    .state
                    .scheduler()
                    .map(|scheduler| scheduler.list())
                    .unwrap_or(ScheduleListResult {
                        schedules: Vec::new(),
                    }))
            }),
            ScheduleRun::NAME => {
                call::<ScheduleRun>(request.params, |params: ScheduleRunParams| {
                    let scheduler = self.state.scheduler().ok_or_else(|| {
                        ErrorObject::new(codes::INTERNAL_ERROR, "the scheduler is not running")
                    })?;
                    let started = scheduler
                        .run_now(&self.state, &params.id)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                    Ok(ScheduleRunResult { started })
                })
            }
            ModList::NAME => call::<ModList>(request.params, |_: ModListParams| {
                Ok(crate::mods::list(
                    &self.state.mods_dir(),
                    &crate::mods::bundled_roots(),
                ))
            }),
            ConfigChanges::NAME => {
                call::<ConfigChanges>(request.params, |_: ConfigChangesParams| {
                    Ok(ConfigChangesResult {
                        changes: crate::config_changes::list(&self.state),
                    })
                })
            }
            ConfigUndo::NAME => call::<ConfigUndo>(request.params, |params: ConfigUndoParams| {
                crate::config_changes::undo(&self.state, params.id)
                    .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
            }),
            ModelConfig::NAME => call::<ModelConfig>(request.params, |_: ModelConfigParams| {
                Ok(crate::model_config::config(&self.state))
            }),
            ModelSetConfig::NAME => {
                call::<ModelSetConfig>(request.params, |params: ModelSetConfigParams| {
                    crate::model_config::set_config(&self.state, params)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                })
            }
            LocalModelList::NAME => {
                call::<LocalModelList>(request.params, |_: LocalModelListParams| {
                    Ok(self.state.local_models())
                })
            }
            ModelFolderAdd::NAME => {
                call_later::<ModelFolderAdd, _>(request.params, &mut later, |params| {
                    crate::onboarding::add_folder(Arc::clone(&self.state), params)
                })
            }
            ModelFolderRemove::NAME => {
                call_later::<ModelFolderRemove, _>(request.params, &mut later, |params| {
                    crate::onboarding::remove_folder(Arc::clone(&self.state), params)
                })
            }
            ModelUse::NAME => call::<ModelUse>(request.params, |params: ModelUseParams| {
                crate::onboarding::use_model(&self.state, params)
            }),
            ModelAddEndpoint::NAME => {
                call::<ModelAddEndpoint>(request.params, |params: ModelAddEndpointParams| {
                    crate::onboarding::add_endpoint(&self.state, params)
                })
            }
            ModelTest::NAME => call_later::<ModelTest, _>(request.params, &mut later, |params| {
                crate::onboarding::test_model(Arc::clone(&self.state), params)
            }),
            RuntimeStatus::NAME => {
                call_later::<RuntimeStatus, _>(request.params, &mut later, |_| {
                    let state = Arc::clone(&self.state);
                    async move { Ok(crate::onboarding::runtime_status(state).await) }
                })
            }
            RuntimeSetServer::NAME => {
                call_later::<RuntimeSetServer, _>(request.params, &mut later, |params| {
                    crate::onboarding::set_server(Arc::clone(&self.state), params)
                })
            }
            RuntimeInstall::NAME => {
                call_later::<RuntimeInstall, _>(request.params, &mut later, |_| {
                    crate::onboarding::install_runtime(Arc::clone(&self.state))
                })
            }
            RuntimeUpdateExternal::NAME => {
                call_later::<RuntimeUpdateExternal, _>(request.params, &mut later, |_| {
                    crate::llama_runtime::update_external(Arc::clone(&self.state))
                })
            }
            HfFiles::NAME => call_later::<HfFiles, _>(request.params, &mut later, |params| {
                crate::downloads::files(Arc::clone(&self.state), params)
            }),
            ModelDownload::NAME => {
                call_later::<ModelDownload, _>(request.params, &mut later, |params| {
                    crate::downloads::start(Arc::clone(&self.state), params)
                })
            }
            ModelDownloads::NAME => {
                call::<ModelDownloads>(request.params, |_: ModelDownloadsParams| {
                    Ok(ModelDownloadsResult {
                        jobs: self.state.downloads().list(),
                    })
                })
            }
            ModelDownloadCancel::NAME => {
                call::<ModelDownloadCancel>(request.params, |params: ModelDownloadCancelParams| {
                    Ok(ModelDownloadCancelResult {
                        found: self.state.downloads().cancel(&self.state, &params.job_id),
                    })
                })
            }
            SessionList::NAME => call::<SessionList>(request.params, |_: SessionListParams| {
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
            SessionSearch::NAME => call_later::<SessionSearch, _>(
                request.params,
                &mut later,
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
                call::<SessionSubscribe>(request.params, |params: SessionSubscribeParams| {
                    // An archived session's history comes from its journal; nothing follows.
                    if let Some(history) = self.state.archived_history(&params.session_id) {
                        return Ok(SessionSubscribeResult { history });
                    }
                    let (history, events) =
                        self.session(&params.session_id)?.subscribe_with_history();
                    subscription = Some(Subscription {
                        session_id: params.session_id,
                        events,
                    });
                    Ok(SessionSubscribeResult { history })
                })
            }
            SessionSend::NAME => {
                call::<SessionSend>(request.params, |params: SessionSendParams| {
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
                call::<SessionAttach>(request.params, |params: SessionAttachParams| {
                    let session = self.session(&params.session_id)?;
                    crate::attachments::attach(&self.state, &session, params)
                })
            }
            VoiceTranscribe::NAME => {
                call_later::<VoiceTranscribe, _>(request.params, &mut later, |params| {
                    crate::attachments::transcribe(Arc::clone(&self.state), params)
                })
            }
            SessionQueue::NAME => {
                call::<SessionQueue>(request.params, |params: SessionQueueParams| {
                    self.queue(params)
                })
            }
            SessionAgents::NAME => {
                call::<SessionAgents>(request.params, |params: SessionAgentsParams| {
                    Ok(SessionAgentsResult {
                        agents: self.session(&params.session_id)?.agents(),
                    })
                })
            }
            SessionAgentResume::NAME => {
                call::<SessionAgentResume>(request.params, |params: SessionAgentResumeParams| {
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
                request.params,
                &mut later,
                |params: SessionCheckpointsParams| {
                    let state = Arc::clone(&self.state);
                    async move {
                        blocking(move || crate::checkpoints::list(&state, &params.session_id)).await
                    }
                },
            ),
            SessionRewind::NAME => call_later::<SessionRewind, _>(
                request.params,
                &mut later,
                |params: SessionRewindParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::checkpoints::rewind(&state, &params)).await }
                },
            ),
            SessionReview::NAME => call_later::<SessionReview, _>(
                request.params,
                &mut later,
                |params: SessionReviewParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::review(&state, &params)).await }
                },
            ),
            SessionReviewHunk::NAME => call_later::<SessionReviewHunk, _>(
                request.params,
                &mut later,
                |params: SessionReviewHunkParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::act_on_hunk(&state, &params)).await }
                },
            ),
            SessionReviewCommit::NAME => call_later::<SessionReviewCommit, _>(
                request.params,
                &mut later,
                |params: SessionReviewCommitParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::commit(&state, &params)).await }
                },
            ),
            SessionReviewPullRequest::NAME => call_later::<SessionReviewPullRequest, _>(
                request.params,
                &mut later,
                |params: SessionReviewPullRequestParams| {
                    let state = Arc::clone(&self.state);
                    async move { blocking(move || crate::review::pull_request(&state, &params)).await }
                },
            ),
            SessionAgentCancel::NAME => {
                call::<SessionAgentCancel>(request.params, |params: SessionAgentCancelParams| {
                    self.session(&params.session_id)?
                        .cancel_agent(&params.agent_id)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))?;
                    Ok(SessionAgentCancelResult {})
                })
            }
            SessionSuggestionRespond::NAME => {
                call_later::<SessionSuggestionRespond, _>(request.params, &mut later, |params| {
                    crate::suggestions::respond(Arc::clone(&self.state), params)
                })
            }
            SessionDelete::NAME => {
                call::<SessionDelete>(request.params, |params: SessionDeleteParams| {
                    self.state
                        .delete_session(&params.session_id, params.remove_worktrees)
                        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
                })
            }
            SessionAnswer::NAME => {
                call::<SessionAnswer>(request.params, |params: SessionAnswerParams| {
                    self.session(&params.session_id)?
                        .answer(params.request_id, params.answers)
                        .map_err(stopped)?;
                    Ok(Accepted {})
                })
            }
            SessionRespond::NAME => {
                call::<SessionRespond>(request.params, |params: SessionRespondParams| {
                    self.session(&params.session_id)?
                        .respond(params.request_id, params.decision)
                        .map_err(stopped)?;
                    Ok(Accepted {})
                })
            }
            other => Err(ErrorObject::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )),
        };
        let id = request.id;
        if let (Ok(_), Some(later)) = (&outcome, later) {
            return Handled {
                response: None,
                subscription,
                deferred: Some(Box::pin(async move {
                    match later.await {
                        Ok(result) => Response::success(id, result),
                        Err(error) => Response::failure(Some(id), error),
                    }
                })),
            };
        }
        let response = match outcome {
            Ok(result) => Response::success(id, result),
            Err(error) => Response::failure(Some(id), error),
        };
        Handled {
            response: Some(response),
            subscription,
            deferred: None,
        }
    }

    fn handshake(&mut self, params: HandshakeParams) -> Result<HandshakeResult, ErrorObject> {
        if !PROTOCOL_VERSION.is_compatible_with(params.protocol) {
            return Err(incompatible(params.protocol));
        }
        self.handshake_done = true;
        Ok(HandshakeResult {
            daemon: Peer {
                name: "imperium-core".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            protocol: PROTOCOL_VERSION,
        })
    }

    fn create_session(
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
            (None, None) => {
                return Err(ErrorObject::new(
                    codes::INVALID_PARAMS,
                    "give environment_id (or project_dir) for the session",
                ));
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

    /// The `environments.*` methods; every change is announced to all clients.
    fn environment_call(&self, method: &str, params: Option<Value>) -> Result<Value, ErrorObject> {
        let envs = self.state.environments();
        let failed = |error: EnvironmentError| {
            let code = match error {
                EnvironmentError::Save(_) => codes::INTERNAL_ERROR,
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
                        // The agent's tools follow the new runtime from its next start.
                        self.restart_environment_agent(&id);
                    }
                    self.state.sync_environment_agent(&id);
                    Ok(envs.get(&id).unwrap_or(env))
                })
            }
            EnvironmentDelete::NAME => call::<EnvironmentDelete>(params, |p: EnvironmentRef| {
                envs.delete(&p.id).map_err(failed)?;
                self.state.sync_environment_agent(&p.id);
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

    /// Stops an environment's persistent agent so the next sync starts it afresh (resuming its
    /// conversation) with the environment as it is now.
    fn restart_environment_agent(&self, environment_id: &str) {
        if let Some(session) = self.state.project_agents().environment(environment_id) {
            self.state
                .project_agents()
                .forget_environment(environment_id);
            self.state.forget_session(session.id());
        }
    }

    /// `session.control`. Most actions apply at once; switching models and compacting wait for
    /// the session (compaction asks a model for a summary).
    fn control(
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
                    effort: None,
                },
                None => SessionControlResult::default(),
            })
        }
    }

    /// `session.queue`: lists, edits, removes, or steers an agent's queued messages.
    fn queue(&self, params: SessionQueueParams) -> Result<SessionQueueResult, ErrorObject> {
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

    fn session(&self, id: &str) -> Result<Arc<Session>, ErrorObject> {
        self.state
            .session(id)
            .ok_or_else(|| ErrorObject::new(codes::SESSION_NOT_FOUND, format!("no session {id:?}")))
    }
}

fn reply(response: Response) -> Handled {
    Handled {
        response: Some(response),
        subscription: None,
        deferred: None,
    }
}

fn stopped(error: imperium_engine::SessionClosed) -> ErrorObject {
    ErrorObject::new(codes::INTERNAL_ERROR, error.to_string())
}

fn queue_error(error: QueueError) -> ErrorObject {
    let code = match error {
        QueueError::Closed => codes::INTERNAL_ERROR,
        QueueError::UnknownAgent(_)
        | QueueError::UnknownMessage(_)
        | QueueError::Empty
        | QueueError::Steering(_) => codes::INVALID_PARAMS,
    };
    ErrorObject::new(code, error.to_string())
}

fn archive_error(error: crate::state::ArchiveError) -> ErrorObject {
    use crate::state::ArchiveError;
    let code = match &error {
        ArchiveError::NotFound(_) => codes::SESSION_NOT_FOUND,
        ArchiveError::Persistent(_) => codes::INVALID_PARAMS,
        ArchiveError::Record(_) | ArchiveError::Resume(_) => codes::INTERNAL_ERROR,
    };
    ErrorObject::new(code, error.to_string())
}

fn switch_error(error: SwitchError) -> ErrorObject {
    let code = match &error {
        SwitchError::Busy => codes::SESSION_BUSY,
        SwitchError::TooLarge { .. } | SwitchError::NoFit { .. } => codes::CONTEXT_TOO_LARGE,
        SwitchError::UnknownWindow(_) | SwitchError::NoModel => codes::INVALID_PARAMS,
        SwitchError::Unavailable(_) | SwitchError::Summary(_) => codes::MODEL_UNAVAILABLE,
        SwitchError::Closed => codes::INTERNAL_ERROR,
    };
    ErrorObject::new(code, error.to_string())
}

fn incompatible(client: ProtocolVersion) -> ErrorObject {
    ErrorObject {
        data: Some(json!({ "daemon": PROTOCOL_VERSION })),
        ..ErrorObject::new(
            codes::INCOMPATIBLE_PROTOCOL,
            format!(
                "client protocol {client} is incompatible with daemon protocol {PROTOCOL_VERSION}"
            ),
        )
    }
}

/// Decodes a method's params (absent params mean `{}`), runs it, and encodes its result.
fn call<M: Method>(
    params: Option<Value>,
    run: impl FnOnce(M::Params) -> Result<M::Result, ErrorObject>,
) -> Result<Value, ErrorObject> {
    run(decode::<M>(params)?).map(|result| to_value(&result))
}

/// Runs blocking work (git, file reads) on the blocking pool; its error message becomes an
/// invalid-params error.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, ErrorObject> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?
        .map_err(|message| ErrorObject::new(codes::INVALID_PARAMS, message))
}

/// Like [`call`] for a method whose result takes a while: decodes the params now and leaves
/// the work in `later`, for the connection to reply when it is done.
fn call_later<M: Method, F>(
    params: Option<Value>,
    later: &mut Option<Later>,
    run: impl FnOnce(M::Params) -> F,
) -> Result<Value, ErrorObject>
where
    F: Future<Output = Result<M::Result, ErrorObject>> + Send + 'static,
    M::Result: Send,
{
    let work = run(decode::<M>(params)?);
    *later = Some(Box::pin(async move {
        work.await.map(|result| to_value(&result))
    }));
    Ok(Value::Null)
}

fn decode<M: Method>(params: Option<Value>) -> Result<M::Params, ErrorObject> {
    serde_json::from_value(params.unwrap_or_else(|| json!({}))).map_err(|error| {
        ErrorObject::new(
            codes::INVALID_PARAMS,
            format!("invalid params for {}: {error}", M::NAME),
        )
    })
}

fn to_value(result: &impl Serialize) -> Value {
    serde_json::to_value(result).expect("protocol result types serialise to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_config::{Config, Layer, Paths};
    use imperium_protocol::jsonrpc::Outcome;

    fn connection() -> Connection {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        // Environments are saved in the data folder, so each connection gets its own.
        let home: &'static tempfile::TempDir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        Connection::new(DaemonState::new(Paths::under(home.path()), settings, None))
    }

    fn result_of(handled: &Handled) -> Value {
        match &handled
            .response
            .as_ref()
            .expect("an immediate reply")
            .outcome
        {
            Outcome::Result(result) => result.clone(),
            Outcome::Error(error) => panic!("expected a result, got {error:?}"),
        }
    }

    fn request(id: i64, method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
    }

    fn handshake_params(major: u32) -> Value {
        json!({
            "client": {"name": "test", "version": "0"},
            "protocol": {"major": major, "minor": 0}
        })
    }

    fn error_code(handled: &Handled) -> Option<i32> {
        let response = handled.response.as_ref().expect("an immediate reply");
        match &response.outcome {
            Outcome::Error(error) => Some(error.code),
            Outcome::Result(_) => None,
        }
    }

    fn handshaken() -> Connection {
        let mut connection = connection();
        let handled = connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        assert_eq!(error_code(&handled), None);
        connection
    }

    #[test]
    fn methods_other_than_handshake_require_a_handshake_first() {
        let mut connection = connection();
        let handled = connection.handle_text(&request(1, Ping::NAME, json!({})));
        assert_eq!(error_code(&handled), Some(codes::HANDSHAKE_REQUIRED));
        assert_eq!(
            handled.response.as_ref().unwrap().id,
            Some(RequestId::Number(1))
        );
    }

    #[test]
    fn ping_succeeds_after_a_compatible_handshake() {
        let mut connection = handshaken();
        let ping = connection.handle_text(&request(2, Ping::NAME, Value::Null));
        assert_eq!(error_code(&ping), None, "{:?}", ping.response);
    }

    #[tokio::test]
    async fn shutdown_replies_then_signals_the_daemon() {
        let mut connection = handshaken();
        let state = connection.state.clone();
        let stopped = connection.handle_text(&request(2, Shutdown::NAME, Value::Null));
        assert_eq!(error_code(&stopped), None, "{:?}", stopped.response);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.shutdown_requested(),
        )
        .await
        .expect("the daemon is asked to stop");
    }

    #[test]
    fn an_incompatible_major_version_is_refused() {
        let mut connection = connection();
        let handled = connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major + 1),
        ));
        assert_eq!(error_code(&handled), Some(codes::INCOMPATIBLE_PROTOCOL));
        let ping = connection.handle_text(&request(2, Ping::NAME, json!({})));
        assert_eq!(error_code(&ping), Some(codes::HANDSHAKE_REQUIRED));
    }

    #[test]
    fn malformed_input_gets_the_matching_error() {
        let mut connection = connection();
        assert_eq!(
            error_code(&connection.handle_text("{not json")),
            Some(codes::PARSE_ERROR)
        );

        let invalid = connection.handle_text(r#"{"jsonrpc":"2.0","id":5}"#);
        assert_eq!(error_code(&invalid), Some(codes::INVALID_REQUEST));
        assert_eq!(
            invalid.response.as_ref().unwrap().id,
            Some(RequestId::Number(5))
        );

        let bad_params = connection.handle_text(&request(3, Handshake::NAME, json!({"x": 1})));
        assert_eq!(error_code(&bad_params), Some(codes::INVALID_PARAMS));
    }

    #[test]
    fn unknown_methods_and_sessions_are_reported() {
        let mut connection = handshaken();
        let unknown = connection.handle_text(&request(2, "system.nope", json!({})));
        assert_eq!(error_code(&unknown), Some(codes::METHOD_NOT_FOUND));

        let no_session = connection.handle_text(&request(
            3,
            SessionSend::NAME,
            json!({"session_id": "missing", "text": "hi"}),
        ));
        assert_eq!(error_code(&no_session), Some(codes::SESSION_NOT_FOUND));
    }

    #[test]
    fn the_model_table_lists_rows_with_prices() {
        let config: toml::Table = r#"
            [endpoints.cloud]
            base_url = "https://example.com/v1"
            pricing."big" = { input_usd_per_mtok = 2.0, output_usd_per_mtok = 8.0 }

            [[models.catalog]]
            id = "big"
            spec = "cloud:big"
            location = "cloud"
            tier = "strong"

            [[models.catalog]]
            id = "laptop"
            spec = "nowhere:small"
            location = "local"
            tier = "basic"
            tags = ["code"]
        "#
        .parse()
        .unwrap();
        let settings = Config::from_layers([
            Layer::defaults(),
            Layer::new(imperium_config::Source::User("u".into()), config),
        ])
        .settings()
        .unwrap();
        let mut connection = Connection::new(DaemonState::new(
            Paths::under(std::path::Path::new("unused")),
            settings,
            None,
        ));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        let handled = connection.handle_text(&request(2, ModelList::NAME, json!({})));
        let Some(Outcome::Result(result)) = handled.response.map(|response| response.outcome)
        else {
            panic!("models.list failed");
        };
        assert_eq!(result["prefer"], "cheapest");
        assert_eq!(result["models"][0]["input_usd_per_mtok"], 2.0);
        assert_eq!(result["models"][0]["endpoint_configured"], true);
        assert_eq!(result["models"][1]["location"], "local");
        assert_eq!(result["models"][1]["endpoint_configured"], false);
    }

    fn result(handled: Handled) -> Value {
        match handled.response.map(|response| response.outcome) {
            Some(Outcome::Result(result)) => result,
            other => panic!("expected a result, got {other:?}"),
        }
    }

    #[test]
    fn settings_are_described_and_written_to_the_user_file() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let mut connection = Connection::new(DaemonState::new(paths.clone(), settings, None));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));

        let described =
            result(connection.handle_text(&request(2, ConfigDescribe::NAME, json!({}))));
        let entries = described["entries"].as_array().unwrap();
        let fidget = entries
            .iter()
            .find(|entry| entry["key"] == "pets.fidget")
            .unwrap();
        assert_eq!(fidget["section"], "pets");
        assert_eq!(fidget["kind"], "bool");
        assert_eq!(fidget["default"], true);
        assert_eq!(fidget["restart"], true);

        let set = result(connection.handle_text(&request(
            3,
            ConfigSet::NAME,
            json!({"key": "pets.fidget", "value": false}),
        )));
        assert_eq!(set["entry"]["value"], false);
        assert_eq!(set["entry"]["source"], "user");
        assert_eq!(set["restart"], true);
        assert_eq!(set["overridden"], false);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.contains("fidget = false"), "{text}");

        let refused = connection.handle_text(&request(
            4,
            ConfigSet::NAME,
            json!({"key": "pets.fidget", "value": "sometimes"}),
        ));
        assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
        let unknown = connection.handle_text(&request(
            5,
            ConfigSet::NAME,
            json!({"key": "endpoints.x.base_url", "value": "u"}),
        ));
        assert_eq!(error_code(&unknown), Some(codes::INVALID_PARAMS));
    }

    fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[test]
    fn pets_are_listed_and_stripped_without_the_pet_engine() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        let bundled =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pets/pixel-clawd");
        copy_dir(
            &bundled,
            &paths.config_dir().join("pets").join("pixel-clawd"),
        );
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        // The engine is never started here, as when `pets.enabled` is false.
        let mut connection = Connection::new(DaemonState::new(paths, settings, None));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));

        let listed = result(connection.handle_text(&request(2, PetList::NAME, json!({}))));
        assert!(listed["selected"].is_null());
        assert!(listed["state"].is_null());
        let pet = listed["pets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pet| pet["id"] == "pixel-clawd")
            .expect("the installed pet is listed");
        assert!(pet["drawn"].as_array().unwrap().iter().any(|a| a == "idle"));
        assert!(pet["pet_json"].as_str().unwrap().contains("pixel-clawd"));

        let strip = result(connection.handle_text(&request(
            3,
            PetStrip::NAME,
            json!({"pet_id": "pixel-clawd", "animation": "idle"}),
        )));
        assert!(!strip["png_base64"].as_str().unwrap().is_empty());
        // Without the engine there is no selected pet; unknown pets and animations are refused,
        // including names that try to leave the strips folder.
        for params in [
            json!({"animation": "idle"}),
            json!({"pet_id": "nobody", "animation": "idle"}),
            json!({"pet_id": "pixel-clawd", "animation": "../pet"}),
            json!({"pet_id": "../pixel-clawd", "animation": "idle"}),
        ] {
            let refused = connection.handle_text(&request(4, PetStrip::NAME, params));
            assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
        }
    }

    fn connectors_connection() -> (
        tempfile::TempDir,
        Paths,
        Arc<imperium_config::MemorySecrets>,
        Connection,
    ) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let secrets = Arc::new(imperium_config::MemorySecrets::new());
        let mut connection = Connection::new(DaemonState::with_secrets(
            paths.clone(),
            settings,
            None,
            secrets.clone(),
        ));
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        (home, paths, secrets, connection)
    }

    #[test]
    fn connectors_are_installed_from_the_catalogue_with_secrets_kept_out_of_the_config() {
        use imperium_config::SecretStore;
        let (_home, paths, secrets, mut connection) = connectors_connection();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), "# mine\n[daemon]\nport = 7000\n").unwrap();

        let catalog = result(connection.handle_text(&request(2, McpCatalog::NAME, json!({}))));
        let slack = catalog["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == "slack")
            .unwrap();
        assert_eq!(slack["fields"][0]["kind"], "secret");

        let installed = result(connection.handle_text(&request(
            3,
            McpInstall::NAME,
            json!({"catalog_id": "slack", "values": {"bot_token": "xoxb-secret", "team_id": "T1"}}),
        )));
        let server = &installed["server"];
        assert_eq!(server["name"], "slack");
        assert_eq!(server["config"]["catalog"], "slack");
        assert_eq!(
            server["config"]["secret_env"]["SLACK_BOT_TOKEN"],
            "mcp.slack.bot-token"
        );
        assert_eq!(server["status"]["state"], "untested");
        assert_eq!(server["status"]["missing_secrets"], json!([]));
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(
            text.starts_with("# mine\n[daemon]\nport = 7000\n"),
            "{text}"
        );
        assert!(
            text.contains("SLACK_BOT_TOKEN = { secret = \"mcp.slack.bot-token\" }"),
            "{text}"
        );
        assert!(!text.contains("xoxb-secret"), "{text}");
        let name: imperium_config::SecretName = "mcp.slack.bot-token".parse().unwrap();
        assert_eq!(secrets.load(&name).unwrap().as_deref(), Some("xoxb-secret"));

        // A second install under the same name is refused; a missing field too.
        let again = connection.handle_text(&request(
            4,
            McpInstall::NAME,
            json!({"catalog_id": "slack", "values": {"bot_token": "x", "team_id": "T"}}),
        ));
        assert_eq!(error_code(&again), Some(codes::INVALID_PARAMS));
        let missing = connection.handle_text(&request(
            5,
            McpInstall::NAME,
            json!({"catalog_id": "slack", "name": "slack2", "values": {}}),
        ));
        assert_eq!(error_code(&missing), Some(codes::INVALID_PARAMS));

        // A custom server, then edits: disable, update, and the list reflects each.
        let saved = result(connection.handle_text(&request(
            6,
            McpSave::NAME,
            json!({
                "name": "remote",
                "create": true,
                "config": {"url": "https://example.com/mcp", "bearer_secret": "mcp.remote.token"},
                "secrets": {"mcp.remote.token": "tok"}
            }),
        )));
        assert_eq!(
            saved["server"]["config"]["bearer_secret"],
            "mcp.remote.token"
        );
        let disabled = result(connection.handle_text(&request(
            7,
            McpSetEnabled::NAME,
            json!({"name": "remote", "enabled": false}),
        )));
        assert_eq!(disabled["server"]["status"]["state"], "disabled");
        let listed = result(connection.handle_text(&request(8, McpList::NAME, json!({}))));
        let names: Vec<&str> = listed["servers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|server| server["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["remote", "slack"]);
        assert_eq!(listed["servers"][0]["config"]["enabled"], false);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(text.contains("enabled = false"), "{text}");
        assert!(!text.contains("tok\""), "{text}");

        // Removing forgets the server and, when asked, its secrets.
        let removed = result(connection.handle_text(&request(
            9,
            McpRemove::NAME,
            json!({"name": "slack", "forget_secrets": true}),
        )));
        assert_eq!(removed["removed"], true);
        assert_eq!(secrets.load(&name).unwrap(), None);
        let text = std::fs::read_to_string(paths.user_config_file()).unwrap();
        assert!(!text.contains("slack"), "{text}");
        assert!(text.starts_with("# mine\n"), "{text}");
        let again =
            result(connection.handle_text(&request(10, McpRemove::NAME, json!({"name": "slack"}))));
        assert_eq!(again["removed"], false);
    }

    #[tokio::test]
    async fn testing_a_connector_records_its_status() {
        let (_home, _paths, _secrets, mut connection) = connectors_connection();
        result(connection.handle_text(&request(
            2,
            McpSave::NAME,
            json!({"name": "broken", "config": {"command": "definitely-not-an-mcp-server-xyz"}}),
        )));
        let handled = connection.handle_text(&request(3, McpTest::NAME, json!({"name": "broken"})));
        let response = handled.deferred.expect("a deferred reply").await;
        let Outcome::Result(status) = response.outcome else {
            panic!("mcp.test failed: {response:?}");
        };
        assert_eq!(status["state"], "error");
        assert!(
            status["error"]
                .as_str()
                .unwrap()
                .contains("could not start"),
            "{status}"
        );
        assert!(status["checked_at"].is_number());

        let listed = result(connection.handle_text(&request(4, McpList::NAME, json!({}))));
        assert_eq!(listed["servers"][0]["status"]["state"], "error");

        let unknown = connection.handle_text(&request(5, McpTest::NAME, json!({"name": "nope"})));
        let response = unknown.deferred.expect("a deferred reply").await;
        assert!(
            matches!(response.outcome, Outcome::Error(ref error) if error.code == codes::INVALID_PARAMS)
        );
    }

    #[tokio::test]
    async fn attachments_are_staged_inside_the_session_and_checked_again_on_send() {
        let mut connection = handshaken();
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("notes.md"), "# Plan").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "key").unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": project.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();
        let store = connection
            .state
            .data_dir()
            .join("attachments")
            .join(&id)
            .canonicalize()
            .unwrap();

        // Inside the project: referenced where it is.
        let inside = result_of(&connection.handle_text(&request(
            3,
            SessionAttach::NAME,
            json!({"session_id": id, "path": project.path().join("notes.md")}),
        )));
        assert_eq!(inside["attachment"]["kind"], "text");
        assert_eq!(inside["attachment"]["copied"], false);
        // Pasted bytes and outside files are copied into the session's attachments folder.
        let pasted = result_of(&connection.handle_text(&request(
            4,
            SessionAttach::NAME,
            json!({"session_id": id, "name": "shot.png",
                   "data": base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nx")}),
        )));
        assert_eq!(pasted["attachment"]["kind"], "image");
        assert_eq!(pasted["attachment"]["copied"], true);
        assert!(
            pasted["image_data"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        let copied = result_of(&connection.handle_text(&request(
            5,
            SessionAttach::NAME,
            json!({"session_id": id, "path": outside.path().join("secret.txt")}),
        )));
        assert_eq!(copied["attachment"]["copied"], true);
        let copy = std::path::Path::new(copied["attachment"]["path"].as_str().unwrap());
        assert!(copy.canonicalize().unwrap().starts_with(&store));
        // Neither or both sources, and oversized files, are refused.
        let neither =
            connection.handle_text(&request(6, SessionAttach::NAME, json!({"session_id": id})));
        assert_eq!(error_code(&neither), Some(codes::INVALID_PARAMS));

        // A sent attachment pointing outside the session is refused; the staged ones are sent.
        let mut smuggled = inside["attachment"].clone();
        smuggled["path"] = json!(outside.path().join("secret.txt"));
        let refused = connection.handle_text(&request(
            7,
            SessionSend::NAME,
            json!({"session_id": id, "text": "", "attachments": [smuggled]}),
        ));
        assert_eq!(error_code(&refused), Some(codes::INVALID_PARAMS));
        let sent = connection.handle_text(&request(
            8,
            SessionSend::NAME,
            json!({"session_id": id, "text": "", "attachments": [
                inside["attachment"], pasted["attachment"], copied["attachment"]
            ]}),
        ));
        assert_eq!(error_code(&sent), None, "{:?}", sent.response);

        // Deleting the session deletes its attachments folder.
        let deleted =
            connection.handle_text(&request(9, SessionDelete::NAME, json!({"session_id": id})));
        assert_eq!(error_code(&deleted), None, "{:?}", deleted.response);
        assert!(!store.exists());
    }

    #[tokio::test]
    async fn sessions_are_archived_and_unarchived_over_rpc() {
        let mut connection = handshaken();
        let mut notices = connection.state.announcements();
        let dir = tempfile::tempdir().unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": dir.path()}),
        )));
        let id = created["session_id"].as_str().unwrap().to_string();
        let control = |connection: &mut Connection, n: i64, action: &str| {
            connection
                .handle_text(&request(
                    n,
                    SessionControl::NAME,
                    json!({"session_id": id, "action": action}),
                ))
                .deferred
                .expect("a deferred reply")
        };

        let archived = control(&mut connection, 3, "archive").await;
        assert!(
            matches!(archived.outcome, Outcome::Result(_)),
            "{archived:?}"
        );
        let listed = result_of(&connection.handle_text(&request(4, SessionList::NAME, json!({}))));
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(listed["sessions"][0]["archived"], true);
        assert!(listed["sessions"][0]["archived_at"].is_u64());
        // Every client hears of it.
        let mut updated = false;
        while let Ok(notice) = notices.try_recv() {
            if notice.method == imperium_protocol::methods::SESSION_UPDATED
                && notice.params["archived"] == true
            {
                updated = true;
            }
        }
        assert!(updated, "no session.updated for the archive");
        // Its history is still readable, with nothing live to follow.
        let subscribed = connection.handle_text(&request(
            5,
            SessionSubscribe::NAME,
            json!({"session_id": id}),
        ));
        assert!(subscribed.subscription.is_none());
        assert!(result_of(&subscribed)["history"].is_array());
        // It no longer takes messages.
        let send = connection.handle_text(&request(
            6,
            SessionSend::NAME,
            json!({"session_id": id, "text": "hi"}),
        ));
        assert_eq!(error_code(&send), Some(codes::SESSION_NOT_FOUND));

        let unarchived = control(&mut connection, 7, "unarchive").await;
        assert!(
            matches!(unarchived.outcome, Outcome::Result(_)),
            "{unarchived:?}"
        );
        let listed = result_of(&connection.handle_text(&request(8, SessionList::NAME, json!({}))));
        assert_eq!(listed["sessions"][0]["archived"], false);
        assert!(connection.state.session(&id).is_some());

        let missing = connection
            .handle_text(&request(
                9,
                SessionControl::NAME,
                json!({"session_id": "nope", "action": "archive"}),
            ))
            .deferred
            .expect("a deferred reply")
            .await;
        assert!(
            matches!(missing.outcome, Outcome::Error(ref error) if error.code == codes::SESSION_NOT_FOUND)
        );
    }

    #[tokio::test]
    async fn sessions_start_without_a_model_and_wait_for_one() {
        let mut connection = handshaken();
        let missing_folder = connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": "definitely/not/a/folder"}),
        ));
        assert_eq!(error_code(&missing_folder), Some(codes::INVALID_PARAMS));

        // No model given and no models.default: the session is created anyway, without one.
        let dir = tempfile::tempdir().unwrap();
        let created = connection.handle_text(&request(
            3,
            SessionCreate::NAME,
            json!({"project_dir": dir.path()}),
        ));
        assert!(created.subscription.is_some());
        let created = result_of(&created);
        assert_eq!(created["model"], Value::Null);
        let id = created["session_id"].as_str().unwrap().to_string();
        let listed = result_of(&connection.handle_text(&request(4, SessionList::NAME, json!({}))));
        assert_eq!(listed["sessions"][0]["model"], Value::Null);
        assert_eq!(listed["sessions"][0]["status"], "idle");

        // A message waits for a model instead of failing.
        let session = connection.state.session(&id).unwrap();
        session.send("hello".into()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while session.status().0 != imperium_protocol::session::SessionStatus::NeedsModel {
            assert!(
                std::time::Instant::now() < deadline,
                "the message never waited"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(session.queue(None).unwrap().len(), 1);

        // A model that cannot be resolved is refused, and the session keeps waiting.
        let refused = connection.handle_text(&request(
            5,
            SessionControl::NAME,
            json!({"session_id": id, "action": "set_model", "model": "nowhere:m"}),
        ));
        let refused = refused.deferred.expect("a deferred reply").await;
        assert!(
            matches!(refused.outcome, Outcome::Error(ref error) if error.code == codes::MODEL_UNAVAILABLE)
        );
        assert_eq!(session.model(), None);

        let neither = connection.handle_text(&request(6, SessionCreate::NAME, json!({})));
        assert_eq!(error_code(&neither), Some(codes::INVALID_PARAMS));
    }

    #[test]
    fn environments_are_managed_over_rpc() {
        let mut connection = handshaken();
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let created = result_of(&connection.handle_text(&request(
            2,
            "environments.create",
            json!({"name": "Work", "folders": [one.path()]}),
        )));
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["folders"].as_array().unwrap().len(), 1);
        // A second environment is just as easy (one per folder, or several folders each).
        result_of(&connection.handle_text(&request(
            3,
            "environments.create",
            json!({"name": "Other", "folders": [two.path()]}),
        )));
        let added = result_of(&connection.handle_text(&request(
            4,
            "environments.add_folder",
            json!({"id": id, "folder": two.path()}),
        )));
        assert_eq!(added["folders"].as_array().unwrap().len(), 2);
        let renamed = result_of(&connection.handle_text(&request(
            5,
            "environments.update",
            json!({"id": id, "name": "Day job", "icon": "code"}),
        )));
        assert_eq!(renamed["name"], "Day job");
        assert_eq!(renamed["icon"], "code");
        let removed = result_of(&connection.handle_text(&request(
            6,
            "environments.remove_folder",
            json!({"id": id, "folder": two.path()}),
        )));
        assert_eq!(removed["folders"].as_array().unwrap().len(), 1);
        let listed =
            result_of(&connection.handle_text(&request(7, "environments.list", json!({}))));
        assert_eq!(listed["environments"].as_array().unwrap().len(), 2);

        let missing = connection.handle_text(&request(
            8,
            "environments.add_folder",
            json!({"id": id, "folder": one.path().join("missing")}),
        ));
        assert_eq!(error_code(&missing), Some(codes::INVALID_PARAMS));
        let unknown = connection.handle_text(&request(
            9,
            SessionCreate::NAME,
            json!({"environment_id": "nope"}),
        ));
        assert_eq!(error_code(&unknown), Some(codes::INVALID_PARAMS));
        let deleted =
            connection.handle_text(&request(10, "environments.delete", json!({"id": id})));
        assert_eq!(error_code(&deleted), None);
        let listed =
            result_of(&connection.handle_text(&request(11, "environments.list", json!({}))));
        assert_eq!(listed["environments"].as_array().unwrap().len(), 1);
        let bogus = connection.handle_text(&request(12, "environments.nothing", json!({})));
        assert_eq!(error_code(&bogus), Some(codes::METHOD_NOT_FOUND));
    }
}
