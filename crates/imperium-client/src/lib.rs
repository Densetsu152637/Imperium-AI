//! A client for the daemon protocol: connect with an endpoint's token, handshake, and make
//! typed calls. Used by the `imperium` CLI and the desktop app's native side.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{SinkExt, StreamExt};
use imperium_protocol::PROTOCOL_VERSION;
pub use imperium_protocol::discovery::read_endpoint;
use imperium_protocol::discovery::{DaemonEndpoint, EndpointError};
use imperium_protocol::jsonrpc::{
    ErrorObject, JsonRpcVersion, Notification, Outcome, Request, RequestId, Response,
};
use imperium_protocol::methods::{Handshake, HandshakeParams, HandshakeResult, Method, Peer};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
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

/// The byte stream under the WebSocket: loopback TCP, or the daemon's local socket.
#[derive(Debug)]
enum Transport {
    Tcp(MaybeTlsStream<TcpStream>),
    Local(imperium_ipc::Stream),
}

impl AsyncRead for Transport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Transport::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Transport::Local(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut *self {
            Transport::Tcp(stream) => Pin::new(stream).poll_write(cx, data),
            Transport::Local(stream) => Pin::new(stream).poll_write(cx, data),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Transport::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Transport::Local(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut *self {
            Transport::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Transport::Local(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// A handshaken connection to the daemon. Calls are sequential: one request, one reply.
#[derive(Debug)]
pub struct Client {
    socket: WebSocketStream<Transport>,
    next_id: i64,
    daemon: HandshakeResult,
    /// Notifications that arrived while waiting for a response.
    pending: VecDeque<Notification>,
}

impl Client {
    /// Connects with the endpoint's token and performs `system.handshake`. When the endpoint
    /// names a local socket, that is tried first (and used alone when there is no URL, as for a
    /// socket forwarded over SSH); otherwise, or when it cannot be reached, the loopback URL is.
    pub async fn connect(endpoint: &DaemonEndpoint, client: Peer) -> Result<Self, ClientError> {
        if let Some(address) = &endpoint.socket {
            match Self::connect_socket(address, &endpoint.token, Some(endpoint.pid), client.clone())
                .await
            {
                Ok(connected) => return Ok(connected),
                Err(error) if endpoint.url.is_empty() => return Err(error),
                Err(_) => {}
            }
        }
        let mut request = endpoint
            .url
            .as_str()
            .into_client_request()
            .map_err(|error| ClientError::Connect(Box::new(error)))?;
        request
            .headers_mut()
            .insert("Authorization", authorization(&endpoint.token)?);
        let uri = request.uri();
        let host = uri
            .host()
            .ok_or_else(|| ClientError::Protocol(format!("endpoint URL {uri} has no host")))?;
        let port = uri.port_u16().unwrap_or(80);
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|error| ClientError::Connect(Box::new(tungstenite::Error::Io(error))))?;
        let (socket, _) =
            tokio_tungstenite::client_async(request, Transport::Tcp(MaybeTlsStream::Plain(tcp)))
                .await
                .map_err(|error| ClientError::Connect(Box::new(error)))?;
        Self::handshake(socket, client).await
    }

    /// Connects over the daemon's local socket (a Unix socket's path or a Windows named pipe's
    /// name) with the same token the loopback listener takes, and performs `system.handshake`.
    ///
    /// On Windows, `expected_pid` (the daemon's process id) must match the process serving the
    /// pipe, or the connection is refused before the token is sent.
    pub async fn connect_socket(
        address: &str,
        token: &str,
        expected_pid: Option<u32>,
        client: Peer,
    ) -> Result<Self, ClientError> {
        let stream = imperium_ipc::connect(address, expected_pid)
            .await
            .map_err(|error| ClientError::Connect(Box::new(tungstenite::Error::Io(error))))?;
        // The host and path only fill in the upgrade request; nothing resolves `localhost`.
        let mut request = "ws://localhost/rpc"
            .into_client_request()
            .map_err(|error| ClientError::Connect(Box::new(error)))?;
        request
            .headers_mut()
            .insert("Authorization", authorization(token)?);
        let (socket, _) = tokio_tungstenite::client_async(request, Transport::Local(stream))
            .await
            .map_err(|error| ClientError::Connect(Box::new(error)))?;
        Self::handshake(socket, client).await
    }

    async fn handshake(
        socket: WebSocketStream<Transport>,
        client: Peer,
    ) -> Result<Self, ClientError> {
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

fn authorization(token: &str) -> Result<tungstenite::http::HeaderValue, ClientError> {
    format!("Bearer {token}")
        .parse()
        .map_err(|_| ClientError::Protocol("endpoint token is not a valid header".into()))
}

fn protocol_error(error: serde_json::Error) -> ClientError {
    ClientError::Protocol(error.to_string())
}
