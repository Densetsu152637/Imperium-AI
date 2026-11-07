//! Typed method definitions. Each method names its wire string and its params/result types.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ProtocolVersion;
use crate::session::PermissionDecision;

/// A daemon method: its wire name and the shapes of its params and result.
pub trait Method {
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;
}

/// `system.handshake`: must be the first call on every connection.
#[derive(Debug)]
pub struct Handshake;

impl Method for Handshake {
    const NAME: &'static str = "system.handshake";
    type Params = HandshakeParams;
    type Result = HandshakeResult;
}

/// `system.ping`: a liveness check.
#[derive(Debug)]
pub struct Ping;

impl Method for Ping {
    const NAME: &'static str = "system.ping";
    type Params = PingParams;
    type Result = PingResult;
}

/// Names and versions a program, such as the CLI or the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Peer {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HandshakeParams {
    pub client: Peer,
    pub protocol: ProtocolVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HandshakeResult {
    pub daemon: Peer,
    pub protocol: ProtocolVersion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PingResult {
    /// Milliseconds since the daemon started.
    #[ts(type = "number")]
    pub uptime_ms: u64,
}

/// `session.create`: starts a session in a project folder. The calling connection receives the
/// session's `session.event` notifications.
#[derive(Debug)]
pub struct SessionCreate;

impl Method for SessionCreate {
    const NAME: &'static str = "session.create";
    type Params = SessionCreateParams;
    type Result = SessionCreateResult;
}

/// `session.send`: sends a user message. Messages sent during a turn run after it finishes.
#[derive(Debug)]
pub struct SessionSend;

impl Method for SessionSend {
    const NAME: &'static str = "session.send";
    type Params = SessionSendParams;
    type Result = Accepted;
}

/// `session.respond`: answers a `permission_requested` event.
#[derive(Debug)]
pub struct SessionRespond;

impl Method for SessionRespond {
    const NAME: &'static str = "session.respond";
    type Params = SessionRespondParams;
    type Result = Accepted;
}

/// `session.list`: the sessions running in this daemon.
#[derive(Debug)]
pub struct SessionList;

impl Method for SessionList {
    const NAME: &'static str = "session.list";
    type Params = SessionListParams;
    type Result = SessionListResult;
}

/// `session.subscribe`: receive a session's `session.event` notifications on this connection.
#[derive(Debug)]
pub struct SessionSubscribe;

impl Method for SessionSubscribe {
    const NAME: &'static str = "session.subscribe";
    type Params = SessionSubscribeParams;
    type Result = SessionSubscribeResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSummary {
    pub session_id: String,
    pub project_dir: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSubscribeParams {
    pub session_id: String,
}

/// Every event the session has emitted so far; later ones arrive as notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSubscribeResult {
    pub history: Vec<crate::session::AgentEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateParams {
    /// Absolute path of the project folder the session works in.
    pub project_dir: String,
    /// `<endpoint>:<model>`; defaults to `models.default` from the configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCreateResult {
    pub session_id: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSendParams {
    pub session_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionRespondParams {
    pub session_id: String,
    #[ts(type = "number")]
    pub request_id: u64,
    pub decision: PermissionDecision,
}

/// An empty acknowledgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Accepted {}
