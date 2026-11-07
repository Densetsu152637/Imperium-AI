//! Request handling for one client connection.

use std::sync::Arc;

use imperium_core::Session;
use imperium_protocol::jsonrpc::{ErrorObject, Request, RequestId, Response, codes};
use imperium_protocol::methods::{
    Accepted, Handshake, HandshakeParams, HandshakeResult, LocalModelList, LocalModelListParams,
    Method, ModList, ModListParams, ModelList, ModelListParams, Peer, Ping, PingParams, PingResult,
    ScheduleList, ScheduleListParams, ScheduleListResult, ScheduleRun, ScheduleRunParams,
    ScheduleRunResult, SessionCreate, SessionCreateParams, SessionCreateResult, SessionList,
    SessionListParams, SessionListResult, SessionRespond, SessionRespondParams, SessionSend,
    SessionSendParams, SessionSubscribe, SessionSubscribeParams, SessionSubscribeResult,
    SessionSummary,
};
use imperium_protocol::session::AgentEvent;
use imperium_protocol::{PROTOCOL_VERSION, ProtocolVersion};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::state::{CreateError, DaemonState};

/// A session's events, to be forwarded to this connection as notifications.
#[derive(Debug)]
pub struct Subscription {
    pub session_id: String,
    pub events: broadcast::Receiver<AgentEvent>,
}

/// The reply to one frame, plus any subscription the call started.
#[derive(Debug)]
pub struct Handled {
    pub response: Response,
    pub subscription: Option<Subscription>,
}

/// Per-connection protocol state. The first call must be `system.handshake`.
#[derive(Debug)]
pub struct Connection {
    state: Arc<DaemonState>,
    handshake_done: bool,
}

impl Connection {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Connection {
            state,
            handshake_done: false,
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
        let outcome = match request.method.as_str() {
            Handshake::NAME => call::<Handshake>(request.params, |params| self.handshake(params)),
            _ if !self.handshake_done => Err(ErrorObject::new(
                codes::HANDSHAKE_REQUIRED,
                "call system.handshake first",
            )),
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
            SessionList::NAME => call::<SessionList>(request.params, |_: SessionListParams| {
                Ok(SessionListResult {
                    sessions: self
                        .state
                        .sessions()
                        .iter()
                        .map(|session| SessionSummary {
                            session_id: session.id().to_string(),
                            project_dir: session.project_dir().to_string(),
                            model: session.model().to_string(),
                        })
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
        let response = match outcome {
            Ok(result) => Response::success(request.id, result),
            Err(error) => Response::failure(Some(request.id), error),
        };
        Handled {
            response,
            subscription,
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
        response,
        subscription: None,
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
    let params = serde_json::from_value(params.unwrap_or_else(|| json!({}))).map_err(|error| {
        ErrorObject::new(
            codes::INVALID_PARAMS,
            format!("invalid params for {}: {error}", M::NAME),
        )
    })?;
    run(params).map(|result| to_value(&result))
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
        match &handled.response.outcome {
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
        assert_eq!(handled.response.id, Some(RequestId::Number(1)));
    }

    #[test]
    fn ping_succeeds_after_a_compatible_handshake() {
        let mut connection = handshaken();
        let ping = connection.handle_text(&request(2, Ping::NAME, Value::Null));
        assert_eq!(error_code(&ping), None, "{:?}", ping.response);
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
        assert_eq!(invalid.response.id, Some(RequestId::Number(5)));

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
        let Outcome::Result(result) = handled.response.outcome else {
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
