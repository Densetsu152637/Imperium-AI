use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use imperium_protocol::discovery::DaemonEndpoint;

/// The daemon's endpoint file. It is created readable only by the current user (on Unix,
/// mode 0600; on Windows the per-user data directory's ACL applies) and removed when dropped.
#[derive(Debug)]
pub struct EndpointFile {
    path: PathBuf,
}

impl EndpointFile {
    pub fn write(path: &Path, endpoint: &DaemonEndpoint) -> std::io::Result<Self> {
        let json = serde_json::to_vec_pretty(endpoint)?;
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&json)?;
        file.sync_all()?;
        Ok(EndpointFile {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for EndpointFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            tracing::warn!(path = %self.path.display(), %error, "could not remove endpoint file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_exists_while_held_and_is_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        let endpoint = DaemonEndpoint {
            url: "ws://127.0.0.1:1/rpc".into(),
            token: "t".into(),
            pid: 42,
            protocol: imperium_protocol::PROTOCOL_VERSION,
        };

        let file = EndpointFile::write(&path, &endpoint).unwrap();
        let read: DaemonEndpoint = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read, endpoint);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        drop(file);
        assert!(!path.exists());
    }
}
