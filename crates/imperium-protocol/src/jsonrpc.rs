//! JSON-RPC 2.0 envelopes.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use ts_rs::TS;

/// The literal `"2.0"` version marker. Deserialising any other value fails.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, TS)]
#[ts(export, type = "\"2.0\"")]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let version = String::deserialize(deserializer)?;
        if version == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported JSON-RPC version {version:?}, expected \"2.0\""
            )))
        }
    }
}

/// A request identifier chosen by the client.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// A call from client to daemon. Notifications (requests without an id) are not accepted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Request {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "unknown")]
    pub params: Option<Value>,
}

/// The daemon's reply to one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Response {
    pub jsonrpc: JsonRpcVersion,
    /// `null` only when the request was too malformed to read its id.
    pub id: Option<RequestId>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Exactly one of `result` or `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum Outcome {
    Result(#[ts(type = "unknown")] Value),
    Error(ErrorObject),
}

impl Response {
    pub fn success(id: RequestId, result: Value) -> Self {
        Response {
            jsonrpc: JsonRpcVersion,
            id: Some(id),
            outcome: Outcome::Result(result),
        }
    }

    pub fn failure(id: Option<RequestId>, error: ErrorObject) -> Self {
        Response {
            jsonrpc: JsonRpcVersion,
            id,
            outcome: Outcome::Error(error),
        }
    }
}

/// A message from daemon to client that needs no reply, such as a session event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Notification {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    #[ts(type = "unknown")]
    pub params: Value,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Notification {
            jsonrpc: JsonRpcVersion,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "unknown")]
    pub data: Option<Value>,
}

impl ErrorObject {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        ErrorObject {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Error codes: the JSON-RPC reserved range plus Imperium's own (-32000 to -32099).
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    /// The client's protocol major version differs from the daemon's.
    pub const INCOMPATIBLE_PROTOCOL: i32 = -32001;
    /// A method other than `system.handshake` was called before the handshake.
    pub const HANDSHAKE_REQUIRED: i32 = -32002;
    /// No session has the given id.
    pub const SESSION_NOT_FOUND: i32 = -32003;
    /// The requested model cannot be used (unknown endpoint, missing key, no default).
    pub const MODEL_UNAVAILABLE: i32 = -32004;
    /// The caller is not allowed to use this method (a paired phone outside its scopes).
    pub const FORBIDDEN: i32 = -32005;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trips_with_and_without_params() {
        let text = r#"{"jsonrpc":"2.0","id":7,"method":"system.ping"}"#;
        let request: Request = serde_json::from_str(text).unwrap();
        assert_eq!(request.id, RequestId::Number(7));
        assert_eq!(request.params, None);
        assert_eq!(serde_json::to_string(&request).unwrap(), text);

        let with_params: Request =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":"a","method":"m","params":{"x":1}}))
                .unwrap();
        assert_eq!(with_params.id, RequestId::String("a".into()));
        assert_eq!(with_params.params, Some(json!({"x": 1})));
    }

    #[test]
    fn other_jsonrpc_versions_are_rejected() {
        let result = serde_json::from_str::<Request>(r#"{"jsonrpc":"1.0","id":1,"method":"m"}"#);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported JSON-RPC version")
        );
    }

    #[test]
    fn responses_carry_exactly_one_of_result_or_error() {
        let ok = Response::success(RequestId::Number(1), json!({"a": true}));
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"jsonrpc":"2.0","id":1,"result":{"a":true}})
        );

        let err = Response::failure(None, ErrorObject::new(codes::PARSE_ERROR, "bad json"));
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad json"}})
        );

        let parsed: Response =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"result":null})).unwrap();
        assert_eq!(parsed.outcome, Outcome::Result(Value::Null));
    }
}
