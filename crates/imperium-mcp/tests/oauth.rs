//! OAuth sign-in against a fake authorization server and a fake MCP server, all on loopback
//! addresses of this machine. Nothing here reaches the real network.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use imperium_mcp::oauth::{self, CredentialStore, OAuthSource};
use imperium_mcp::{Launch, McpServers, ServerSpec, TokenSource};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// ---- the fake world ----

#[derive(Default)]
struct Inner {
    as_base: String,
    mcp_base: String,
    evil_base: String,
    // authorization server
    codes: HashMap<String, String>,
    refresh: String,
    issued: u32,
    expires_in: u64,
    tamper_state: bool,
    no_s256: bool,
    no_registration: bool,
    refresh_fails: bool,
    as_requests: Vec<String>,
    registrations: Vec<Value>,
    last_authorize: HashMap<String, String>,
    revoked: Vec<String>,
    // MCP server
    valid: HashSet<String>,
    auth_headers: Vec<Option<String>>,
    mcp_redirect: bool,
    prm_resource: Option<String>,
    prm_omit_resource: bool,
    issuer_override: Option<String>,
    // a third origin that must never see anything
    evil_hits: u32,
}

#[derive(Default)]
struct Fake(Mutex<Inner>);

impl Fake {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap()
    }
}

type Shared = Arc<Fake>;

fn s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    let bytes: Vec<u8> = digest.iter().copied().collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn form(body: &str) -> HashMap<String, String> {
    reqwest::Url::parse(&format!("http://form.invalid/?{body}"))
        .unwrap()
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "invalid_grant", "error_description": "no"})),
    )
        .into_response()
}

async fn as_metadata(State(fake): State<Shared>) -> Json<Value> {
    let mut inner = fake.lock();
    inner.as_requests.push("metadata".into());
    let base = inner.as_base.clone();
    let mut document = json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "revocation_endpoint": format!("{base}/revoke"),
        "registration_endpoint": format!("{base}/register"),
        "code_challenge_methods_supported": ["S256"],
    });
    if let Some(issuer) = &inner.issuer_override {
        document["issuer"] = json!(issuer);
    }
    if inner.no_s256 {
        document["code_challenge_methods_supported"] = json!(["plain"]);
    }
    if inner.no_registration {
        document
            .as_object_mut()
            .unwrap()
            .remove("registration_endpoint");
    }
    Json(document)
}

async fn as_register(State(fake): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut inner = fake.lock();
    inner.as_requests.push("register".into());
    inner.registrations.push(body);
    (StatusCode::CREATED, Json(json!({"client_id": "client-1"}))).into_response()
}

async fn as_authorize(
    State(fake): State<Shared>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let mut inner = fake.lock();
    inner.as_requests.push("authorize".into());
    if params.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let code = format!("code-{}", inner.codes.len() + 1);
    inner
        .codes
        .insert(code.clone(), params["code_challenge"].clone());
    let state = if inner.tamper_state {
        "forged".to_string()
    } else {
        params["state"].clone()
    };
    let location = format!("{}?code={code}&state={state}", params["redirect_uri"]);
    inner.last_authorize = params;
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

fn issue(inner: &mut Inner) -> Response {
    inner.issued += 1;
    let access = format!("at-{}", inner.issued);
    let refresh = format!("rt-{}", inner.issued);
    inner.valid.insert(access.clone());
    inner.refresh = refresh.clone();
    Json(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": inner.expires_in,
        "refresh_token": refresh,
    }))
    .into_response()
}

async fn as_token(State(fake): State<Shared>, body: String) -> Response {
    let params = form(&body);
    let mut inner = fake.lock();
    let grant = params.get("grant_type").cloned().unwrap_or_default();
    inner.as_requests.push(format!("token:{grant}"));
    match grant.as_str() {
        "authorization_code" => {
            let Some(challenge) = params.get("code").and_then(|code| inner.codes.remove(code))
            else {
                return invalid_grant();
            };
            let verifier = params.get("code_verifier").cloned().unwrap_or_default();
            // The authorization server checks what a real one would.
            let resource_ok = params.get("resource") == inner.last_authorize.get("resource");
            if s256(&verifier) != challenge || !resource_ok {
                return invalid_grant();
            }
            issue(&mut inner)
        }
        "refresh_token" => {
            if inner.refresh_fails || params.get("refresh_token") != Some(&inner.refresh) {
                return invalid_grant();
            }
            issue(&mut inner)
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn as_revoke(State(fake): State<Shared>, body: String) -> StatusCode {
    let params = form(&body);
    let mut inner = fake.lock();
    inner.as_requests.push("revoke".into());
    inner
        .revoked
        .push(params.get("token").cloned().unwrap_or_default());
    StatusCode::OK
}

async fn mcp_protected_resource(State(fake): State<Shared>) -> Json<Value> {
    let inner = fake.lock();
    let resource = inner
        .prm_resource
        .clone()
        .unwrap_or_else(|| format!("{}/mcp", inner.mcp_base));
    let mut document = json!({"resource": resource, "authorization_servers": [inner.as_base]});
    if inner.prm_omit_resource {
        document.as_object_mut().unwrap().remove("resource");
    }
    Json(document)
}

fn answer(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let result = match request["method"].as_str()? {
        "initialize" => json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                               "serverInfo": {"name": "fake", "version": "1"}}),
        "tools/list" => json!({"tools": [{"name": "echo", "description": "Echo text"}]}),
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}}),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

async fn mcp_post(State(fake): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let mut inner = fake.lock();
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    inner.auth_headers.push(auth.clone());
    if inner.mcp_redirect {
        let location = format!("{}/capture", inner.evil_base);
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, location)],
        )
            .into_response();
    }
    let allowed = auth
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| inner.valid.contains(token));
    if !allowed {
        let challenge = format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
            inner.mcp_base
        );
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, challenge)],
        )
            .into_response();
    }
    let request: Value = serde_json::from_str(&body).unwrap();
    match answer(&request) {
        Some(reply) => Json(reply).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn evil_capture(State(fake): State<Shared>) -> StatusCode {
    fake.lock().evil_hits += 1;
    StatusCode::OK
}

// ---- credentials in memory ----

#[derive(Debug, Default)]
struct MemoryCredentials(Mutex<HashMap<String, String>>);

impl CredentialStore for MemoryCredentials {
    fn load(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err("empty".into());
        }
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<bool, String> {
        Ok(self.0.lock().unwrap().remove(key).is_some())
    }
}

struct Env {
    fake: Shared,
    mcp_url: String,
    memory: Arc<MemoryCredentials>,
    store: Arc<dyn CredentialStore>,
}

async fn listener() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

async fn setup() -> Env {
    let fake: Shared = Arc::default();
    let (as_listener, as_base) = listener().await;
    let (mcp_listener, mcp_base) = listener().await;
    let (evil_listener, evil_base) = listener().await;
    {
        let mut inner = fake.lock();
        inner.as_base = as_base;
        inner.mcp_base = mcp_base.clone();
        inner.evil_base = evil_base;
        inner.expires_in = 3600;
    }
    let authorization = Router::new()
        .route("/.well-known/oauth-authorization-server", get(as_metadata))
        .route("/register", post(as_register))
        .route("/authorize", get(as_authorize))
        .route("/token", post(as_token))
        .route("/revoke", post(as_revoke))
        .with_state(Arc::clone(&fake));
    let mcp = Router::new()
        .route("/mcp", post(mcp_post))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(mcp_protected_resource),
        )
        .with_state(Arc::clone(&fake));
    let evil = Router::new()
        .route("/capture", post(evil_capture).get(evil_capture))
        .with_state(Arc::clone(&fake));
    for (listener, router) in [
        (as_listener, authorization),
        (mcp_listener, mcp),
        (evil_listener, evil),
    ] {
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
    }
    let memory = Arc::new(MemoryCredentials::default());
    let store: Arc<dyn CredentialStore> = memory.clone();
    Env {
        fake,
        mcp_url: format!("{mcp_base}/mcp"),
        memory,
        store,
    }
}

impl Env {
    /// What the user does: opens the address in a browser, which the authorization server sends
    /// back to the loopback listener.
    async fn browser(&self, address: &str) {
        reqwest::get(address).await.unwrap();
    }

    /// A full sign-in.
    async fn sign_in(&self, server: &str) {
        let pending = oauth::begin(server, &self.mcp_url, Arc::clone(&self.store))
            .await
            .unwrap();
        let address = pending.authorization_url().to_string();
        let finishing = tokio::spawn(pending.finish());
        self.browser(&address).await;
        finishing.await.unwrap().unwrap();
    }

    fn auths(&self, server: &str) -> HashMap<String, Arc<dyn TokenSource>> {
        let source: Arc<dyn TokenSource> =
            Arc::new(OAuthSource::new(server, Arc::clone(&self.store)));
        HashMap::from([(server.to_string(), source)])
    }

    fn spec(&self, server: &str) -> ServerSpec {
        ServerSpec {
            name: server.to_string(),
            launch: Launch::Http {
                url: self.mcp_url.clone(),
                headers: Vec::new(),
            },
            allow: Vec::new(),
            timeout: Duration::from_secs(10),
            sampling: false,
        }
    }

    /// Makes the stored access token look expired.
    fn expire(&self, server: &str) {
        let key = format!("mcp.{server}.oauth.meta");
        let mut meta: Value =
            serde_json::from_str(&self.memory.load(&key).unwrap().unwrap()).unwrap();
        meta["expires_at"] = json!(0);
        self.memory.store(&key, &meta.to_string()).unwrap();
    }

    fn stored(&self, server: &str, part: &str) -> Option<String> {
        self.memory
            .load(&format!("mcp.{server}.oauth.{part}"))
            .unwrap()
    }

    /// Whether none of the three token entries is stored (the client registration may be).
    fn no_tokens(&self, server: &str) -> bool {
        oauth::credential_keys(server)
            .iter()
            .all(|key| self.memory.load(key).unwrap().is_none())
    }

    fn as_requests(&self) -> Vec<String> {
        self.fake.lock().as_requests.clone()
    }
}

// ---- tests ----

#[tokio::test]
async fn signing_in_stores_tokens_and_the_server_then_accepts_them() {
    let env = setup().await;
    let pending = oauth::begin("remote", &env.mcp_url, Arc::clone(&env.store))
        .await
        .unwrap();
    let address = pending.authorization_url().to_string();

    // The address carries what the specification asks of an authorization request.
    let parsed = reqwest::Url::parse(&address).unwrap();
    let query: HashMap<String, String> = parsed
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(
        parsed.origin().ascii_serialization(),
        env.fake.lock().as_base
    );
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "client-1");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["resource"], env.mcp_url);
    assert!(query["state"].len() >= 32 && query["code_challenge"].len() >= 43);
    assert!(
        query["redirect_uri"].starts_with("http://127.0.0.1:")
            && query["redirect_uri"].ends_with("/callback"),
        "{}",
        query["redirect_uri"]
    );

    let finishing = tokio::spawn(pending.finish());
    env.browser(&address).await;
    finishing.await.unwrap().unwrap();

    // Imperium registered itself as a public native client with that loopback address.
    {
        let inner = env.fake.lock();
        let registration = &inner.registrations[0];
        assert_eq!(registration["token_endpoint_auth_method"], "none");
        assert_eq!(registration["redirect_uris"][0], query["redirect_uri"]);
        // The authorization server saw the PKCE verifier hash to the challenge (it checks).
        assert_eq!(
            inner.as_requests.last().map(String::as_str),
            Some("token:authorization_code")
        );
    }
    assert!(oauth::is_signed_in(
        env.memory.as_ref(),
        "remote",
        &env.mcp_url
    ));
    assert_eq!(env.stored("remote", "access").as_deref(), Some("at-1"));
    assert_eq!(env.stored("remote", "refresh").as_deref(), Some("rt-1"));
    let meta = env.stored("remote", "meta").unwrap();
    assert!(
        !meta.contains("at-1") && !meta.contains("rt-1"),
        "tokens stay out of the metadata"
    );

    let (servers, failed) =
        McpServers::connect_with(vec![env.spec("remote")], env.auths("remote")).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(servers.tools().count(), 1);
    let headers = env.fake.lock().auth_headers.clone();
    assert_eq!(headers.last().unwrap().as_deref(), Some("Bearer at-1"));
}

#[tokio::test]
async fn a_response_with_the_wrong_state_is_refused_and_the_listener_keeps_waiting() {
    let env = setup().await;
    env.fake.lock().tamper_state = true;
    let pending = oauth::begin("remote", &env.mcp_url, Arc::clone(&env.store))
        .await
        .unwrap();
    let address = pending.authorization_url().to_string();
    let port = reqwest::Url::parse(&address)
        .unwrap()
        .query_pairs()
        .find(|(name, _)| name == "redirect_uri")
        .map(|(_, value)| reqwest::Url::parse(&value).unwrap().port().unwrap())
        .unwrap();
    let mut finishing = tokio::spawn(pending.finish());

    // The forged response is answered 400 and gets no further.
    env.browser(&address).await;
    let waiting = tokio::time::timeout(Duration::from_millis(300), &mut finishing).await;
    assert!(
        waiting.is_err(),
        "a forged response must not end the sign-in"
    );
    assert!(
        !env.as_requests()
            .iter()
            .any(|request| request.starts_with("token:"))
    );
    assert!(env.no_tokens("remote"));

    // A stray request does not end it either.
    let stray = reqwest::get(format!("http://127.0.0.1:{port}/favicon.ico"))
        .await
        .unwrap();
    assert_eq!(stray.status(), 404);
    let waiting = tokio::time::timeout(Duration::from_millis(200), &mut finishing).await;
    assert!(waiting.is_err());

    // Only the response with the right state finishes it.
    env.fake.lock().tamper_state = false;
    env.browser(&address).await;
    finishing.await.unwrap().unwrap();
    assert!(oauth::is_signed_in(
        env.memory.as_ref(),
        "remote",
        &env.mcp_url
    ));
    let token_requests = env
        .as_requests()
        .iter()
        .filter(|request| request.starts_with("token:"))
        .count();
    assert_eq!(token_requests, 1);
}

#[tokio::test]
async fn an_authorization_server_without_s256_or_registration_is_refused() {
    let env = setup().await;
    env.fake.lock().no_s256 = true;
    let error = oauth::begin("remote", &env.mcp_url, Arc::clone(&env.store))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(error.0.contains("PKCE"), "{error}");

    let env = setup().await;
    env.fake.lock().no_registration = true;
    let error = oauth::begin("remote", &env.mcp_url, Arc::clone(&env.store))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(error.0.contains("dynamic client registration"), "{error}");
}

#[tokio::test]
async fn resource_metadata_for_another_server_is_refused() {
    let env = setup().await;
    env.fake.lock().prm_resource = Some("http://127.0.0.1:1/mcp".into());
    let error = oauth::begin("remote", &env.mcp_url, Arc::clone(&env.store))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(error.0.contains("not this server"), "{error}");
}

#[tokio::test]
async fn metadata_that_names_no_resource_or_the_wrong_issuer_is_refused() {
    let env = setup().await;
    env.fake.lock().prm_omit_resource = true;
    let error = oauth::begin("remote", &env.mcp_url, Arc::clone(&env.store))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(error.0.contains("names no resource"), "{error}");

    let env = setup().await;
    env.fake.lock().issuer_override = Some("http://127.0.0.1:1".into());
    let error = oauth::begin("remote", &env.mcp_url, Arc::clone(&env.store))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(error.0.contains("issuer"), "{error}");
    assert!(
        env.as_requests()
            .iter()
            .all(|request| request != "register")
    );
}

#[tokio::test]
async fn two_sessions_refreshing_at_once_both_get_a_token_and_the_sign_in_survives() {
    let env = setup().await;
    env.sign_in("remote").await;
    env.expire("remote");
    // Two separate sources over one store: what two processes (or two sessions of an older
    // daemon) would be. The authorization server rotates the refresh token, so one of the two
    // sends a token that was just replaced; it must look again, not sign the server out.
    let a = OAuthSource::new("remote", Arc::clone(&env.store));
    let b = OAuthSource::new("remote", Arc::clone(&env.store));
    let (first, second) = tokio::join!(a.token(&env.mcp_url, false), b.token(&env.mcp_url, false));
    assert_eq!(first.as_deref(), Some("at-2"));
    assert_eq!(second.as_deref(), Some("at-2"));
    assert!(oauth::is_signed_in(
        env.memory.as_ref(),
        "remote",
        &env.mcp_url
    ));
    assert_eq!(env.stored("remote", "refresh").as_deref(), Some("rt-2"));
}

#[tokio::test]
async fn one_shared_source_refreshes_once_for_concurrent_callers() {
    let env = setup().await;
    env.sign_in("remote").await;
    env.expire("remote");
    let source = Arc::new(OAuthSource::new("remote", Arc::clone(&env.store)));
    let (first, second) = tokio::join!(
        source.token(&env.mcp_url, false),
        source.token(&env.mcp_url, false)
    );
    assert_eq!(first.as_deref(), Some("at-2"));
    assert_eq!(second.as_deref(), Some("at-2"));
    let refreshes = env
        .as_requests()
        .iter()
        .filter(|request| request.as_str() == "token:refresh_token")
        .count();
    assert_eq!(refreshes, 1);
}

#[tokio::test]
async fn an_expiring_token_is_refreshed_and_a_rotated_refresh_token_kept() {
    let env = setup().await;
    env.sign_in("remote").await;
    let source = OAuthSource::new("remote", Arc::clone(&env.store));
    let refreshes = |env: &Env| {
        env.as_requests()
            .iter()
            .filter(|request| request.as_str() == "token:refresh_token")
            .count()
    };

    assert_eq!(
        source.token(&env.mcp_url, false).await.as_deref(),
        Some("at-1")
    );
    assert_eq!(refreshes(&env), 0, "a fresh token is not refreshed");

    env.expire("remote");
    assert_eq!(
        source.token(&env.mcp_url, false).await.as_deref(),
        Some("at-2")
    );
    assert_eq!(
        env.stored("remote", "refresh").as_deref(),
        Some("rt-2"),
        "rotation"
    );
    assert_eq!(
        source.token(&env.mcp_url, false).await.as_deref(),
        Some("at-2")
    );
    assert_eq!(refreshes(&env), 1);

    // The server stops accepting the token: one forced refresh, then the call goes through.
    env.fake.lock().valid.remove("at-2");
    let (servers, failed) =
        McpServers::connect_with(vec![env.spec("remote")], env.auths("remote")).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(servers.tools().count(), 1);
    assert_eq!(refreshes(&env), 2);
    let headers = env.fake.lock().auth_headers.clone();
    assert_eq!(headers.last().unwrap().as_deref(), Some("Bearer at-3"));
}

#[tokio::test]
async fn a_refresh_token_the_server_rejects_signs_the_server_out() {
    let env = setup().await;
    env.sign_in("remote").await;
    env.expire("remote");
    env.fake.lock().refresh_fails = true;
    let source = OAuthSource::new("remote", Arc::clone(&env.store));
    assert_eq!(source.token(&env.mcp_url, false).await, None);
    assert!(!oauth::is_signed_in(
        env.memory.as_ref(),
        "remote",
        &env.mcp_url
    ));
    assert!(env.no_tokens("remote"));
}

#[tokio::test]
async fn an_unsigned_server_reports_needs_sign_in_without_starting_one() {
    let env = setup().await;
    let (servers, failed) =
        McpServers::connect_with(vec![env.spec("remote")], env.auths("remote")).await;
    assert_eq!(servers.tools().count(), 0);
    assert_eq!(failed.len(), 1);
    assert!(
        failed[0].reason.contains("needs sign-in"),
        "{}",
        failed[0].reason
    );
    assert!(
        failed[0].reason.contains("imperium mcp login remote"),
        "{}",
        failed[0].reason
    );
    // Connecting never contacted the authorization server, let alone registered or authorized.
    assert!(env.as_requests().is_empty(), "{:?}", env.as_requests());
    assert!(env.memory.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tokens_are_never_offered_to_another_origin() {
    let env = setup().await;
    env.sign_in("remote").await;
    let source = OAuthSource::new("remote", Arc::clone(&env.store));
    // Not another origin, nor another path on the same one.
    assert_eq!(source.token("http://127.0.0.1:1/mcp", false).await, None);
    let elsewhere = env.mcp_url.replace("/mcp", "/other");
    assert_eq!(source.token(&elsewhere, false).await, None);
    assert!(!oauth::is_signed_in(
        env.memory.as_ref(),
        "remote",
        &elsewhere
    ));

    // A server that redirects the request to another origin does not get the token there.
    env.fake.lock().mcp_redirect = true;
    let (servers, failed) =
        McpServers::connect_with(vec![env.spec("remote")], env.auths("remote")).await;
    assert_eq!(servers.tools().count(), 0);
    assert_eq!(failed.len(), 1);
    assert!(failed[0].reason.contains("307"), "{}", failed[0].reason);
    assert_eq!(
        env.fake.lock().evil_hits,
        0,
        "the redirect was not followed"
    );
}

#[tokio::test]
async fn signing_out_revokes_the_token_forgets_the_tokens_and_keeps_the_registration() {
    let env = setup().await;
    env.sign_in("remote").await;
    let source = OAuthSource::new("remote", Arc::clone(&env.store));
    assert!(source.logout().await.unwrap());
    assert_eq!(env.fake.lock().revoked, ["rt-1"]);
    assert!(env.no_tokens("remote"));
    assert!(!source.logout().await.unwrap());

    // The next sign-in reuses the registration (on the port it was made for, when that is free).
    env.sign_in("remote").await;
    let registrations = env.fake.lock().registrations.len();
    assert!(cfg!(windows) || registrations == 1, "{registrations}");

    // Removing the server revokes and forgets everything, the registration too.
    let revocation = oauth::forget_for_removal(env.memory.as_ref(), "remote")
        .unwrap()
        .unwrap();
    revocation.send().await;
    assert_eq!(env.fake.lock().revoked.len(), 2);
    assert!(env.memory.0.lock().unwrap().is_empty());
}
