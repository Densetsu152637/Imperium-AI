//! The local WebSocket transport.

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::get;
use imperium_protocol::jsonrpc::{ErrorObject, Response, codes};
use tokio::net::TcpListener;

use crate::{AccessToken, Connection};

struct ServerState {
    token: AccessToken,
    started: Instant,
}

/// Serves JSON-RPC on `GET /rpc` (WebSocket upgrade) until `shutdown` resolves.
/// Upgrades without `Authorization: Bearer <token>` are refused with 401.
pub async fn serve(
    listener: TcpListener,
    token: AccessToken,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let state = Arc::new(ServerState {
        token,
        started: Instant::now(),
    });
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
    let started = state.started;
    ws.on_upgrade(move |socket| run_connection(socket, started))
}

async fn run_connection(mut socket: WebSocket, started: Instant) {
    let mut connection = Connection::new(started);
    while let Some(Ok(message)) = socket.recv().await {
        let response = match message {
            Message::Text(text) => connection.handle_text(text.as_str()),
            Message::Binary(_) => Response::failure(
                None,
                ErrorObject::new(codes::INVALID_REQUEST, "send JSON-RPC as text frames"),
            ),
            Message::Close(_) => break,
            // Pings and pongs are answered by the WebSocket layer.
            Message::Ping(_) | Message::Pong(_) => continue,
        };
        let text = serde_json::to_string(&response).expect("responses serialise to JSON");
        if socket.send(Message::Text(text.into())).await.is_err() {
            break;
        }
    }
}
