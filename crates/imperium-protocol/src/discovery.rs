//! How local clients find a running daemon.
//!
//! The daemon writes a [`DaemonEndpoint`] as JSON to its endpoint file (see
//! `imperium_config::Paths::daemon_endpoint_file`) when it starts and removes it on shutdown.
//! The file is readable only by the user who started the daemon.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ProtocolVersion;

/// Where a running daemon listens and the bearer token it accepts.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonEndpoint {
    /// WebSocket URL, for example `ws://127.0.0.1:52011/rpc`.
    pub url: String,
    /// Per-launch bearer token for the `Authorization` header.
    pub token: String,
    pub pid: u32,
    pub protocol: ProtocolVersion,
    /// Where the same protocol is also served over a local socket, when `daemon.socket` is on:
    /// a Unix socket's path, or a Windows named pipe's name. The token is the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
}

impl std::fmt::Debug for DaemonEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonEndpoint")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .field("pid", &self.pid)
            .field("protocol", &self.protocol)
            .field("socket", &self.socket)
            .finish()
    }
}

/// Why a daemon endpoint could not be read.
#[derive(Debug)]
pub enum EndpointError {
    /// No endpoint file: the daemon is not running.
    NotRunning(PathBuf),
    /// The file exists but could not be read or parsed.
    Unreadable { path: PathBuf, reason: String },
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointError::NotRunning(path) => write!(
                f,
                "imperium-core is not running (no endpoint file at {})",
                path.display()
            ),
            EndpointError::Unreadable { path, reason } => write!(
                f,
                "could not read the endpoint file {}: {reason}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for EndpointError {}

/// Reads the endpoint file a running daemon writes on startup.
pub fn read_endpoint(path: &Path) -> Result<DaemonEndpoint, EndpointError> {
    let unreadable = |reason: String| EndpointError::Unreadable {
        path: path.to_path_buf(),
        reason,
    };
    let bytes = std::fs::read(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => EndpointError::NotRunning(path.to_path_buf()),
        _ => unreadable(error.to_string()),
    })?;
    serde_json::from_slice(&bytes).map_err(|error| unreadable(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_token() {
        let endpoint = DaemonEndpoint {
            url: "ws://127.0.0.1:1/rpc".into(),
            token: "s3cret-token".into(),
            pid: 1,
            protocol: crate::PROTOCOL_VERSION,
            socket: None,
        };
        assert!(!format!("{endpoint:?}").contains("s3cret-token"));
    }
}
