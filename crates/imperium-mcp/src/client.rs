//! A JSON-RPC connection to one MCP server over stdio (newline-delimited messages) or
//! streamable HTTP (POSTed messages answered with JSON or an SSE stream).
//!
//! Requests the server makes of the client are pings (always answered) and, only where the
//! client was told to allow it and a request of its own is in flight, sampling; everything else
//! is refused. Sampling is answered inside the request that is waiting, by the [`Sampling`]
//! that request was given.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::Instant;

pub use crate::sampling::Sampling;

/// The MCP revision this client speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

const SAMPLING_METHOD: &str = "sampling/createMessage";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum McpError {
    #[error("the server closed the connection")]
    Closed,
    #[error("no answer within {0} s")]
    Timeout(u64),
    #[error("server error {code}: {message}")]
    Server { code: i64, message: String },
    #[error("{0}")]
    Transport(String),
    #[error("unexpected response: {0}")]
    Protocol(String),
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>>;

/// A request the server made of the client.
struct ServerRequest {
    id: Value,
    method: String,
    params: Value,
}

/// What the server asked while this client's own requests are in flight.
#[derive(Default)]
struct Inbound {
    /// Sampling requests read from a stream, waiting for a request of ours to answer them.
    queue: Mutex<VecDeque<ServerRequest>>,
    wake: Notify,
    /// Requests of ours waiting for an answer.
    in_flight: AtomicUsize,
    /// Whether the server may sample at all.
    sampling: AtomicBool,
}

impl Inbound {
    fn pop(&self) -> Option<ServerRequest> {
        lock(&self.queue).pop_front()
    }
}

/// Counts one request of ours as waiting for its answer.
/// When the last one ends (answered, timed out, or cancelled), sampling requests still queued
/// are refused: nobody is left to answer them.
struct InFlight<'a> {
    inbound: &'a Inbound,
    replies: Option<&'a mpsc::UnboundedSender<String>>,
}

impl<'a> InFlight<'a> {
    fn new(inbound: &'a Inbound, replies: Option<&'a mpsc::UnboundedSender<String>>) -> Self {
        inbound.in_flight.fetch_add(1, Ordering::SeqCst);
        InFlight { inbound, replies }
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if self.inbound.in_flight.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        while let Some(request) = self.inbound.pop() {
            if let Some(replies) = self.replies {
                let refusal = reply_message(
                    &request.id,
                    Err((-1, "the call ended before the request was answered".into())),
                );
                let _ = replies.send(refusal.to_string());
            }
        }
    }
}

enum Transport {
    /// Messages to write; a background task owns the writer.
    Stream {
        outgoing: mpsc::UnboundedSender<String>,
        pending: Pending,
    },
    Http(Http),
}

struct Http {
    http: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    /// `Mcp-Session-Id` assigned by the server at initialisation.
    session: Mutex<Option<String>>,
}

pub struct Client {
    transport: Transport,
    inbound: Arc<Inbound>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.transport {
            Transport::Stream { .. } => "stream".to_string(),
            Transport::Http(http) => http.url.clone(),
        };
        f.debug_struct("Client")
            .field("transport", &kind)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Client {
    /// A client over a byte stream, such as a child process's stdin and stdout. Spawns the
    /// reader and writer tasks; requests the server sends are answered (ping) or refused.
    pub fn over_stream(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
        timeout: Duration,
    ) -> Self {
        let pending: Pending = Arc::default();
        let inbound: Arc<Inbound> = Arc::default();
        let (outgoing, mut queue) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(message) = queue.recv().await {
                let line = format!("{message}\n");
                if writer.write_all(line.as_bytes()).await.is_err() || writer.flush().await.is_err()
                {
                    break;
                }
            }
        });
        let replies = outgoing.clone();
        let reading = Arc::clone(&pending);
        let incoming = Arc::clone(&inbound);
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    tracing::debug!(%line, "ignoring non-JSON output from an MCP server");
                    continue;
                };
                dispatch(message, &reading, &replies, &incoming);
            }
            // The server is gone: fail everything still waiting.
            for (_, waiter) in lock(&reading).drain() {
                let _ = waiter.send(Err(McpError::Closed));
            }
        });
        Client {
            transport: Transport::Stream { outgoing, pending },
            inbound,
            next_id: AtomicU64::new(1),
            timeout,
        }
    }

    /// A client for a streamable-HTTP server at `url`.
    pub fn over_http(url: String, headers: Vec<(String, String)>, timeout: Duration) -> Self {
        Client {
            transport: Transport::Http(Http {
                http: reqwest::Client::new(),
                url,
                headers,
                session: Mutex::new(None),
            }),
            inbound: Arc::default(),
            next_id: AtomicU64::new(1),
            timeout,
        }
    }

    /// Lets the server send sampling requests, which are answered only inside a request made
    /// with [`Client::request_with`] and a [`Sampling`]. Off until this is called.
    pub fn allow_sampling(&self) {
        self.inbound.sampling.store(true, Ordering::SeqCst);
    }

    /// Sends a request and waits for its result. Sampling requests the server makes meanwhile
    /// are refused.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        self.request_with(method, params, None).await
    }

    /// Like [`Client::request`], answering the server's sampling requests with `sampling` while
    /// it waits. Time spent answering (the user deciding, a model writing) does not count
    /// toward the timeout.
    pub async fn request_with(
        &self,
        method: &str,
        params: Value,
        sampling: Option<Sampling<'_>>,
    ) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let seconds = self.timeout.as_secs();
        let _waiting = InFlight::new(
            &self.inbound,
            match &self.transport {
                Transport::Stream { outgoing, .. } => Some(outgoing),
                Transport::Http(_) => None,
            },
        );
        match &self.transport {
            Transport::Stream { outgoing, pending } => {
                let (sender, mut receiver) = oneshot::channel();
                lock(pending).insert(id, sender);
                if outgoing.send(message.to_string()).is_err() {
                    lock(pending).remove(&id);
                    return Err(McpError::Closed);
                }
                let mut deadline = Instant::now() + self.timeout;
                let result = loop {
                    tokio::select! {
                        result = &mut receiver => break result.unwrap_or(Err(McpError::Closed)),
                        () = self.inbound.wake.notified() => {
                            // Only the time spent answering is given back; a server that
                            // keeps sending requests cannot keep the call alive.
                            let started = Instant::now();
                            while let Some(request) = self.inbound.pop() {
                                self.serve(request, sampling).await;
                            }
                            deadline += started.elapsed();
                        }
                        () = tokio::time::sleep_until(deadline) => {
                            break Err(McpError::Timeout(seconds));
                        }
                    }
                };
                lock(pending).remove(&id);
                result
            }
            Transport::Http(http) => {
                http.post(&message, Some(id), &self.inbound, sampling, self.timeout)
                    .await
            }
        }
    }

    /// Sends a notification (no answer expected).
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        match &self.transport {
            Transport::Stream { outgoing, .. } => outgoing
                .send(message.to_string())
                .map_err(|_| McpError::Closed),
            Transport::Http(http) => http
                .post(&message, None, &self.inbound, None, self.timeout)
                .await
                .map(|_| ()),
        }
    }

    /// Answers a sampling request that was read from a stream.
    async fn serve(&self, request: ServerRequest, sampling: Option<Sampling<'_>>) {
        // Only stream transports queue requests; HTTP ones are answered where they arrive.
        let Transport::Stream { outgoing, .. } = &self.transport else {
            return;
        };
        // If the call is cancelled while the request is being answered, the server is told so.
        let mut unanswered = Refusal {
            replies: outgoing,
            id: Some(request.id.clone()),
        };
        let outcome = match (request.method.as_str(), sampling) {
            (SAMPLING_METHOD, Some(sampling)) => {
                crate::sampling::create_message(sampling, &request.params).await
            }
            (method, _) => Err((
                -32601,
                format!("{method} is not available outside a tool call"),
            )),
        };
        unanswered.id = None;
        let reply = reply_message(&request.id, outcome);
        let _ = outgoing.send(reply.to_string());
    }
}

/// Refuses a server's request when dropped while still unanswered.
struct Refusal<'a> {
    replies: &'a mpsc::UnboundedSender<String>,
    id: Option<Value>,
}

impl Drop for Refusal<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let refusal = reply_message(
                &id,
                Err((-1, "the call ended before the request was answered".into())),
            );
            let _ = self.replies.send(refusal.to_string());
        }
    }
}

impl Http {
    /// POSTs one message and checks the status.
    async fn send(&self, message: &Value) -> Result<reqwest::Response, McpError> {
        let mut request = self
            .http
            .post(&self.url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL_VERSION)
            .json(message);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(id) = lock(&self.session).clone() {
            request = request.header("mcp-session-id", id);
        }
        let transport = |error: reqwest::Error| McpError::Transport(error.to_string());
        let response = request.send().await.map_err(transport)?;
        if let Some(id) = response.headers().get("mcp-session-id")
            && let Ok(id) = id.to_str()
        {
            *lock(&self.session) = Some(id.to_string());
        }
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!("HTTP {status}: {body}")));
        }
        Ok(response)
    }

    async fn post(
        &self,
        message: &Value,
        id: Option<u64>,
        inbound: &Inbound,
        sampling: Option<Sampling<'_>>,
        limit: Duration,
    ) -> Result<Value, McpError> {
        let seconds = limit.as_secs();
        let timed_out = || McpError::Timeout(seconds);
        let transport = |error: reqwest::Error| McpError::Transport(error.to_string());
        let mut deadline = Instant::now() + limit;
        let response = tokio::time::timeout_at(deadline, self.send(message))
            .await
            .map_err(|_| timed_out())??;
        let Some(id) = id else {
            return Ok(Value::Null);
        };
        let is_stream = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if !is_stream {
            let message: Value = tokio::time::timeout_at(deadline, response.json())
                .await
                .map_err(|_| timed_out())?
                .map_err(transport)?;
            return result_of(&message, id);
        }
        // An SSE stream may carry server notifications and requests before the response.
        let mut body = response.bytes_stream();
        let mut buffer = String::new();
        loop {
            let next = tokio::time::timeout_at(deadline, body.next())
                .await
                .map_err(|_| timed_out())?;
            let Some(chunk) = next else {
                return Err(McpError::Closed);
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk.map_err(transport)?));
            while let Some(end) = buffer.find("\n\n") {
                let event: String = buffer.drain(..end + 2).collect();
                let data: String = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                let Ok(message) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                if message.get("method").is_some() {
                    // A request or notification from the server.
                    if let Some(request_id) = message.get("id") {
                        let started = Instant::now();
                        let reply = answer(request_id, &message, inbound, sampling).await;
                        // Only the time spent answering is given back (a ping costs none).
                        deadline += started.elapsed();
                        let _ = tokio::time::timeout_at(deadline, self.send(&reply)).await;
                    }
                } else if message.get("id").and_then(Value::as_u64) == Some(id) {
                    return result_of(&message, id);
                }
            }
        }
    }
}

/// Answers a request the server sent in an SSE stream.
async fn answer(
    id: &Value,
    message: &Value,
    inbound: &Inbound,
    sampling: Option<Sampling<'_>>,
) -> Value {
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let outcome = match method {
        "ping" => Ok(json!({})),
        SAMPLING_METHOD if inbound.sampling.load(Ordering::SeqCst) => match sampling {
            Some(sampling) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                crate::sampling::create_message(sampling, &params).await
            }
            None => Err((-32601, format!("{method} is not available here"))),
        },
        _ => Err((-32601, format!("{method} is not supported by this client"))),
    };
    reply_message(id, outcome)
}

fn reply_message(id: &Value, outcome: Result<Value, (i64, String)>) -> Value {
    match outcome {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    }
}

/// Routes one message read from a stream: responses to their waiters, server requests to a
/// reply (sampling to the request that will answer it).
fn dispatch(
    message: Value,
    pending: &Pending,
    replies: &mpsc::UnboundedSender<String>,
    inbound: &Inbound,
) {
    let id = message.get("id").cloned();
    match (message.get("method").and_then(Value::as_str), id) {
        (Some(method), Some(id)) => {
            if method == SAMPLING_METHOD
                && inbound.sampling.load(Ordering::SeqCst)
                && inbound.in_flight.load(Ordering::SeqCst) > 0
            {
                lock(&inbound.queue).push_back(ServerRequest {
                    id,
                    method: method.to_string(),
                    params: message.get("params").cloned().unwrap_or(Value::Null),
                });
                inbound.wake.notify_one();
                return;
            }
            let reply = if method == "ping" {
                json!({"jsonrpc": "2.0", "id": id, "result": {}})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": -32601, "message": format!("{method} is not supported by this client")}})
            };
            let _ = replies.send(reply.to_string());
        }
        (Some(_), None) => {} // notifications, such as progress or list changes
        (None, Some(id)) => {
            let Some(id) = id.as_u64() else { return };
            if let Some(waiter) = lock(pending).remove(&id) {
                let _ = waiter.send(result_of(&message, id));
            }
        }
        (None, None) => {}
    }
}

fn result_of(message: &Value, id: u64) -> Result<Value, McpError> {
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return Err(McpError::Protocol(format!("expected the response to {id}")));
    }
    if let Some(error) = message.get("error") {
        return Err(McpError::Server {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::Protocol("a response with neither result nor error".into()))
}
