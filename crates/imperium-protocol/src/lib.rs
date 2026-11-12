//! Wire contract between `imperium-core` and its clients.
//!
//! Messages are JSON-RPC 2.0 over a WebSocket. Every type here derives [`ts_rs::TS`] and is
//! exported to `packages/protocol/src/generated/` when the crate's tests run, so TypeScript
//! clients share the same definitions.

pub mod agents;
pub mod budgets;
pub mod discovery;
pub mod initiative;
pub mod jsonrpc;
pub mod methods;
pub mod permissions;
pub mod session;
pub mod tools;
pub mod workspaces;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion {
    major: 0,
    minor: 28,
};

/// Protocol version exchanged in the handshake. Peers are compatible when majors match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

impl ProtocolVersion {
    pub fn is_compatible_with(self, other: ProtocolVersion) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_with_the_same_major_are_compatible() {
        let a = ProtocolVersion { major: 1, minor: 0 };
        let b = ProtocolVersion { major: 1, minor: 7 };
        let c = ProtocolVersion { major: 2, minor: 0 };
        assert!(a.is_compatible_with(b));
        assert!(!a.is_compatible_with(c));
    }
}
