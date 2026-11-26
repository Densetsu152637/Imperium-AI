//! Wire contract between `imperium-core` and its clients.
//!
//! Messages are JSON-RPC 2.0 over a WebSocket. Every type here derives [`ts_rs::TS`] and is
//! exported to `packages/protocol/src/generated/` when the crate's tests run, so TypeScript
//! clients share the same definitions.

#![warn(unreachable_pub)]

pub mod agents;
pub mod budgets;
pub mod clarification;
pub mod discovery;
pub mod errors;
pub mod initiative;
pub mod jsonrpc;
pub mod knowledge_eval;
pub mod methods;
pub mod patterns;
pub mod permissions;
pub mod session;
pub mod tools;
pub mod workspaces;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion {
    major: 0,
    minor: 64,
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

    /// Reads `source` for the method names (`const NAME: &'static str = "..."` in a `Method`
    /// impl) and the notification names (`pub const X: &str = "area.event"`) it defines.
    fn names_in(source: &str, names: &mut std::collections::BTreeSet<String>) {
        for line in source.lines().map(str::trim) {
            let quoted = |after: &str| {
                let rest = line.strip_prefix(after)?;
                Some(rest[..rest.find('"')?].to_string())
            };
            if let Some(name) = quoted("const NAME: &'static str = \"") {
                names.insert(format!("method {name}"));
            } else if line.starts_with("pub const ")
                && let Some((_, rest)) = line.split_once(": &str = \"")
                && let Some(end) = rest.find('"')
                && rest[..end].contains('.')
                && rest[..end]
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || matches!(c, '_' | '.'))
            {
                names.insert(format!("notification {}", &rest[..end]));
            }
        }
    }

    fn scan(dir: &std::path::Path, names: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read the source folder") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                scan(&path, names);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let source = std::fs::read_to_string(&path).expect("read a source file");
                names_in(&source, names);
            }
        }
    }

    /// Writes every method and notification name to `snapshots/methods.txt`.
    /// `scripts/check-rust.sh` fails when it differs from what is checked in, so a method that
    /// is added, renamed or removed (for example by moving code between files) shows in review.
    #[test]
    fn exports_the_method_names() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut names = std::collections::BTreeSet::new();
        scan(&root.join("src"), &mut names);
        let mut text = names.into_iter().collect::<Vec<_>>().join("\n");
        text.push('\n');
        std::fs::create_dir_all(root.join("snapshots")).expect("create snapshots");
        std::fs::write(root.join("snapshots/methods.txt"), text).expect("write the names");
    }
}
