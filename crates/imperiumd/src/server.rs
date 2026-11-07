//! The local WebSocket transport.

use std::future::Future;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::get;
use imperium_protocol::jsonrpc::{ErrorObject, Notification, Response, codes};
use imperium_protocol::session::{SESSION_EVENT, SessionEventParams};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

use crate::rpc::Subscription;
use crate::{AccessToken, Connection, DaemonState};

struct ServerState {
    token: AccessToken,
    daemon: Arc<DaemonState>,
}

/// Serves JSON-RPC on `GET /rpc` (WebSocket upgrade) until `shutdown` resolves.
/// Upgrades without `Authorization: Bearer <token>` are refused with 401.
pub async fn serve(
    listener: TcpListener,
    token: AccessToken,
    daemon: Arc<DaemonState>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let state = Arc::new(ServerState { token, daemon });
    let app = Router::new().route("/rpc", get(upgrade)).with_state(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

async fn upgrade(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> HttpResponse {
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| state.token.accepts_header(value));
    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let daemon = Arc::clone(&state.daemon);
    ws.on_upgrade(move |socket| run_connection(socket, daemon))
}

/// Reads requests and writes responses, interleaving notifications from subscribed sessions.
async fn run_connection(mut socket: WebSocket, daemon: Arc<DaemonState>) {
    let mut connection = Connection::new(daemon);
    let (notify, mut notifications) = mpsc::unbounded_channel::<Notification>();
    loop {
        let outgoing = tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                let handled = match message {
                    Message::Text(text) => connection.handle_text(text.as_str()),
                    Message::Binary(_) => crate::rpc::Handled {
                        response: Response::failure(
                            None,
                            ErrorObject::new(codes::INVALID_REQUEST, "send JSON-RPC as text frames"),
                        ),
                        subscription: None,
                    },
                    Message::Close(_) => break,
                    // Pings and pongs are answered by the WebSocket layer.
                    Message::Ping(_) | Message::Pong(_) => continue,
                };
                if let Some(subscription) = handled.subscription {
                    tokio::spawn(forward(subscription, notify.clone()));
                }
                serde_json::to_string(&handled.response)
            }
            Some(notification) = notifications.recv() => serde_json::to_string(&notification),
        };
        let text = outgoing.expect("protocol messages serialise to JSON");
        if socket.send(Message::Text(text.into())).await.is_err() {
            break;
        }
    }
}

/// Turns a session's events into notifications until the session or the connection ends.
async fn forward(mut subscription: Subscription, notify: mpsc::UnboundedSender<Notification>) {
    loop {
        let event = match subscription.events.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(session = %subscription.session_id, missed, "client fell behind; events dropped");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let params = SessionEventParams {
            session_id: subscription.session_id.clone(),
            event,
        };
        let params = serde_json::to_value(params).expect("session events serialise to JSON");
        if notify
            .send(Notification::new(SESSION_EVENT, params))
            .is_err()
        {
            return;
        }
    }
}
