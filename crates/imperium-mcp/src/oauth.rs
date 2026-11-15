//! OAuth for remote (HTTP) MCP servers, following the MCP authorization specification.
//!
//! The pieces:
//!
//! - **Discovery.** A server that wants a sign-in answers 401 with a `WWW-Authenticate`
//!   challenge. From it (or from the well-known locations) the client reads the protected
//!   resource metadata (RFC 9728), then the authorization server's metadata (RFC 8414). The
//!   server must advertise PKCE with `S256`, or the sign-in is refused.
//! - **Registration.** Imperium registers itself as a public native client (RFC 7591) at the
//!   authorization server's registration endpoint. A server without one is not supported yet.
//! - **Authorization code flow with PKCE.** [`begin`] listens on `127.0.0.1` on a random port,
//!   builds the authorization URL (with `state`, the `S256` code challenge, and the `resource`
//!   indicator of RFC 8707) and returns it; the user opens it in their own browser. Imperium
//!   never drives a browser. [`PendingLogin::finish`] waits for the redirect to the loopback
//!   listener, refuses a response whose `state` differs, and exchanges the code for tokens.
//! - **Storage.** Tokens and the client registration go to a [`CredentialStore`] (the OS
//!   keychain in the daemon), keyed by server name, and are bound to the server's URL: tokens
//!   issued for one resource are never offered to any other.
//! - **Use.** [`OAuthSource`] is the [`TokenSource`] an HTTP client asks before each request. It
//!   refreshes an expiring token (keeping a rotated refresh token), and signs the server out when
//!   the authorization server rejects the refresh token for good.
//! - **Sign-out.** [`logout`] revokes the token where the server offers revocation, then forgets
//!   everything stored.
//!
//! Nothing here starts on its own: a session that finds a server unsigned reports that it needs
//! sign-in and goes on. Only [`begin`], called for a user's explicit request, starts a flow.
//! Tokens never appear in errors or logs.

use imperium_api::time::unix_secs;
use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use crate::client::{PROTOCOL_VERSION, TokenFuture, TokenSource};

/// How long a sign-in may wait for the user to finish in their browser.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 1 << 20;
/// A token expiring within this many seconds is refreshed before use.
const REFRESH_SKEW_SECS: u64 = 60;
/// No access token is trusted to live longer than this, whatever `expires_in` says.
const MAX_LIFETIME_SECS: u64 = 365 * 24 * 3600;
const CALLBACK_PATH: &str = "/callback";
const CLIENT_NAME: &str = "Imperium";

/// Why a sign-in step failed. Never carries a token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct OAuthError(pub String);

fn fail<T>(message: impl Into<String>) -> Result<T, OAuthError> {
    Err(OAuthError(message.into()))
}

// ---- credential storage ----

/// Where sign-in state is kept: the OS keychain in the daemon, memory in tests. Keys are secret
/// names (see [`credential_keys`]).
pub trait CredentialStore: Send + Sync + fmt::Debug {
    /// Reads a value; `None` when absent.
    fn load(&self, key: &str) -> Result<Option<String>, String>;
    /// Stores or replaces a value.
    fn store(&self, key: &str, value: &str) -> Result<(), String>;
    /// Removes a value; whether one existed.
    fn delete(&self, key: &str) -> Result<bool, String>;
}

fn key(server: &str, part: &str) -> String {
    format!("mcp.{server}.oauth.{part}")
}

/// The three keys a server's sign-in occupies: its registration and endpoints, its access token,
/// and its refresh token. They are separate because a keychain entry may be small.
pub fn credential_keys(server: &str) -> [String; 3] {
    [
        key(server, "meta"),
        key(server, "access"),
        key(server, "refresh"),
    ]
}

fn store_error(message: String) -> OAuthError {
    OAuthError(format!("the keychain refused the sign-in state: {message}"))
}

/// The registration, endpoints and expiry of a sign-in (everything but the two tokens).
#[derive(Clone, Serialize, Deserialize)]
struct Meta {
    /// The server URL the tokens were issued for (the `resource` indicator).
    resource: String,
    issuer: String,
    token_endpoint: String,
    #[serde(default)]
    revocation_endpoint: Option<String>,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    /// When the access token expires, in Unix seconds.
    expires_at: u64,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Clone)]
struct Stored {
    meta: Meta,
    access: Option<String>,
    refresh: Option<String>,
}

/// The client registration kept for reuse: the next sign-in to the same authorization server
/// reuses it (and the port it was registered with) instead of registering again.
#[derive(Serialize, Deserialize)]
struct ClientRecord {
    issuer: String,
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
    port: u16,
}

fn load_client(store: &dyn CredentialStore, server: &str) -> Option<ClientRecord> {
    let text = store.load(&key(server, "client")).ok().flatten()?;
    serde_json::from_str(&text).ok()
}

fn load(store: &dyn CredentialStore, server: &str) -> Result<Option<Stored>, OAuthError> {
    let Some(meta) = store.load(&key(server, "meta")).map_err(store_error)? else {
        return Ok(None);
    };
    let meta: Meta = serde_json::from_str(&meta).map_err(|error| {
        OAuthError(format!(
            "the stored sign-in for {server} is unreadable: {error}"
        ))
    })?;
    let access = store.load(&key(server, "access")).map_err(store_error)?;
    let refresh = store.load(&key(server, "refresh")).map_err(store_error)?;
    Ok(Some(Stored {
        meta,
        access,
        refresh,
    }))
}

fn save(store: &dyn CredentialStore, server: &str, stored: &Stored) -> Result<(), OAuthError> {
    let meta = serde_json::to_string(&stored.meta)
        .map_err(|error| OAuthError(format!("could not encode the sign-in: {error}")))?;
    match &stored.access {
        Some(access) => store
            .store(&key(server, "access"), access)
            .map_err(store_error)?,
        None => {
            store.delete(&key(server, "access")).map_err(store_error)?;
        }
    }
    match &stored.refresh {
        Some(refresh) => store
            .store(&key(server, "refresh"), refresh)
            .map_err(store_error)?,
        None => {
            store.delete(&key(server, "refresh")).map_err(store_error)?;
        }
    }
    store
        .store(&key(server, "meta"), &meta)
        .map_err(store_error)
}

/// Removes everything stored for `server`. Returns whether anything was there.
pub fn forget(store: &dyn CredentialStore, server: &str) -> Result<bool, OAuthError> {
    let mut found = false;
    for name in credential_keys(server) {
        found |= store.delete(&name).map_err(store_error)?;
    }
    Ok(found)
}

/// Removes everything stored for `server`, including the client registration kept for reuse.
pub fn forget_all(store: &dyn CredentialStore, server: &str) -> Result<bool, OAuthError> {
    let found = forget(store, server)?;
    let registration = store.delete(&key(server, "client")).map_err(store_error)?;
    Ok(found || registration)
}

/// Whether `server` has a sign-in stored for exactly `url` (tokens for another URL do not count).
pub fn is_signed_in(store: &dyn CredentialStore, server: &str, url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    match load(store, server) {
        Ok(Some(stored)) => {
            stored.meta.resource == canonical(&parsed)
                && (stored.access.is_some() || stored.refresh.is_some())
        }
        _ => false,
    }
}

/// When a token that lives `expires_in` seconds expires (clamped, never overflowing).
fn expiry(expires_in: u64) -> u64 {
    unix_secs().saturating_add(expires_in.min(MAX_LIFETIME_SECS))
}

// ---- URLs ----

fn host_ip(url: &Url) -> Option<IpAddr> {
    url.host_str()?.trim_matches(['[', ']']).parse().ok()
}

/// Loopback, private, link-local, unspecified, broadcast, `0.0.0.0/8` and shared (CGNAT,
/// `100.64.0.0/10`) IPv4 addresses.
fn local_v4(ip: std::net::Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || octets[0] == 0
        || (octets[0] == 100 && octets[1] & 0xc0 == 0x40)
}

fn is_loopback(url: &Url) -> bool {
    url.host_str() == Some("localhost") || host_ip(url).is_some_and(|ip| ip.is_loopback())
}

/// Loopback, private, link-local or unspecified addresses (and `localhost`).
fn is_local(url: &Url) -> bool {
    if url.host_str() == Some("localhost") {
        return true;
    }
    match host_ip(url) {
        Some(IpAddr::V4(ip)) => local_v4(ip),
        Some(IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            // An IPv4 address written as IPv6 (`::ffff:7f00:1`) is judged as the IPv4 address.
            ip.to_ipv4_mapped().is_some_and(local_v4)
                || ip.is_loopback()
                || ip.is_unspecified()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80
        }
        None => false,
    }
}

/// Parses `text` and accepts only `https`, or `http` to a loopback host when the server being
/// signed in to is itself local (`public` is false: development and tests), without credentials
/// in the URL. When the server is public, an address that points at the local machine or a
/// private network is refused too, so a public server cannot aim Imperium at one.
fn secure_url(text: &str, what: &str, public: bool) -> Result<Url, OAuthError> {
    let url = Url::parse(text)
        .map_err(|error| OAuthError(format!("{what} ({text}) is not a URL: {error}")))?;
    let scheme_ok =
        url.scheme() == "https" || (url.scheme() == "http" && !public && is_loopback(&url));
    if !scheme_ok {
        return fail(format!("{what} ({text}) must use https"));
    }
    if public && is_local(&url) {
        return fail(format!(
            "{what} ({text}) is a local or private address, which a public server may not name"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return fail(format!("{what} ({text}) must not carry credentials"));
    }
    Ok(url)
}

fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// The resource indicator for a server URL: the URL without a fragment or a bare trailing slash.
fn canonical(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    let mut text = url.to_string();
    if url.path() == "/" && url.query().is_none() && text.ends_with('/') {
        text.pop();
    }
    text
}

/// `<origin>/.well-known/<suffix><path>`, then without the path (RFC 9728 and RFC 8414).
fn well_known(url: &Url, suffix: &str) -> Vec<String> {
    let origin = origin_of(url);
    let path = url.path().trim_end_matches('/');
    let mut found = Vec::new();
    if !path.is_empty() {
        found.push(format!("{origin}/.well-known/{suffix}{path}"));
    }
    found.push(format!("{origin}/.well-known/{suffix}"));
    found
}

fn form_body(params: &[(&str, &str)]) -> String {
    let mut url = Url::parse("http://form.invalid/").expect("a constant URL parses");
    url.query_pairs_mut().extend_pairs(params.iter().copied());
    url.query().unwrap_or_default().to_string()
}

fn http_client() -> Result<reqwest::Client, OAuthError> {
    // No redirects: an authorization server endpoint that redirects is not followed, so no
    // request body (a code or a token) is ever replayed somewhere else.
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| OAuthError(format!("could not set up HTTP: {error}")))
}

/// The body of a response, refused once it passes [`MAX_BODY`] (checked as it arrives).
async fn read_limited(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err("answered with too much data".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn fetch_json(http: &reqwest::Client, url: &str) -> Result<Value, OAuthError> {
    let response = http
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|error| OAuthError(format!("could not reach {url}: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return fail(format!("{url} answered HTTP {status}"));
    }
    let bytes = read_limited(response)
        .await
        .map_err(|error| OAuthError(format!("could not read {url}: {error}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| OAuthError(format!("{url} did not answer with JSON: {error}")))
}

/// Text from a server, safe to show: no control characters, at most 200 characters.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(200).collect()
}

// ---- the 401 challenge ----

/// What a `WWW-Authenticate: Bearer ...` challenge says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Challenge {
    /// Where the protected resource metadata is.
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

/// Reads the parameters of a `WWW-Authenticate` header value (several headers joined by commas).
pub fn parse_challenge(header: &str) -> Challenge {
    let mut challenge = Challenge::default();
    let mut rest = header;
    while let Some(equals) = rest.find('=') {
        let name = rest[..equals]
            .rsplit(|c: char| c == ',' || c.is_whitespace())
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let after = rest[equals + 1..].trim_start();
        let (value, remaining) = if let Some(quoted) = after.strip_prefix('"') {
            let mut value = String::new();
            let mut end = quoted.len();
            let mut chars = quoted.char_indices();
            while let Some((index, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => {
                        end = index + 1;
                        break;
                    }
                    _ => value.push(c),
                }
            }
            (value, &quoted[end..])
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].trim().to_string(), &after[end..])
        };
        match name.as_str() {
            "resource_metadata" => challenge.resource_metadata = Some(value),
            "scope" => challenge.scope = Some(value),
            "error" => challenge.error = Some(value),
            _ => {}
        }
        rest = remaining;
    }
    challenge
}

// ---- discovery ----

struct Endpoints {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    revocation_endpoint: Option<String>,
    scope: Option<String>,
}

/// Asks the server something with no token, to learn what it wants.
async fn probe(http: &reqwest::Client, url: &Url) -> Result<Challenge, OAuthError> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "imperium", "version": env!("CARGO_PKG_VERSION")},
        },
    });
    let response = http
        .post(url.clone())
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|error| OAuthError(format!("could not reach {url}: {error}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        let header = response
            .headers()
            .get_all("www-authenticate")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(parse_challenge(&header));
    }
    if status.is_success() {
        return fail("the server answered without asking for a sign-in");
    }
    // Some servers answer something else; the well-known locations may still say.
    Ok(Challenge::default())
}

/// Whether `path` is `prefix` or lies below it, segment by segment (`/mcp` covers `/mcp/x`, not
/// `/mcpx`).
fn path_covers(prefix: &str, path: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// The metadata must name a `resource` and it must describe this server: same origin, and a
/// path that covers it.
fn check_resource(document: &Value, resource: &Url) -> Result<(), OAuthError> {
    let Some(claimed) = document.get("resource").and_then(Value::as_str) else {
        return fail("the protected resource metadata names no resource; refusing it");
    };
    let claimed = Url::parse(claimed).map_err(|_| {
        OAuthError("the protected resource metadata names a resource that is not a URL".into())
    })?;
    let covers =
        origin_of(&claimed) == origin_of(resource) && path_covers(claimed.path(), resource.path());
    if covers {
        Ok(())
    } else {
        fail(format!(
            "the protected resource metadata is for {claimed}, not this server; refusing it"
        ))
    }
}

async fn discover(
    http: &reqwest::Client,
    resource: &Url,
    challenge: &Challenge,
    public: bool,
) -> Result<Endpoints, OAuthError> {
    let candidates = match &challenge.resource_metadata {
        Some(location) => {
            secure_url(location, "the resource metadata URL", public)?;
            vec![location.clone()]
        }
        None => well_known(resource, "oauth-protected-resource"),
    };
    let mut issuer = None;
    let mut scope = challenge.scope.clone().filter(|scope| !scope.is_empty());
    for candidate in &candidates {
        let Ok(document) = fetch_json(http, candidate).await else {
            continue;
        };
        check_resource(&document, resource)?;
        issuer = document
            .get("authorization_servers")
            .and_then(Value::as_array)
            .and_then(|servers| servers.first())
            .and_then(Value::as_str)
            .map(str::to_string);
        if scope.is_none() {
            scope = document
                .get("scopes_supported")
                .and_then(Value::as_array)
                .map(|scopes| {
                    scopes
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|scope| !scope.is_empty());
        }
        break;
    }
    // Without resource metadata the server's own origin is taken to be the authorization server
    // (the earlier revision of the specification).
    let issuer_text = issuer.unwrap_or_else(|| origin_of(resource));
    let issuer = secure_url(&issuer_text, "the authorization server", public)?;
    let mut endpoints = authorization_server(http, &issuer, &issuer_text, public).await?;
    endpoints.scope = scope;
    Ok(endpoints)
}

async fn authorization_server(
    http: &reqwest::Client,
    issuer: &Url,
    issuer_text: &str,
    public: bool,
) -> Result<Endpoints, OAuthError> {
    if issuer.query().is_some() || issuer.fragment().is_some() {
        return fail(format!(
            "the authorization server {issuer_text} carries a query or fragment; refusing it"
        ));
    }
    let origin = origin_of(issuer);
    let path = issuer.path().trim_end_matches('/');
    let candidates = if path.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    };
    let mut last = OAuthError(format!(
        "no authorization server metadata found for {issuer_text}"
    ));
    for candidate in &candidates {
        match fetch_json(http, candidate).await {
            Ok(document) => return endpoints_from(&document, issuer_text, public),
            Err(error) => last = error,
        }
    }
    Err(last)
}

fn endpoints_from(document: &Value, issuer: &str, public: bool) -> Result<Endpoints, OAuthError> {
    // The issuer the metadata names must be exactly the one it was fetched for (RFC 8414).
    if document.get("issuer").and_then(Value::as_str) != Some(issuer) {
        return fail(format!(
            "the authorization server metadata does not name {issuer} as its issuer; refusing it"
        ));
    }
    let pkce = document
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        .is_some_and(|methods| methods.iter().any(|method| method == "S256"));
    if !pkce {
        return fail(format!(
            "{issuer} does not advertise PKCE with S256, so Imperium will not sign in to it"
        ));
    }
    let endpoint = |name: &str| -> Result<Option<String>, OAuthError> {
        match document.get(name).and_then(Value::as_str) {
            Some(text) => Ok(Some(secure_url(text, name, public)?.to_string())),
            None => Ok(None),
        }
    };
    let Some(authorization_endpoint) = endpoint("authorization_endpoint")? else {
        return fail(format!("{issuer} names no authorization_endpoint"));
    };
    let Some(token_endpoint) = endpoint("token_endpoint")? else {
        return fail(format!("{issuer} names no token_endpoint"));
    };
    Ok(Endpoints {
        issuer: issuer.to_string(),
        authorization_endpoint,
        token_endpoint,
        registration_endpoint: endpoint("registration_endpoint").ok().flatten(),
        revocation_endpoint: endpoint("revocation_endpoint").ok().flatten(),
        scope: None,
    })
}

struct Registered {
    client_id: String,
    client_secret: Option<String>,
}

async fn register(
    http: &reqwest::Client,
    endpoint: &str,
    redirect: &str,
) -> Result<Registered, OAuthError> {
    let body = json!({
        "client_name": CLIENT_NAME,
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": "native",
    });
    let response = http
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|error| OAuthError(format!("could not reach {endpoint}: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return fail(format!(
            "the authorization server refused to register Imperium (HTTP {status})"
        ));
    }
    let bytes = read_limited(response)
        .await
        .map_err(|error| OAuthError(format!("could not read the registration: {error}")))?;
    let document: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let Some(client_id) = document
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return fail("the registration answered without a client_id");
    };
    Ok(Registered {
        client_id: client_id.to_string(),
        client_secret: document
            .get("client_secret")
            .and_then(Value::as_str)
            .filter(|secret| !secret.is_empty())
            .map(str::to_string),
    })
}

// ---- PKCE and randomness ----

fn random_token() -> Result<String, OAuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| OAuthError(format!("no random source for the sign-in: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The `S256` code challenge of a verifier (RFC 7636).
fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    let bytes: Vec<u8> = digest.iter().copied().collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Compares two secrets without stopping at the first difference.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |difference, (x, y)| difference | (x ^ y))
            == 0
}

// ---- token endpoint ----

struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
}

struct TokenFailure {
    /// The authorization server rejected the grant itself; trying again cannot help.
    permanent: bool,
    message: String,
}

async fn token_request(
    http: &reqwest::Client,
    endpoint: &str,
    params: &[(&str, &str)],
) -> Result<TokenResponse, TokenFailure> {
    let transient = |message: String| TokenFailure {
        permanent: false,
        message,
    };
    let response = http
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(form_body(params))
        .send()
        .await
        .map_err(|error| transient(format!("could not reach the token endpoint: {error}")))?;
    let status = response.status();
    let bytes = read_limited(response).await.map_err(|error| {
        transient(format!(
            "could not read the token endpoint's answer: {error}"
        ))
    })?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if !status.is_success() {
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let description = body
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let permanent = matches!(
            code,
            "invalid_grant" | "invalid_client" | "unauthorized_client"
        );
        let detail = if code.is_empty() {
            String::new()
        } else {
            format!(": {} {}", clean(code), clean(description))
        };
        return Err(TokenFailure {
            permanent,
            message: format!("the token endpoint answered HTTP {status}{detail}"),
        });
    }
    let Some(access_token) = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
    else {
        return Err(transient(
            "the token endpoint answered without an access token".into(),
        ));
    };
    if let Some(kind) = body.get("token_type").and_then(Value::as_str)
        && !kind.eq_ignore_ascii_case("bearer")
    {
        return Err(transient(format!(
            "the token endpoint issued a {} token, not a bearer token",
            clean(kind)
        )));
    }
    Ok(TokenResponse {
        access_token: access_token.to_string(),
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string),
        expires_in: body
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(3600),
        scope: body
            .get("scope")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

// ---- signing in ----

/// A sign-in that has been set up and waits for the user to finish it in their browser.
pub struct PendingLogin {
    server: String,
    store: Arc<dyn CredentialStore>,
    http: reqwest::Client,
    listener: TcpListener,
    port: u16,
    redirect: String,
    authorization_url: String,
    state: String,
    verifier: String,
    resource: String,
    issuer: String,
    token_endpoint: String,
    revocation_endpoint: Option<String>,
    client_id: String,
    client_secret: Option<String>,
    scope: Option<String>,
    timeout: Duration,
    /// The client registration was kept from an earlier sign-in; dropped again if this one fails.
    reused_client: bool,
}

impl fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingLogin")
            .field("server", &self.server)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// Sets up a sign-in to the server at `url`: discovers its authorization server, registers
/// Imperium there, starts listening on `127.0.0.1`, and builds the URL the user opens in their
/// browser. Nothing is stored until [`PendingLogin::finish`] succeeds.
pub async fn begin(
    server: &str,
    url: &str,
    store: Arc<dyn CredentialStore>,
) -> Result<PendingLogin, OAuthError> {
    let resource_url = secure_url(url, "the server URL", false)?;
    let public = !is_local(&resource_url);
    let http = http_client()?;
    let challenge = probe(&http, &resource_url).await?;
    let endpoints = discover(&http, &resource_url, &challenge, public).await?;
    // A registration kept from an earlier sign-in is reused, on the port it was made for, when
    // that port is free; otherwise Imperium registers again on a fresh random port.
    let mut reused = None;
    if let Some(record) =
        load_client(store.as_ref(), server).filter(|record| record.issuer == endpoints.issuer)
        && let Ok(listener) = TcpListener::bind(("127.0.0.1", record.port)).await
    {
        reused = Some((listener, record));
    }
    let reused_client = reused.is_some();
    let (listener, port, registered) = match reused {
        Some((listener, record)) => (
            listener,
            record.port,
            Registered {
                client_id: record.client_id,
                client_secret: record.client_secret,
            },
        ),
        None => {
            let Some(registration) = endpoints.registration_endpoint.clone() else {
                return fail(format!(
                    "{} does not offer dynamic client registration, and registering Imperium by \
                     hand is not supported yet",
                    endpoints.issuer
                ));
            };
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|error| OAuthError(format!("could not listen on 127.0.0.1: {error}")))?;
            let port = listener
                .local_addr()
                .map_err(|error| OAuthError(format!("could not read the listening port: {error}")))?
                .port();
            let redirect = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
            let registered = register(&http, &registration, &redirect).await?;
            let record = ClientRecord {
                issuer: endpoints.issuer.clone(),
                client_id: registered.client_id.clone(),
                client_secret: registered.client_secret.clone(),
                port,
            };
            if let Ok(text) = serde_json::to_string(&record) {
                let _ = store.store(&key(server, "client"), &text);
            }
            (listener, port, registered)
        }
    };
    let redirect = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let verifier = random_token()?;
    let state = random_token()?;
    let resource = canonical(&resource_url);
    let mut authorization = secure_url(
        &endpoints.authorization_endpoint,
        "authorization_endpoint",
        public,
    )?;
    {
        let mut query = authorization.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &registered.client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("state", &state)
            .append_pair("code_challenge", &code_challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &resource);
        if let Some(scope) = &endpoints.scope {
            query.append_pair("scope", scope);
        }
    }
    Ok(PendingLogin {
        server: server.to_string(),
        store,
        http,
        listener,
        port,
        redirect,
        authorization_url: authorization.to_string(),
        state,
        verifier,
        resource,
        issuer: endpoints.issuer,
        token_endpoint: endpoints.token_endpoint,
        revocation_endpoint: endpoints.revocation_endpoint,
        client_id: registered.client_id,
        client_secret: registered.client_secret,
        scope: endpoints.scope,
        timeout: LOGIN_TIMEOUT,
        reused_client,
    })
}

impl PendingLogin {
    /// The URL for the user to open in their browser.
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// How long [`PendingLogin::finish`] waits for the browser.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Waits for the browser to come back to the loopback listener, checks the response, trades
    /// the code for tokens, and stores them. A response with the wrong `state` is refused.
    pub async fn finish(self) -> Result<(), OAuthError> {
        let result = self.exchange().await;
        if result.is_err() && self.reused_client {
            // The authorization server may have forgotten the client; register afresh next time.
            let _ = self.store.delete(&key(&self.server, "client"));
        }
        result
    }

    async fn exchange(&self) -> Result<(), OAuthError> {
        let deadline = Instant::now() + self.timeout;
        let code = wait_for_code(
            &self.listener,
            &self.state,
            self.port,
            deadline,
            self.timeout.as_secs(),
        )
        .await?;
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", self.redirect.as_str()),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", self.verifier.as_str()),
            ("resource", self.resource.as_str()),
        ];
        if let Some(secret) = &self.client_secret {
            params.push(("client_secret", secret.as_str()));
        }
        let response = token_request(&self.http, &self.token_endpoint, &params)
            .await
            .map_err(|failure| OAuthError(format!("sign-in failed: {}", failure.message)))?;
        let stored = Stored {
            meta: Meta {
                resource: self.resource.clone(),
                issuer: self.issuer.clone(),
                token_endpoint: self.token_endpoint.clone(),
                revocation_endpoint: self.revocation_endpoint.clone(),
                client_id: self.client_id.clone(),
                client_secret: self.client_secret.clone(),
                expires_at: expiry(response.expires_in),
                scope: response.scope.clone().or_else(|| self.scope.clone()),
            },
            access: Some(response.access_token),
            refresh: response.refresh_token,
        };
        save(self.store.as_ref(), &self.server, &stored)
    }
}

enum Callback {
    /// Not a request for the redirect address.
    Ignore,
    /// A request for it with the wrong `state`: refused, and the listener goes on waiting.
    Stray,
    Code(String),
    /// The right `state`, but an error or no code: the sign-in ends.
    Refuse(String),
}

async fn wait_for_code(
    listener: &TcpListener,
    state: &str,
    port: u16,
    deadline: Instant,
    seconds: u64,
) -> Result<String, OAuthError> {
    loop {
        let accepted = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| OAuthError(format!("no sign-in finished within {seconds} s")))?;
        let (mut stream, _) = accepted
            .map_err(|error| OAuthError(format!("the sign-in listener failed: {error}")))?;
        let Some(head) = read_head(&mut stream).await else {
            continue;
        };
        match check_callback(&head, port, state) {
            Callback::Ignore => respond(&mut stream, "404 Not Found", "Not found").await,
            Callback::Stray => {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "This sign-in response was not expected and was ignored.",
                )
                .await;
            }
            Callback::Code(code) => {
                respond(
                    &mut stream,
                    "200 OK",
                    "Signed in. You can close this tab and return to Imperium.",
                )
                .await;
                return Ok(code);
            }
            Callback::Refuse(reason) => {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "Imperium refused this sign-in response. Start the sign-in again.",
                )
                .await;
                return Err(OAuthError(reason));
            }
        }
    }
}

/// The request line and headers a browser sent, or `None` when it sent nothing usable.
async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let read = async {
        loop {
            let count = stream.read(&mut chunk).await.ok()?;
            if count == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..count]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() > 8192 {
                return Some(());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .ok()
        .flatten()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n\
         cache-control: no-store\r\nreferrer-policy: no-referrer\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(text.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Decides what a request to the loopback listener is: not ours (ignored), the redirect we
/// wait for, or a redirect that must be refused (wrong `state`, an error, no code).
fn check_callback(head: &str, port: u16, state: &str) -> Callback {
    let mut lines = head.lines();
    let Some(first) = lines.next() else {
        return Callback::Ignore;
    };
    let mut parts = first.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return Callback::Ignore;
    };
    // Only the address Imperium itself gave out, so a page elsewhere cannot reach this listener
    // through a name it controls (DNS rebinding).
    let expected_host = format!("127.0.0.1:{port}");
    let host_ok = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("host") && value.trim() == expected_host
        })
    });
    if !host_ok || !target.starts_with('/') {
        return Callback::Ignore;
    }
    let Ok(url) = Url::parse(&format!("http://127.0.0.1:{port}{target}")) else {
        return Callback::Ignore;
    };
    if url.host_str() != Some("127.0.0.1") || url.path() != CALLBACK_PATH {
        return Callback::Ignore;
    }
    let query: HashMap<String, String> = url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    let given = query.get("state").map_or("", String::as_str);
    if !same_secret(given, state) {
        // Anything on the loopback address can send this; only the right state ends the flow.
        return Callback::Stray;
    }
    if let Some(error) = query.get("error") {
        return Callback::Refuse(format!(
            "the authorization server refused the sign-in: {}",
            clean(error)
        ));
    }
    match query.get("code").filter(|code| !code.is_empty()) {
        Some(code) => Callback::Code(code.clone()),
        None => Callback::Refuse("the sign-in response carried no code".into()),
    }
}

// ---- using and ending a sign-in ----

/// The [`TokenSource`] for one server: reads its stored sign-in, refreshes it when it is about
/// to expire, and never offers it to a URL other than the one it was issued for.
///
/// Keep one per server for the life of the daemon and share it between sessions: the refresh
/// lock it holds is what stops two sessions from using a rotating refresh token twice.
pub struct OAuthSource {
    server: String,
    store: Arc<dyn CredentialStore>,
    http: reqwest::Client,
    /// Held while refreshing or signing out.
    refreshing: tokio::sync::Mutex<()>,
    /// A refreshed sign-in the keychain refused to keep, used for as long as this source lives
    /// (so a rotated refresh token is not lost); it is dropped as soon as the store takes a newer
    /// one or is found empty.
    fallback: std::sync::Mutex<Option<Stored>>,
}

impl fmt::Debug for OAuthSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthSource")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

impl OAuthSource {
    pub fn new(server: &str, store: Arc<dyn CredentialStore>) -> Self {
        OAuthSource {
            server: server.to_string(),
            store,
            http: http_client().expect("an HTTP client without redirects builds"),
            refreshing: tokio::sync::Mutex::new(()),
            fallback: std::sync::Mutex::new(None),
        }
    }

    fn fallback(&self) -> std::sync::MutexGuard<'_, Option<Stored>> {
        self.fallback.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The sign-in for `resource`: the stored one, or the in-memory one when it is newer (or the
    /// keychain cannot be read).
    fn read(&self, resource: &str) -> Option<Stored> {
        match load(self.store.as_ref(), &self.server) {
            Ok(Some(stored)) if stored.meta.resource == resource => {
                let mut fallback = self.fallback();
                match fallback.take() {
                    Some(kept)
                        if kept.meta.resource == resource
                            && kept.meta.expires_at > stored.meta.expires_at =>
                    {
                        *fallback = Some(kept.clone());
                        Some(kept)
                    }
                    _ => Some(stored),
                }
            }
            Ok(_) => {
                *self.fallback() = None;
                None
            }
            Err(error) => {
                tracing::warn!(server = %self.server, %error, "could not read an MCP sign-in");
                self.fallback()
                    .clone()
                    .filter(|kept| kept.meta.resource == resource)
            }
        }
    }

    /// Writes a refreshed sign-in; if the keychain refuses, keeps it in memory and says so.
    fn persist(&self, updated: &Stored) {
        match save(self.store.as_ref(), &self.server, updated) {
            Ok(()) => *self.fallback() = None,
            Err(error) => {
                tracing::warn!(server = %self.server, %error,
                    "could not store a refreshed MCP token; keeping it in memory until the daemon restarts");
                *self.fallback() = Some(updated.clone());
            }
        }
    }

    async fn current(&self, url: &str, force: bool) -> Option<String> {
        let resource = canonical(&Url::parse(url).ok()?);
        let usable = |stored: &Stored| {
            stored.access.is_some()
                && stored.meta.expires_at > unix_secs().saturating_add(REFRESH_SKEW_SECS)
        };
        let stored = self.read(&resource)?;
        if !force && usable(&stored) {
            return stored.access;
        }
        // One refresh at a time: a rotated refresh token can be used once.
        let _turn = self.refreshing.lock().await;
        let stored = self.read(&resource)?;
        if !force && usable(&stored) {
            return stored.access;
        }
        let refresh_token = stored.refresh.clone()?;
        let mut params = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", stored.meta.client_id.as_str()),
            ("resource", stored.meta.resource.as_str()),
        ];
        if let Some(secret) = &stored.meta.client_secret {
            params.push(("client_secret", secret.as_str()));
        }
        match token_request(&self.http, &stored.meta.token_endpoint, &params).await {
            Ok(response) => {
                let mut meta = stored.meta.clone();
                meta.expires_at = expiry(response.expires_in);
                if response.scope.is_some() {
                    meta.scope = response.scope.clone();
                }
                let updated = Stored {
                    meta,
                    access: Some(response.access_token.clone()),
                    // A rotated refresh token replaces the old one; otherwise the old one stays.
                    refresh: response
                        .refresh_token
                        .clone()
                        .or_else(|| Some(refresh_token.clone())),
                };
                self.persist(&updated);
                Some(response.access_token)
            }
            Err(failure) if failure.permanent => {
                // Another session (another process, even) may have used the refresh token a
                // moment ago and stored the one that replaced it; look again before giving up,
                // and remove the sign-in only if the refresh token stored is still the one sent.
                tokio::time::sleep(Duration::from_millis(250)).await;
                match self.read(&resource) {
                    Some(again) if again.refresh.as_deref() != Some(refresh_token.as_str()) => {
                        if usable(&again) {
                            again.access
                        } else {
                            None
                        }
                    }
                    Some(_) => {
                        tracing::warn!(server = %self.server, reason = %failure.message,
                            "an MCP server's refresh token was rejected; the sign-in was removed");
                        let _ = forget(self.store.as_ref(), &self.server);
                        *self.fallback() = None;
                        None
                    }
                    None => None,
                }
            }
            Err(failure) => {
                tracing::warn!(server = %self.server, reason = %failure.message,
                    "could not refresh an MCP token");
                // The server may still accept the old token for a little while.
                if !force && stored.meta.expires_at > unix_secs() {
                    stored.access
                } else {
                    None
                }
            }
        }
    }

    /// Forgets the in-memory sign-in after a new sign-in was stored, so nothing older shadows it.
    pub async fn reset(&self) {
        let _turn = self.refreshing.lock().await;
        *self.fallback() = None;
    }

    /// For a server being removed: waits for a refresh in progress (so it cannot store tokens
    /// afterwards), then forgets everything stored, the registration too, and returns what is
    /// needed to revoke the token.
    pub async fn forget_for_removal(&self) -> Result<Option<Revocation>, OAuthError> {
        let _turn = self.refreshing.lock().await;
        *self.fallback() = None;
        forget_for_removal(self.store.as_ref(), &self.server)
    }

    /// Signs out: waits for a refresh in progress, revokes the token where the authorization
    /// server offers revocation (best effort), and forgets everything stored (keeping the client
    /// registration for the next sign-in). Returns whether a sign-in existed.
    pub async fn logout(&self) -> Result<bool, OAuthError> {
        let _turn = self.refreshing.lock().await;
        let kept = self.fallback().take().is_some();
        let existed = logout(self.store.as_ref(), &self.server).await?;
        Ok(existed || kept)
    }
}

impl TokenSource for OAuthSource {
    fn token<'a>(&'a self, url: &'a str, refresh: bool) -> TokenFuture<'a> {
        Box::pin(self.current(url, refresh))
    }
}

/// Signs out of `server`: asks the authorization server to revoke the token where it offers
/// revocation (best effort), then forgets everything stored. Returns whether a sign-in existed.
/// Prefer [`OAuthSource::logout`], which also waits for a refresh in progress.
pub async fn logout(store: &dyn CredentialStore, server: &str) -> Result<bool, OAuthError> {
    let stored = load(store, server).ok().flatten();
    if let Some(stored) = &stored {
        revoke(stored).await;
    }
    let forgotten = forget(store, server)?;
    Ok(stored.is_some() || forgotten)
}

/// A sign-in taken out of the store because its server was removed, still to be revoked.
pub struct Revocation(Stored);

impl Revocation {
    /// Asks the authorization server to revoke the token (best effort).
    pub async fn send(self) {
        revoke(&self.0).await;
    }
}

/// Forgets everything stored for a server that is being removed (the client registration too)
/// and returns what is needed to revoke its token, if there was a sign-in.
pub fn forget_for_removal(
    store: &dyn CredentialStore,
    server: &str,
) -> Result<Option<Revocation>, OAuthError> {
    let stored = load(store, server).ok().flatten();
    forget_all(store, server)?;
    Ok(stored.map(Revocation))
}

async fn revoke(stored: &Stored) {
    let Some(endpoint) = &stored.meta.revocation_endpoint else {
        return;
    };
    let public = Url::parse(&stored.meta.resource)
        .map(|resource| !is_local(&resource))
        .unwrap_or(true);
    if secure_url(endpoint, "the revocation endpoint", public).is_err() {
        return;
    }
    let (token, hint) = match (&stored.refresh, &stored.access) {
        (Some(refresh), _) => (refresh.as_str(), "refresh_token"),
        (None, Some(access)) => (access.as_str(), "access_token"),
        (None, None) => return,
    };
    let Ok(http) = http_client() else {
        return;
    };
    let mut params = vec![
        ("token", token),
        ("token_type_hint", hint),
        ("client_id", stored.meta.client_id.as_str()),
    ];
    if let Some(secret) = &stored.meta.client_secret {
        params.push(("client_secret", secret.as_str()));
    }
    let _ = http
        .post(endpoint.as_str())
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body(&params))
        .send()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_challenge_matches_the_rfc_7636_example() {
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_ne!(a, b);
        assert!(a.len() >= 43, "{a}");
    }

    #[test]
    fn challenges_are_read() {
        let challenge = parse_challenge(
            r#"Bearer realm="mcp", resource_metadata="https://a.example/.well-known/oauth-protected-resource/mcp", scope="read write", error=invalid_token"#,
        );
        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://a.example/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(challenge.scope.as_deref(), Some("read write"));
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
        assert_eq!(parse_challenge("Bearer"), Challenge::default());
    }

    #[test]
    fn resources_are_canonical_and_urls_checked() {
        let url = Url::parse("https://Example.com/mcp#frag").unwrap();
        assert_eq!(canonical(&url), "https://example.com/mcp");
        let bare = Url::parse("https://example.com").unwrap();
        assert_eq!(canonical(&bare), "https://example.com");
        assert!(secure_url("https://example.com/x", "u", true).is_ok());
        assert!(secure_url("http://127.0.0.1:9/x", "u", false).is_ok());
        assert!(secure_url("http://example.com/x", "u", false).is_err());
        assert!(secure_url("https://user:pw@example.com/x", "u", true).is_err());
        // A public server may not name loopback, private or link-local addresses, nor use http.
        assert!(secure_url("http://127.0.0.1:9/x", "u", true).is_err());
        for local in [
            "https://127.0.0.1/x",
            "https://localhost/x",
            "https://10.1.2.3/x",
            "https://192.168.0.9/x",
            "https://169.254.169.254/x",
            "https://[::1]/x",
            "https://[fe80::1]/x",
            "https://[fd00::1]/x",
            "https://[::ffff:7f00:1]/x",
            "https://[::ffff:10.0.0.1]/x",
            "https://0.1.2.3/x",
            "https://100.64.0.1/x",
        ] {
            assert!(secure_url(local, "u", true).is_err(), "{local}");
            assert!(secure_url(local, "u", false).is_ok(), "{local}");
        }
        assert!(secure_url("https://93.184.216.34/x", "u", true).is_ok());
        assert!(
            path_covers("/mcp", "/mcp") && path_covers("/mcp/", "/mcp/x") && path_covers("", "/a")
        );
        assert!(!path_covers("/mcp", "/mcpx") && !path_covers("/mcp", "/other"));
        assert!(expiry(u64::MAX) + 1 >= unix_secs() + MAX_LIFETIME_SECS);
        assert_eq!(
            well_known(&url, "oauth-protected-resource"),
            [
                "https://example.com/.well-known/oauth-protected-resource/mcp",
                "https://example.com/.well-known/oauth-protected-resource"
            ]
        );
    }

    #[test]
    fn callbacks_are_checked_for_host_path_state_and_errors() {
        let head =
            |target: &str, host: &str| format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n");
        let good = head("/callback?code=abc&state=s1", "127.0.0.1:5000");
        assert!(matches!(check_callback(&good, 5000, "s1"), Callback::Code(code) if code == "abc"));
        let wrong_state = head("/callback?code=abc&state=zzz", "127.0.0.1:5000");
        assert!(matches!(
            check_callback(&wrong_state, 5000, "s1"),
            Callback::Stray
        ));
        let no_state = head("/callback?code=abc", "127.0.0.1:5000");
        assert!(matches!(
            check_callback(&no_state, 5000, "s1"),
            Callback::Stray
        ));
        let denied = head("/callback?error=access_denied&state=s1", "127.0.0.1:5000");
        assert!(matches!(
            check_callback(&denied, 5000, "s1"),
            Callback::Refuse(_)
        ));
        let other_path = head("/other?code=abc&state=s1", "127.0.0.1:5000");
        assert!(matches!(
            check_callback(&other_path, 5000, "s1"),
            Callback::Ignore
        ));
        let rebinding = head("/callback?code=abc&state=s1", "evil.example:5000");
        assert!(matches!(
            check_callback(&rebinding, 5000, "s1"),
            Callback::Ignore
        ));
        let userinfo = head("@evil.example/callback?code=abc&state=s1", "127.0.0.1:5000");
        assert!(matches!(
            check_callback(&userinfo, 5000, "s1"),
            Callback::Ignore
        ));
    }

    #[test]
    fn form_bodies_are_encoded() {
        assert_eq!(
            form_body(&[("a", "b c"), ("d", "e&f=g")]),
            "a=b+c&d=e%26f%3Dg"
        );
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "ab"));
    }
}
