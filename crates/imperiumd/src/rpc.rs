//! Request handling for one client connection.

use std::time::Instant;

use imperium_protocol::jsonrpc::{ErrorObject, Request, RequestId, Response, codes};
use imperium_protocol::methods::{
    Handshake, HandshakeParams, HandshakeResult, Method, Peer, Ping, PingParams, PingResult,
};
use imperium_protocol::{PROTOCOL_VERSION, ProtocolVersion};
use serde::Serialize;
use serde_json::{Value, json};

/// Per-connection protocol state. The first call must be `system.handshake`.
#[derive(Debug)]
pub struct Connection {
    daemon_started: Instant,
    handshake_done: bool,
}

impl Connection {
    pub fn new(daemon_started: Instant) -> Self {
        Connection {
            daemon_started,
            handshake_done: false,
        }
    }

    /// Handles one text frame and returns the response to send back.
    pub fn handle_text(&mut self, text: &str) -> Response {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                return Response::failure(
                    None,
                    ErrorObject::new(codes::PARSE_ERROR, format!("invalid JSON: {error}")),
                );
            }
        };
        // Recover the id, if readable, so even an invalid request gets a matched error.
        let id = value
            .get("id")
            .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok());
        match serde_json::from_value::<Request>(value) {
            Ok(request) => self.handle(request),
            Err(error) => Response::failure(
                id,
                ErrorObject::new(codes::INVALID_REQUEST, format!("invalid request: {error}")),
            ),
        }
    }

    fn handle(&mut self, request: Request) -> Response {
        let outcome = match request.method.as_str() {
            Handshake::NAME => call::<Handshake>(request.params, |params| self.handshake(params)),
            _ if !self.handshake_done => Err(ErrorObject::new(
                codes::HANDSHAKE_REQUIRED,
                "call system.handshake first",
            )),
            Ping::NAME => call::<Ping>(request.params, |_: PingParams| {
                Ok(PingResult {
                    uptime_ms: u64::try_from(self.daemon_started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                })
            }),
            other => Err(ErrorObject::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )),
        };
        match outcome {
            Ok(result) => Response::success(request.id, result),
            Err(error) => Response::failure(Some(request.id), error),
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
    use imperium_protocol::jsonrpc::Outcome;

    fn request(id: i64, method: &str, params: Value) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
    }

    fn handshake_params(major: u32) -> Value {
        json!({
            "client": {"name": "test", "version": "0"},
            "protocol": {"major": major, "minor": 0}
        })
    }

    fn error_code(response: &Response) -> Option<i32> {
        match &response.outcome {
            Outcome::Error(error) => Some(error.code),
            Outcome::Result(_) => None,
        }
    }

    #[test]
    fn methods_other_than_handshake_require_a_handshake_first() {
        let mut connection = Connection::new(Instant::now());
        let response = connection.handle_text(&request(1, Ping::NAME, json!({})));
        assert_eq!(error_code(&response), Some(codes::HANDSHAKE_REQUIRED));
        assert_eq!(response.id, Some(RequestId::Number(1)));
    }

    #[test]
    fn ping_succeeds_after_a_compatible_handshake() {
        let mut connection = Connection::new(Instant::now());
        let handshake = connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        let Outcome::Result(result) = handshake.outcome else {
            panic!("handshake failed: {handshake:?}");
        };
        let result: HandshakeResult = serde_json::from_value(result).unwrap();
        assert_eq!(result.daemon.name, "imperiumd");

        let ping = connection.handle_text(&request(2, Ping::NAME, Value::Null));
        assert_eq!(error_code(&ping), None, "{ping:?}");
    }

    #[test]
    fn an_incompatible_major_version_is_refused() {
        let mut connection = Connection::new(Instant::now());
        let response = connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major + 1),
        ));
        assert_eq!(error_code(&response), Some(codes::INCOMPATIBLE_PROTOCOL));
        let ping = connection.handle_text(&request(2, Ping::NAME, json!({})));
        assert_eq!(error_code(&ping), Some(codes::HANDSHAKE_REQUIRED));
    }

    #[test]
    fn malformed_input_gets_the_matching_error() {
        let mut connection = Connection::new(Instant::now());
        assert_eq!(
            error_code(&connection.handle_text("{not json")),
            Some(codes::PARSE_ERROR)
        );

        let invalid = connection.handle_text(r#"{"jsonrpc":"2.0","id":5}"#);
        assert_eq!(error_code(&invalid), Some(codes::INVALID_REQUEST));
        assert_eq!(invalid.id, Some(RequestId::Number(5)));

        let bad_params = connection.handle_text(&request(3, Handshake::NAME, json!({"x": 1})));
        assert_eq!(error_code(&bad_params), Some(codes::INVALID_PARAMS));
    }

    #[test]
    fn unknown_methods_are_reported() {
        let mut connection = Connection::new(Instant::now());
        connection.handle_text(&request(
            1,
            Handshake::NAME,
            handshake_params(PROTOCOL_VERSION.major),
        ));
        let response = connection.handle_text(&request(2, "system.nope", json!({})));
        assert_eq!(error_code(&response), Some(codes::METHOD_NOT_FOUND));
    }
}
