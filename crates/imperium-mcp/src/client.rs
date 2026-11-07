//! A JSON-RPC connection to one MCP server over stdio (newline-delimited messages) or
//! streamable HTTP (POSTed messages answered with JSON or an SSE stream).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// The MCP revision this client speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

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
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    tracing::debug!(%line, "ignoring non-JSON output from an MCP server");
                    continue;
                };
                dispatch(message, &reading, &replies);
            }
            // The server is gone: fail everything still waiting.
            for (_, waiter) in lock(&reading).drain() {
                let _ = waiter.send(Err(McpError::Closed));
            }
        });
        Client {
            transport: Transport::Stream { outgoing, pending },
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
            next_id: AtomicU64::new(1),
            timeout,
        }
    }

    /// Sends a request and waits for its result.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let seconds = self.timeout.as_secs();
        let answer = async {
            match &self.transport {
                Transport::Stream { outgoing, pending } => {
                    let (sender, receiver) = oneshot::channel();
                    lock(pending).insert(id, sender);
                    if outgoing.send(message.to_string()).is_err() {
                        lock(pending).remove(&id);
                        return Err(McpError::Closed);
                    }
                    let result = receiver.await.unwrap_or(Err(McpError::Closed));
                    lock(pending).remove(&id);
                    result
                }
                Transport::Http(http) => http.post(&message, Some(id)).await,
            }
        };
        tokio::time::timeout(self.timeout, answer)
            .await
            .unwrap_or(Err(McpError::Timeout(seconds)))
    }

    /// Sends a notification (no answer expected).
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let message = json!({"jsonrpc": "2.0", "method": method, "params": params});
        match &self.transport {
            Transport::Stream { outgoing, .. } => outgoing
                .send(message.to_string())
                .map_err(|_| McpError::Closed),
            Transport::Http(http) => http.post(&message, None).await.map(|_| ()),
        }
    }
}

impl Http {
    async fn post(&self, message: &Value, id: Option<u64>) -> Result<Value, McpError> {
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
        let Some(id) = id else {
            return Ok(Value::Null);
        };
        let is_stream = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if !is_stream {
            let message: Value = response.json().await.map_err(transport)?;
            return result_of(&message, id);
        }
        // An SSE stream may carry server notifications before the response.
        let mut body = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = body.next().await {
            buffer.push_str(&String::from_utf8_lossy(&chunk.map_err(transport)?));
            while let Some(end) = buffer.find("\n\n") {
                let event: String = buffer.drain(..end + 2).collect();
                let data: String = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(message) = serde_json::from_str::<Value>(&data)
                    && message.get("id").and_then(Value::as_u64) == Some(id)
                {
                    return result_of(&message, id);
                }
            }
        }
        Err(McpError::Closed)
    }
}

/// Routes one message read from a stream: responses to their waiters, server requests to a
/// reply.
fn dispatch(message: Value, pending: &Pending, replies: &mpsc::UnboundedSender<String>) {
    let id = message.get("id").cloned();
    match (message.get("method").and_then(Value::as_str), id) {
        (Some(method), Some(id)) => {
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
