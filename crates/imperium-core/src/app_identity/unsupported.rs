//! A platform other than Linux, Windows and macOS: not built. Every approval
//! is refused, so these platforms record nothing.

use std::path::Path;

/// The peer of a local-socket connection; nothing is taken from it yet.
#[derive(Debug)]
pub(crate) struct Peer;

impl Peer {
    #[cfg(test)]
    pub(crate) fn test_peer() -> Peer {
        Peer
    }
}

pub(super) fn at_accept(_: &imperium_ipc::Stream) -> Option<Peer> {
    Some(Peer)
}

pub(super) fn identify(_: &Peer, _: &Path) -> Result<(), String> {
    Err(
        "identifying the desktop app is not built for this platform yet: nothing was recorded"
            .into(),
    )
}

/// Check 7 applies on Windows only.
pub(super) fn may_count(_: crate::plugin_records::ApprovalKind) -> Result<(), String> {
    Ok(())
}
