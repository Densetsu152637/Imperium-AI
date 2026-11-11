//! The OpenAI-compatible local API server: other tools (editors, chat front ends, scripts) use
//! Imperium's local models through `GET /v1/models` and `POST /v1/chat/completions`.
//!
//! It is a listener of its own, off until `api.openai_compatible.enabled`, bound to loopback
//! unless `allow_lan` says otherwise, and every request needs a bearer token that is separate
//! from the daemon's and kept in the secret store (never in the config or the logs). A call
//! takes a slot on its model like any other caller, so it loads, queues, and shows up in
//! `model.residency`, and it counts against the budgets as the `api` source. Hosted models
//! are served only when the user lists them in `api.openai_compatible.expose`.

mod chat;
mod error;
mod models;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use imperium_config::{OpenAiApiSettings, SecretName, SecretStore, Settings};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    ApiModel, ApiSetParams, ApiStatus, ApiTokenParams, ApiTokenResult,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use self::error::ApiError;
use crate::{AccessToken, DaemonState};

/// The secret that holds the API token.
const TOKEN_SECRET: &str = "api.openai-token";
/// The user-config keys the server reads.
const KEY_PREFIX: &str = "api.openai_compatible.";
/// Where the search for a free port begins when the API is first enabled: above the ports
/// local model servers conventionally use, and below the range the OS hands out.
const FIRST_PORT: u16 = 11_500;
const PORT_ATTEMPTS: u16 = 100;
/// The largest request body: room for several inline images.
const MAX_BODY: usize = 64 * 1024 * 1024;
/// How long a stopping server gets to finish before it is cut off.
const STOP_GRACE: Duration = Duration::from_secs(1);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The server's state inside the daemon.
#[derive(Debug, Default)]
pub struct OpenAiApi {
    /// Held while the server starts or stops, so changes made at once do not race.
    lifecycle: tokio::sync::Mutex<()>,
    running: Mutex<Option<Running>>,
    /// Why the server is not running although it is enabled.
    error: Mutex<Option<String>>,
    /// The token requests are checked against, kept here so a request reads no keychain.
    token: Mutex<Option<AccessToken>>,
    requests: AtomicU64,
}

#[derive(Debug)]
struct Running {
    address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

/// What the routes share.
struct ApiState {
    daemon: Weak<DaemonState>,
    http: reqwest::Client,
}

impl ApiState {
    /// A number for each request, which names its holder to the local runtime.
    fn next_request(&self) -> u64 {
        self.daemon.upgrade().map_or(0, |daemon| {
            daemon.openai().requests.fetch_add(1, Ordering::Relaxed)
        })
    }
}

impl OpenAiApi {
    /// Where the server listens, while it does.
    pub fn address(&self) -> Option<SocketAddr> {
        lock(&self.running).as_ref().map(|running| running.address)
    }

    fn error(&self) -> Option<String> {
        lock(&self.error).clone()
    }

    /// The API token, made and stored on first use; with `rotate`, a new one that replaces it
    /// (the old one stops working at once).
    pub fn token(&self, secrets: &dyn SecretStore, rotate: bool) -> Result<String, String> {
        let name = secret_name();
        if !rotate {
            if let Some(token) = lock(&self.token).as_ref() {
                return Ok(token.as_str().to_string());
            }
            let stored = secrets
                .load(&name)
                .map_err(|error| format!("could not read the API token: {error}"))?;
            if let Some(stored) = stored {
                *lock(&self.token) = Some(AccessToken::from(stored.clone()));
                return Ok(stored);
            }
        }
        let fresh = AccessToken::generate()
            .map_err(|error| format!("could not generate an API token: {error}"))?;
        let fresh = AccessToken::from(format!("imp-{}", fresh.as_str()));
        secrets
            .store(&name, fresh.as_str())
            .map_err(|error| format!("could not store the API token: {error}"))?;
        let text = fresh.as_str().to_string();
        *lock(&self.token) = Some(fresh);
        Ok(text)
    }

    fn accepts(&self, authorization: &str) -> bool {
        lock(&self.token)
            .as_ref()
            .is_some_and(|token| token.accepts_header(authorization))
    }

    /// Stops listening. The server gets a moment to finish what it is answering, then is cut
    /// off, so the port is free for the next start.
    async fn stop(&self) {
        let running = lock(&self.running).take();
        let Some(mut running) = running else { return };
        if let Some(stop) = running.stop.take() {
            let _ = stop.send(());
        }
        if tokio::time::timeout(STOP_GRACE, &mut running.task)
            .await
            .is_err()
        {
            running.task.abort();
            let _ = (&mut running.task).await;
        }
        tracing::info!(address = %running.address, "the OpenAI-compatible API stopped");
    }
}

fn secret_name() -> SecretName {
    TOKEN_SECRET
        .parse()
        .expect("the token's secret name is valid")
}

impl DaemonState {
    /// Starts the OpenAI-compatible API server when `api.openai_compatible.enabled` is set.
    /// Call once at start; the server then follows the setting. `Ok(None)` when it is off.
    pub async fn start_openai(self: &Arc<Self>) -> Result<Option<SocketAddr>, String> {
        reconcile(self).await
    }

    /// Where the OpenAI-compatible API server listens, while it does.
    pub fn openai_address(&self) -> Option<SocketAddr> {
        self.openai().address()
    }
}

/// Makes the server match the settings: starts it, restarts it where it must listen elsewhere,
/// or stops it. The reason it could not start is kept for `api.get`.
pub(crate) async fn reconcile(state: &Arc<DaemonState>) -> Result<Option<SocketAddr>, String> {
    let api = state.openai();
    let _serial = api.lifecycle.lock().await;
    let result = apply(state).await;
    *lock(&api.error) = result.as_ref().err().cloned();
    if let Err(error) = &result {
        tracing::warn!(%error, "the OpenAI-compatible API is not running");
    }
    result
}

/// [`reconcile`] from code that cannot wait for it, such as saving a setting.
pub(crate) fn reconcile_later(state: &DaemonState) {
    if let Some(state) = state.this()
        && tokio::runtime::Handle::try_current().is_ok()
    {
        tokio::spawn(async move {
            let _ = reconcile(&state).await;
        });
    }
}

async fn apply(state: &Arc<DaemonState>) -> Result<Option<SocketAddr>, String> {
    let api = state.openai();
    let settings = state.settings();
    let config = &settings.api.openai_compatible;
    if !config.enabled {
        api.stop().await;
        return Ok(None);
    }
    let ip = match listen_ip(&config.bind, config.allow_lan) {
        Ok(ip) => ip,
        Err(error) => {
            api.stop().await;
            return Err(error);
        }
    };
    if let Some(address) = api.address()
        && address.ip() == ip
        && (config.port == 0 || address.port() == config.port)
    {
        return Ok(Some(address));
    }
    api.stop().await;
    // The token exists before anything listens, so no request is ever served without one.
    api.token(&**state.secrets(), false)?;
    let listener = if config.port == 0 {
        bind_first_free(ip).await
    } else {
        TcpListener::bind((ip, config.port)).await
    }
    .map_err(|error| {
        format!(
            "could not listen on {}: {error}",
            SocketAddr::new(ip, config.port)
        )
    })?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("could not read the listening address: {error}"))?;
    if config.port == 0 {
        // The first enable picks a port and keeps it, so tools can be set up once.
        save_port(state, address.port());
    }
    if !ip.is_loopback() {
        tracing::warn!(%address, "the OpenAI-compatible API listens beyond this computer (allow_lan)");
    }
    let app = router(Arc::new(ApiState {
        daemon: Arc::downgrade(state),
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("could not build the HTTP client: {error}"))?,
    }));
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the OpenAI-compatible API server failed");
        }
    });
    *lock(&api.running) = Some(Running {
        address,
        stop: Some(stop),
        task,
    });
    tracing::info!(%address, "the OpenAI-compatible API is listening");
    Ok(Some(address))
}

/// The address to listen on: a loopback address, or any address when `allow_lan` is set.
fn listen_ip(bind: &str, allow_lan: bool) -> Result<IpAddr, String> {
    let bind = bind.trim();
    let ip = if bind.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        bind.parse::<IpAddr>().map_err(|_| {
            format!("api.openai_compatible.bind {bind:?} is not an IP address such as 127.0.0.1")
        })?
    };
    if !ip.is_loopback() && !allow_lan {
        return Err(format!(
            "refusing to listen on {ip}: the API serves your models to anyone who has its \
             token, so it stays on this computer unless api.openai_compatible.allow_lan is set"
        ));
    }
    Ok(ip)
}

async fn bind_first_free(ip: IpAddr) -> std::io::Result<TcpListener> {
    for port in FIRST_PORT..FIRST_PORT + PORT_ATTEMPTS {
        if let Ok(listener) = TcpListener::bind((ip, port)).await {
            return Ok(listener);
        }
    }
    TcpListener::bind((ip, 0)).await
}

/// Writes the port the first enable chose to the user config and into the live settings.
fn save_port(state: &DaemonState, port: u16) {
    let key = format!("{KEY_PREFIX}port");
    let saved = imperium_config::set_user_setting(
        state.paths(),
        &key,
        &Value::from(port),
        std::env::vars(),
    );
    match saved {
        Ok(_) => {
            if let Err(error) = crate::onboarding::apply_saved(state, |next, fresh| {
                next.api.clone_from(&fresh.api);
            }) {
                tracing::warn!(error = %error.message, "could not apply the API port");
            }
        }
        Err(error) => tracing::warn!(%error, "could not save the API port"),
    }
}

fn router(api: Arc<ApiState>) -> Router {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/models/{*id}", get(get_model))
        .route("/v1/chat/completions", post(chat::chat_completions))
        .fallback(no_route)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&api),
            require_token,
        ))
        .with_state(api)
}

/// Every route needs the token, the fallback included.
async fn require_token(State(api): State<Arc<ApiState>>, request: Request, next: Next) -> Response {
    let Some(daemon) = api.daemon.upgrade() else {
        return ApiError::shutting_down().into_response();
    };
    let allowed = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| daemon.openai().accepts(value));
    if !allowed {
        return ApiError::unauthorized().into_response();
    }
    next.run(request).await
}

async fn no_route(request: Request) -> Response {
    ApiError::no_route(request.uri().path()).into_response()
}

async fn list_models(State(api): State<Arc<ApiState>>) -> Response {
    let Some(daemon) = api.daemon.upgrade() else {
        return ApiError::shutting_down().into_response();
    };
    let created = created(&daemon);
    let data: Vec<Value> = models::candidates(&daemon)
        .into_iter()
        .filter(|candidate| candidate.exposed)
        .map(|candidate| model_object(&candidate, created))
        .collect();
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn get_model(State(api): State<Arc<ApiState>>, Path(id): Path<String>) -> Response {
    let Some(daemon) = api.daemon.upgrade() else {
        return ApiError::shutting_down().into_response();
    };
    match models::find(&daemon, &id) {
        Ok(candidate) => Json(model_object(&candidate, created(&daemon))).into_response(),
        Err(hint) => ApiError::model_not_found(&id, &hint).into_response(),
    }
}

/// When the server's models count as created: the daemon's start.
fn created(daemon: &DaemonState) -> i64 {
    let uptime = i64::try_from(daemon.uptime_ms() / 1000).unwrap_or(0);
    chrono::Utc::now().timestamp() - uptime
}

fn model_object(candidate: &models::Candidate, created: i64) -> Value {
    json!({
        "id": candidate.id,
        "object": "model",
        "created": created,
        "owned_by": if candidate.hosted { candidate.endpoint.as_str() } else { "imperium" },
        // OpenAI's model objects have no display name; clients that show one read this.
        "name": candidate.name,
    })
}

// ---- the RPC side: api.get, api.set, api.token ----

fn invalid(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message.into())
}

/// `api.get`.
pub(crate) fn status(state: &DaemonState) -> ApiStatus {
    let settings = state.settings();
    let config = &settings.api.openai_compatible;
    let address = state.openai().address();
    ApiStatus {
        enabled: config.enabled,
        running: address.is_some(),
        bind: config.bind.clone(),
        port: address.map_or(config.port, |address| address.port()),
        allow_lan: config.allow_lan,
        base_url: address.map(|address| {
            let host = if address.ip().is_unspecified() {
                "127.0.0.1".to_string()
            } else {
                address.ip().to_string()
            };
            let host = if host.contains(':') {
                format!("[{host}]")
            } else {
                host
            };
            format!("http://{host}:{}/v1", address.port())
        }),
        expose: config.expose.clone(),
        error: state.openai().error(),
        models: models::candidates(state)
            .into_iter()
            .map(|candidate| ApiModel {
                id: candidate.id,
                name: candidate.name,
                hosted: candidate.hosted,
                exposed: candidate.exposed,
            })
            .collect(),
    }
}

/// `api.set`: saves what changed to the user config and applies it at once.
pub(crate) async fn set(
    state: Arc<DaemonState>,
    params: ApiSetParams,
) -> Result<ApiStatus, ErrorObject> {
    let current = state.settings();
    let now: &OpenAiApiSettings = &current.api.openai_compatible;
    let bind = params.bind.clone().unwrap_or_else(|| now.bind.clone());
    let allow_lan = params.allow_lan.unwrap_or(now.allow_lan);
    listen_ip(&bind, allow_lan).map_err(invalid)?;
    if let Some(expose) = &params.expose {
        check_expose(&current, expose)?;
    }
    let mut writes: Vec<(&str, Value)> = Vec::new();
    if let Some(expose) = &params.expose {
        writes.push(("expose", json!(expose)));
    }
    if let Some(allow_lan) = params.allow_lan {
        writes.push(("allow_lan", json!(allow_lan)));
    }
    if let Some(bind) = &params.bind {
        writes.push(("bind", json!(bind.trim())));
    }
    if let Some(port) = params.port {
        writes.push(("port", json!(port)));
    }
    if let Some(enabled) = params.enabled {
        writes.push(("enabled", json!(enabled)));
    }
    for (key, value) in writes {
        imperium_config::set_user_setting(
            state.paths(),
            &format!("{KEY_PREFIX}{key}"),
            &value,
            std::env::vars(),
        )
        .map_err(|error| invalid(error.to_string()))?;
    }
    crate::onboarding::apply_saved(&state, |next, fresh| next.api.clone_from(&fresh.api))?;
    tracing::info!("a client changed the OpenAI-compatible API settings");
    // A server that cannot start is reported in the status, not as a failed request.
    let _ = reconcile(&state).await;
    Ok(status(&state))
}

/// Each exposed spec must be a hosted row of the model table: that is the opt-in being made.
fn check_expose(settings: &Settings, expose: &[String]) -> Result<(), ErrorObject> {
    for spec in expose {
        let row = settings
            .models
            .catalog
            .iter()
            .find(|entry| &entry.spec == spec)
            .ok_or_else(|| invalid(format!("{spec} is not a row of the model table")))?;
        let (endpoint, _) = row.spec.split_once(':').unwrap_or((&row.spec, ""));
        if models::is_local_endpoint(endpoint) {
            return Err(invalid(format!(
                "{spec} is a local model; local models are served without being listed"
            )));
        }
    }
    Ok(())
}

/// `api.token`.
pub(crate) fn token(
    state: &DaemonState,
    params: ApiTokenParams,
) -> Result<ApiTokenResult, ErrorObject> {
    let token = state
        .openai()
        .token(&**state.secrets(), params.rotate)
        .map_err(|message| ErrorObject::new(codes::INTERNAL_ERROR, message))?;
    if params.rotate {
        tracing::info!("a client rotated the OpenAI-compatible API token");
    }
    Ok(ApiTokenResult { token })
}

/// Whether a setting key belongs to the server.
pub(crate) fn is_api_setting(key: &str) -> bool {
    key.starts_with(KEY_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_addresses_are_accepted_unless_the_lan_is_allowed() {
        assert_eq!(
            listen_ip("127.0.0.1", false),
            Ok(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
        assert!(listen_ip("::1", false).is_ok());
        assert!(listen_ip("localhost", false).is_ok());
        let refused = listen_ip("0.0.0.0", false).unwrap_err();
        assert!(refused.contains("allow_lan"), "{refused}");
        assert!(listen_ip("192.168.1.20", false).is_err());
        assert!(listen_ip("192.168.1.20", true).is_ok());
        assert!(listen_ip("not an address", true).is_err());
    }

    #[test]
    fn tokens_are_made_once_kept_in_the_secret_store_and_rotated() {
        let secrets = imperium_config::MemorySecrets::new();
        let api = OpenAiApi::default();
        let first = api.token(&secrets, false).unwrap();
        assert!(first.starts_with("imp-") && first.len() > 40);
        assert_eq!(api.token(&secrets, false).unwrap(), first);
        // A new process reads the stored one.
        assert_eq!(OpenAiApi::default().token(&secrets, false).unwrap(), first);
        assert!(api.accepts(&format!("Bearer {first}")));
        let second = api.token(&secrets, true).unwrap();
        assert_ne!(second, first);
        assert!(api.accepts(&format!("Bearer {second}")));
        assert!(!api.accepts(&format!("Bearer {first}")));
        assert!(!api.accepts(&second), "the Bearer scheme is required");
        assert!(!OpenAiApi::default().accepts(&format!("Bearer {second}")));
    }
}
