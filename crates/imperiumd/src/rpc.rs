//! Request handling for one client connection.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine;
use imperium_core::Session;
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
    ScheduleListResult, ScheduleRun, ScheduleRunParams, ScheduleRunResult, SessionCreate,
    SessionCreateParams, SessionCreateResult, SessionList, SessionListParams, SessionListResult,
    SessionRespond, SessionRespondParams, SessionSend, SessionSendParams, SessionSubscribe,
    SessionSubscribeParams, SessionSubscribeResult, Shutdown, ShutdownParams, ShutdownResult,
};
use imperium_protocol::session::AgentEvent;
use imperium_protocol::{PROTOCOL_VERSION, ProtocolVersion};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::state::{CreateError, DaemonState};
use imperium_protocol::methods::{
    HfFiles, ModelDownload, ModelDownloadCancel, ModelDownloadCancelParams,
    ModelDownloadCancelResult, ModelDownloads, ModelDownloadsParams, ModelDownloadsResult,
};
use imperium_protocol::methods::{SessionControl, SessionControlParams, SessionControlResult};

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
}

impl Connection {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Connection {
            state,
            handshake_done: false,
            allowed: None,
        }
    }

    /// A connection limited to `allowed` methods (a paired phone's scopes).
    pub fn restricted(state: Arc<DaemonState>, allowed: Vec<&'static str>) -> Self {
        Connection {
            state,
            handshake_done: false,
            allowed: Some(allowed),
        }
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
            Shutdown::NAME => call::<Shutdown>(request.params, |_: ShutdownParams| {
                tracing::info!("a client asked imperiumd to stop");
                self.state.request_shutdown();
                Ok(ShutdownResult {
                    pid: std::process::id(),
                })
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
                    None => PetListResult {
                        pets: Vec::new(),
                        selected: None,
                        state: None,
                    },
                })
            }),
            PetStrip::NAME => call::<PetStrip>(request.params, |params: PetStripParams| {
                let png = self
                    .state
                    .pets()
                    .and_then(|pets| pets.strip(&params.animation))
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
                call::<SessionControl>(request.params, |params: SessionControlParams| {
                    let session = self.state.session(&params.session_id).ok_or_else(|| {
                        ErrorObject::new(codes::SESSION_NOT_FOUND, "no such session")
                    })?;
                    match params.action.as_str() {
                        "set_resume_policy" => session.set_resume_policy(params.resume_policy),
                        "resume_now" => session.resume_now(),
                        "watch" => {
                            let (Some(repo), Some(number)) = (params.repo, params.pull_request)
                            else {
                                return Err(ErrorObject::new(
                                    codes::INVALID_PARAMS,
                                    "watch needs repo and pull_request",
                                ));
                            };
                            self.state.watch(&session, Some((repo, number)));
                        }
                        "unwatch" => self.state.watch(&session, None),
                        other => {
                            return Err(ErrorObject::new(
                                codes::INVALID_PARAMS,
                                format!("unknown action {other:?}"),
                            ));
                        }
                    }
                    Ok(SessionControlResult {})
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
                Ok(crate::mods::list(&self.state.mods_dir()))
            }),
            LocalModelList::NAME => {
                call::<LocalModelList>(request.params, |_: LocalModelListParams| {
                    Ok(self.state.local_models())
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
                Ok(SessionListResult {
                    sessions: self
                        .state
                        .sessions()
                        .iter()
                        .map(|session| crate::state::summary(session))
                        .collect(),
                })
            }),
            SessionSubscribe::NAME => {
                call::<SessionSubscribe>(request.params, |params: SessionSubscribeParams| {
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
                    self.session(&params.session_id)?
                        .send(params.text)
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
                name: "imperiumd".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            protocol: PROTOCOL_VERSION,
        })
    }

    fn create_session(
        &self,
        params: SessionCreateParams,
    ) -> Result<(SessionCreateResult, broadcast::Receiver<AgentEvent>), ErrorObject> {
        let (session, events) = self
            .state
            .create_session(&params.project_dir, params.model.as_deref())
            .map_err(|error| {
                let code = match error {
                    CreateError::Project(..) => codes::INVALID_PARAMS,
                    CreateError::Model(_) => codes::MODEL_UNAVAILABLE,
                    CreateError::Start(_) => codes::INTERNAL_ERROR,
                };
                ErrorObject::new(code, error.to_string())
            })?;
        Ok((
            SessionCreateResult {
                session_id: session.id().to_string(),
                model: session.model().to_string(),
                isolation: session.isolation().to_string(),
            },
            events,
        ))
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

fn stopped(error: imperium_core::SessionClosed) -> ErrorObject {
    ErrorObject::new(codes::INTERNAL_ERROR, error.to_string())
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
        Connection::new(DaemonState::new(
            Paths::under(std::path::Path::new("unused")),
            settings,
            None,
        ))
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

    #[test]
    fn session_creation_explains_missing_folders_and_models() {
        let mut connection = handshaken();
        let missing_folder = connection.handle_text(&request(
            2,
            SessionCreate::NAME,
            json!({"project_dir": "definitely/not/a/folder"}),
        ));
        assert_eq!(error_code(&missing_folder), Some(codes::INVALID_PARAMS));

        let dir = tempfile::tempdir().unwrap();
        let no_model = connection.handle_text(&request(
            3,
            SessionCreate::NAME,
            json!({"project_dir": dir.path()}),
        ));
        assert_eq!(error_code(&no_model), Some(codes::MODEL_UNAVAILABLE));
        assert!(no_model.subscription.is_none());
    }
}
