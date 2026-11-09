//! A client for the daemon protocol: connect with an endpoint's token, handshake, and make
//! typed calls. Used by the `imperium` CLI and the desktop app's native side.

use std::collections::VecDeque;

use futures_util::{SinkExt, StreamExt};
use imperium_protocol::PROTOCOL_VERSION;
pub use imperium_protocol::discovery::read_endpoint;
use imperium_protocol::discovery::{DaemonEndpoint, EndpointError};
use imperium_protocol::jsonrpc::{
    ErrorObject, JsonRpcVersion, Notification, Outcome, Request, RequestId, Response,
};
use imperium_protocol::methods::{Handshake, HandshakeParams, HandshakeResult, Method, Peer};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    #[error("could not connect to imperium-core: {0}")]
    Connect(Box<tungstenite::Error>),
    #[error("imperium-core returned error {}: {}", .0.code, .0.message)]
    Rpc(ErrorObject),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// A handshaken connection to the daemon. Calls are sequential: one request, one reply.
#[derive(Debug)]
pub struct Client {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: i64,
    daemon: HandshakeResult,
    /// Notifications that arrived while waiting for a response.
    pending: VecDeque<Notification>,
}

impl Client {
    /// Connects with the endpoint's token and performs `system.handshake`.
    pub async fn connect(endpoint: &DaemonEndpoint, client: Peer) -> Result<Self, ClientError> {
        let mut request = endpoint
            .url
            .as_str()
            .into_client_request()
            .map_err(|error| ClientError::Connect(Box::new(error)))?;
        let authorization = format!("Bearer {}", endpoint.token)
            .parse()
            .map_err(|_| ClientError::Protocol("endpoint token is not a valid header".into()))?;
        request.headers_mut().insert("Authorization", authorization);
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|error| ClientError::Connect(Box::new(error)))?;

        let mut connection = Client {
            socket,
            next_id: 1,
            pending: VecDeque::new(),
            daemon: HandshakeResult {
                daemon: Peer {
                    name: String::new(),
                    version: String::new(),
                },
                protocol: PROTOCOL_VERSION,
            },
        };
        connection.daemon = connection
            .call::<Handshake>(HandshakeParams {
                client,
                protocol: PROTOCOL_VERSION,
            })
            .await?;
        Ok(connection)
    }

    /// What the daemon reported in the handshake.
    pub fn daemon(&self) -> &HandshakeResult {
        &self.daemon
    }

    pub async fn call<M: Method>(&mut self, params: M::Params) -> Result<M::Result, ClientError> {
        let id = RequestId::Number(self.next_id);
        self.next_id += 1;
        let request = Request {
            jsonrpc: JsonRpcVersion,
            id: id.clone(),
            method: M::NAME.to_string(),
            params: Some(serde_json::to_value(params).map_err(protocol_error)?),
        };
        let text = serde_json::to_string(&request).map_err(protocol_error)?;
        self.socket
            .send(Message::Text(text.into()))
            .await
            .map_err(|error| ClientError::Connect(Box::new(error)))?;

        let response = self.next_response().await?;
        if response.id.as_ref() != Some(&id) {
            return Err(ClientError::Protocol(format!(
                "expected a reply to request {id:?}, got {:?}",
                response.id
            )));
        }
        match response.outcome {
            Outcome::Result(value) => serde_json::from_value(value).map_err(protocol_error),
            Outcome::Error(error) => Err(ClientError::Rpc(error)),
        }
    }

    /// The next server notification, in arrival order.
    pub async fn next_notification(&mut self) -> Result<Notification, ClientError> {
        if let Some(notification) = self.pending.pop_front() {
            return Ok(notification);
        }
        match self.next_message().await? {
            Incoming::Notification(notification) => Ok(notification),
            Incoming::Response(response) => Err(ClientError::Protocol(format!(
                "unexpected response to request {:?}",
                response.id
            ))),
        }
    }

    /// Reads frames until a response arrives, queueing notifications for later.
    async fn next_response(&mut self) -> Result<Response, ClientError> {
        loop {
            match self.next_message().await? {
                Incoming::Response(response) => return Ok(response),
                Incoming::Notification(notification) => self.pending.push_back(notification),
            }
        }
    }

    async fn next_message(&mut self) -> Result<Incoming, ClientError> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or_else(|| ClientError::Protocol("imperium-core closed the connection".into()))?
                .map_err(|error| ClientError::Connect(Box::new(error)))?;
            match message {
                Message::Text(text) => {
                    let value: serde_json::Value =
                        serde_json::from_str(text.as_str()).map_err(protocol_error)?;
                    // Notifications carry a method and no id; responses carry an id.
                    return if value.get("method").is_some() && value.get("id").is_none() {
                        serde_json::from_value(value)
                            .map(Incoming::Notification)
                            .map_err(protocol_error)
                    } else {
                        serde_json::from_value(value)
                            .map(Incoming::Response)
                            .map_err(protocol_error)
                    };
                }
                Message::Close(_) => {
                    return Err(ClientError::Protocol(
                        "imperium-core closed the connection".into(),
                    ));
                }
                Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
    }
}

enum Incoming {
    Response(Response),
    Notification(Notification),
}

fn protocol_error(error: serde_json::Error) -> ClientError {
    ClientError::Protocol(error.to_string())
}
