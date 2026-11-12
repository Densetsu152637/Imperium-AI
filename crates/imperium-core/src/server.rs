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
use imperium_protocol::methods::RESOURCES_CHANGED;
use imperium_protocol::session::{SESSION_EVENT, SessionEventParams};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

use crate::rpc::Subscription;
use crate::{AccessToken, Connection, DaemonState};

struct ServerState {
    token: AccessToken,
    daemon: Arc<DaemonState>,
}

/// WebSocket subprotocol a browser client offers alongside its token, since browsers cannot
/// set an `Authorization` header on WebSocket requests.
const SUBPROTOCOL: &str = "imperium.v0";
const TOKEN_SUBPROTOCOL_PREFIX: &str = "imperium.token.";

/// Serves JSON-RPC on `GET /rpc` (WebSocket upgrade) until `shutdown` resolves.
/// The upgrade must carry the token as `Authorization: Bearer <token>` or, for browser
/// clients, as the subprotocol `imperium.token.<token>` next to `imperium.v0`; anything
/// else is refused with 401.
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
    let by_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| state.token.accepts_header(value));
    let by_subprotocol = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|offered| {
            offered
                .split(',')
                .filter_map(|protocol| protocol.trim().strip_prefix(TOKEN_SUBPROTOCOL_PREFIX))
                .any(|token| state.token.accepts_header(&format!("Bearer {token}")))
        });
    if !by_header && !by_subprotocol {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let daemon = Arc::clone(&state.daemon);
    ws.protocols([SUBPROTOCOL]).on_upgrade(move |socket| {
        let announcements = daemon.announcements();
        run_connection(socket, Connection::new(daemon), announcements)
    })
}

/// Reads requests and writes responses, interleaving notifications from subscribed sessions
/// and replies that took a while (such as Hugging Face listings).
pub(crate) async fn run_connection(
    mut socket: WebSocket,
    mut connection: Connection,
    mut announcements: broadcast::Receiver<Notification>,
) {
    let (notify, mut notifications) = mpsc::unbounded_channel::<Notification>();
    let (reply, mut replies) = mpsc::unbounded_channel::<Response>();
    loop {
        let outgoing = tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break };
                let handled = match message {
                    Message::Text(text) => connection.handle_text(text.as_str()),
                    Message::Binary(_) => crate::rpc::Handled {
                        response: Some(Response::failure(
                            None,
                            ErrorObject::new(codes::INVALID_REQUEST, "send JSON-RPC as text frames"),
                        )),
                        subscription: None,
                        deferred: None,
                    },
                    Message::Close(_) => break,
                    // Pings and pongs are answered by the WebSocket layer.
                    Message::Ping(_) | Message::Pong(_) => continue,
                };
                if let Some(subscription) = handled.subscription {
                    tokio::spawn(forward(subscription, notify.clone(), connection.state()));
                }
                if let Some(deferred) = handled.deferred {
                    let reply = reply.clone();
                    tokio::spawn(async move {
                        // The client may have gone; then nobody needs the reply.
                        let _ = reply.send(deferred.await);
                    });
                }
                match handled.response {
                    Some(response) => serde_json::to_string(&response),
                    None => continue,
                }
            }
            Some(response) = replies.recv() => serde_json::to_string(&response),
            Some(notification) = notifications.recv() => serde_json::to_string(&notification),
            announcement = announcements.recv() => match announcement {
                // Resource snapshots go only to connections that asked for them.
                Ok(notification)
                    if notification.method == RESOURCES_CHANGED
                        && !connection.watches_resources() => continue,
                Ok(notification) => serde_json::to_string(&notification),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
        };
        let text = outgoing.expect("protocol messages serialise to JSON");
        if socket.send(Message::Text(text.into())).await.is_err() {
            break;
        }
    }
}

/// Turns a session's events into notifications until the session or the connection ends.
async fn forward(
    mut subscription: Subscription,
    notify: mpsc::UnboundedSender<Notification>,
    state: Arc<crate::DaemonState>,
) {
    loop {
        let Some(event) = crate::rpc::next_event(&state, &mut subscription).await else {
            return;
        };
        let params = SessionEventParams {
            session_id: subscription.session_id.clone(),
            agent_event: event,
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
