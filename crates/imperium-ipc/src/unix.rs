//! The Unix domain socket transport.

use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

/// One accepted or connected socket.
pub type Stream = UnixStream;

/// Folder inside the data directory that holds the socket. Created 0700, so no other user can
/// reach the socket even for the instant between `bind` and the `chmod`.
const DIRECTORY: &str = "ipc";
const SOCKET: &str = "daemon.sock";

/// A listening socket at `<data>/ipc/daemon.sock`. The file is removed when this is dropped.
#[derive(Debug)]
pub struct Listener {
    inner: UnixListener,
    path: PathBuf,
}

impl Listener {
    /// Binds the socket under `data_dir`. A leftover file from a daemon that died is removed;
    /// a socket another process still answers on, or any file that is not a socket, is an error.
    pub fn bind(data_dir: &Path) -> io::Result<Self> {
        let directory = data_dir.join(DIRECTORY);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        // An existing folder keeps its mode through `create`; make it private regardless.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let path = directory.join(SOCKET);
        remove_stale(&path)?;
        let inner = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Listener { inner, path })
    }

    /// Where clients connect (a file path), for the endpoint file.
    pub fn address(&self) -> String {
        self.path.display().to_string()
    }

    pub async fn accept(&mut self) -> io::Result<Stream> {
        self.inner.accept().await.map(|(stream, _)| stream)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // Best effort: the next start also clears a leftover.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Removes `path` when it is a socket nobody listens on.
fn remove_stale(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is not a socket", path.display()),
        ));
    }
    // A synchronous probe: a refused connection means the owner is gone.
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("another process is listening on {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

/// Connects to a daemon's socket. `_expected_pid` is only used on Windows, where a named pipe
/// could be created by another user; a Unix socket sits in a folder only this user can enter.
pub async fn connect(address: &str, _expected_pid: Option<u32>) -> io::Result<Stream> {
    UnixStream::connect(address).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn the_socket_is_private_carries_bytes_and_is_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let mut listener = Listener::bind(dir.path()).unwrap();
        let address = listener.address();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(Path::new(&address)), 0o600);
        assert_eq!(mode(&dir.path().join(DIRECTORY)), 0o700);

        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            stream.write_all(&[byte[0] + 1]).await.unwrap();
            listener
        });
        let mut client = connect(&address, None).await.unwrap();
        client.write_all(&[41]).await.unwrap();
        let mut reply = [0u8; 1];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [42]);

        drop(server.await.unwrap());
        assert!(!Path::new(&address).exists());
    }

    #[tokio::test]
    async fn a_stale_socket_is_replaced_but_a_live_one_or_a_plain_file_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let live = Listener::bind(dir.path()).unwrap();
        let path = PathBuf::from(live.address());

        // A live listener is never taken over.
        let error = Listener::bind(dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        drop(live);

        // A crash leaves the file with nobody listening: bind a socket and drop it unlinked.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let replacement = Listener::bind(dir.path()).unwrap();
        assert_eq!(replacement.address(), path.display().to_string());
        drop(replacement);

        // Something that is not a socket is left alone.
        std::fs::write(&path, b"mine").unwrap();
        let error = Listener::bind(dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"mine");
    }
}
