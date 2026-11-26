//! Where the worker binary is: beside the daemon's own executable (plan stage 1). Installers ship
//! it as the sidecar `imperium-script` next to `imperium-core`, and the CLI package puts it in
//! the same folder.

use std::path::{Path, PathBuf};

/// The worker binary's name, without the platform's executable suffix.
pub const WORKER_NAME: &str = "imperium-script";

/// The worker beside `daemon` (the daemon's executable path), when it is there. `None` makes
/// code mode unavailable with "the worker is not installed beside the daemon".
#[must_use]
pub fn worker_beside(daemon: &Path) -> Option<PathBuf> {
    let worker = daemon
        .parent()?
        .join(format!("{WORKER_NAME}{}", std::env::consts::EXE_SUFFIX));
    worker.is_file().then_some(worker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_is_found_beside_the_daemon_and_only_there() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = dir
            .path()
            .join(format!("imperium-core{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&daemon, b"").unwrap();
        assert_eq!(worker_beside(&daemon), None);
        let worker = dir
            .path()
            .join(format!("imperium-script{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&worker, b"").unwrap();
        assert_eq!(worker_beside(&daemon), Some(worker));
    }
}
