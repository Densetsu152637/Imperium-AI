//! How local clients find a running daemon.
//!
//! The daemon writes a [`DaemonEndpoint`] as JSON to its endpoint file (see
//! `imperium_config::Paths::daemon_endpoint_file`) when it starts and removes it on shutdown.
//! The file is readable only by the user who started the daemon.

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
}

impl std::fmt::Debug for DaemonEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonEndpoint")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .field("pid", &self.pid)
            .field("protocol", &self.protocol)
            .finish()
    }
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
        };
        assert!(!format!("{endpoint:?}").contains("s3cret-token"));
    }
}
