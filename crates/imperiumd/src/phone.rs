//! Paired phones: a separate listener, off until `phone.bind` is set, that serves the phone
//! web app, completes pairing with one-time codes, and accepts WebSocket connections only from
//! paired devices, restricted to the methods their scopes allow.
//!
//! Pairing gives each phone a random token; only its SHA-256 is stored, in
//! `<data>/devices.json`, and either side can revoke it. The listener does not add encryption:
//! bind it to a private overlay address (Tailscale, ZeroTier, WireGuard) to use the phone away
//! from home, or to a trusted LAN.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Local;
use imperium_protocol::methods::{DeviceInfo, PairingCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

use crate::{Connection, DaemonState};

const CODE_LIFETIME: Duration = Duration::from_secs(300);
const PHONE_APP: &str = include_str!("phone.html");
const TOKEN_SUBPROTOCOL_PREFIX: &str = "imperium.token.";

/// Methods a phone may call, by scope.
const READ_METHODS: &[&str] = &[
    "system.handshake",
    "system.ping",
    "session.list",
    "session.subscribe",
    "schedules.list",
    "approvals.list",
    "pet.list",
    "pet.strip",
    "projects.list",
];
const APPROVE_METHODS: &[&str] = &["approvals.respond", "pet.interact"];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Device {
    id: String,
    name: String,
    token_sha256: String,
    scopes: Vec<String>,
    paired_at: String,
}

/// Paired devices and pending pairing codes.
#[derive(Debug)]
pub struct Devices {
    path: PathBuf,
    devices: Mutex<Vec<Device>>,
    codes: Mutex<Vec<(String, Instant)>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the OS random source is available");
    hex(&buffer)
}

impl Devices {
    pub fn load(path: PathBuf) -> Self {
        let devices = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Devices {
            path,
            devices: Mutex::new(devices),
            codes: Mutex::new(Vec::new()),
        }
    }

    fn save(&self, devices: &[Device]) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(devices).map_err(std::io::Error::other)?;
        std::fs::write(&self.path, text)
    }

    /// A new one-time pairing code, valid for five minutes.
    pub fn new_code(&self) -> PairingCode {
        let mut number = [0u8; 4];
        getrandom::fill(&mut number).expect("the OS random source is available");
        let code = format!("{:08}", u32::from_le_bytes(number) % 100_000_000);
        let mut codes = self.codes.lock().unwrap_or_else(PoisonError::into_inner);
        codes.retain(|(_, expires)| *expires > Instant::now());
        codes.push((code.clone(), Instant::now() + CODE_LIFETIME));
        PairingCode {
            code,
            expires_in_s: CODE_LIFETIME.as_secs(),
        }
    }

    /// Exchanges a valid code for a device token (returned once, never stored).
    pub fn pair(&self, code: &str, name: &str) -> Result<(String, String), String> {
        {
            let mut codes = self.codes.lock().unwrap_or_else(PoisonError::into_inner);
            let before = codes.len();
            codes.retain(|(candidate, expires)| candidate != code || *expires <= Instant::now());
            if codes.len() == before {
                return Err("the code is wrong or has expired".into());
            }
            codes.retain(|(_, expires)| *expires > Instant::now());
        }
        let token = random_hex(32);
        let device = Device {
            id: random_hex(6),
            name: name.chars().take(64).collect(),
            token_sha256: hex(&Sha256::digest(token.as_bytes())),
            scopes: vec!["read".into(), "approve".into()],
            paired_at: Local::now().to_rfc3339(),
        };
        let id = device.id.clone();
        let mut devices = self.devices.lock().unwrap_or_else(PoisonError::into_inner);
        devices.push(device);
        self.save(&devices).map_err(|error| error.to_string())?;
        Ok((id, token))
    }

    /// The methods a token may call, when it belongs to a paired device.
    fn allowed(&self, token: &str) -> Option<Vec<&'static str>> {
        let digest = hex(&Sha256::digest(token.as_bytes()));
        let devices = self.devices.lock().unwrap_or_else(PoisonError::into_inner);
        let device = devices.iter().find(|device| {
            constant_time_eq::constant_time_eq(device.token_sha256.as_bytes(), digest.as_bytes())
        })?;
        let mut methods = Vec::new();
        if device.scopes.iter().any(|scope| scope == "read") {
            methods.extend_from_slice(READ_METHODS);
        }
        if device.scopes.iter().any(|scope| scope == "approve") {
            methods.extend_from_slice(APPROVE_METHODS);
        }
        Some(methods)
    }

    pub fn list(&self) -> Vec<DeviceInfo> {
        self.devices
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|device| DeviceInfo {
                id: device.id.clone(),
                name: device.name.clone(),
                scopes: device.scopes.clone(),
                paired_at: device.paired_at.clone(),
            })
            .collect()
    }

    /// Forgets a device; it can no longer connect. False when there was none.
    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        let mut devices = self.devices.lock().unwrap_or_else(PoisonError::into_inner);
        let before = devices.len();
        devices.retain(|device| device.id != id);
        if devices.len() == before {
            return Ok(false);
        }
        self.save(&devices).map_err(|error| error.to_string())?;
        Ok(true)
    }
}

/// Serves the phone app, pairing, and device connections on `listener` until the daemon stops.
pub async fn serve(listener: TcpListener, daemon: Arc<DaemonState>) -> std::io::Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(PHONE_APP) }))
        .route("/pair", post(pair))
        .route("/rpc", get(upgrade))
        .with_state(daemon);
    axum::serve(listener, app).await
}

/// Binds `address` and serves phones there in the background.
pub async fn start(daemon: &Arc<DaemonState>, address: SocketAddr) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(serve(listener, Arc::clone(daemon)));
    tracing::info!(address = %bound, "phone listener ready");
    Ok(bound)
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    name: String,
}

#[derive(Serialize)]
struct PairResponse {
    device_id: String,
    token: String,
}

async fn pair(
    State(daemon): State<Arc<DaemonState>>,
    axum::Json(request): axum::Json<PairRequest>,
) -> Response {
    match daemon.devices().pair(request.code.trim(), &request.name) {
        Ok((device_id, token)) => axum::Json(PairResponse { device_id, token }).into_response(),
        Err(message) => (StatusCode::FORBIDDEN, message).into_response(),
    }
}

async fn upgrade(
    State(daemon): State<Arc<DaemonState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let from_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string);
    let from_protocol = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(|offered| {
            offered
                .split(',')
                .find_map(|protocol| protocol.trim().strip_prefix(TOKEN_SUBPROTOCOL_PREFIX))
                .map(str::to_string)
        });
    let Some(allowed) = from_header
        .or(from_protocol)
        .and_then(|token| daemon.devices().allowed(&token))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    ws.protocols(["imperium.v0"]).on_upgrade(move |socket| {
        let announcements = daemon.announcements();
        let connection = Connection::restricted(daemon, allowed);
        crate::server::run_connection(socket, connection, announcements)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_pair_once_and_tokens_are_scoped_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let devices = Devices::load(dir.path().join("devices.json"));
        let code = devices.new_code();
        assert_eq!(code.code.len(), 8);
        assert!(devices.pair("00000000x", "phone").is_err());
        let (id, token) = devices.pair(&code.code, "Pixel 9").unwrap();
        assert!(
            devices.pair(&code.code, "again").is_err(),
            "codes work once"
        );
        let allowed = devices.allowed(&token).unwrap();
        assert!(allowed.contains(&"approvals.respond"));
        assert!(!allowed.contains(&"session.create"));
        assert!(devices.allowed("not-a-token").is_none());
        // The token itself is never stored.
        let stored = std::fs::read_to_string(dir.path().join("devices.json")).unwrap();
        assert!(!stored.contains(&token));
        // Pairings survive a restart, and revoking ends them.
        let reloaded = Devices::load(dir.path().join("devices.json"));
        assert_eq!(reloaded.list()[0].name, "Pixel 9");
        assert!(reloaded.revoke(&id).unwrap());
        assert!(reloaded.allowed(&token).is_none());
    }
}
