//! Same-user local sockets for the daemon: a Unix domain socket on Unix, a named pipe on
//! Windows. Both carry the same byte stream as the loopback TCP listener, so the daemon runs the
//! one HTTP and WebSocket server over either and the one token check applies to all of them.
//!
//! Only the current user can connect: the socket file is created inside a private (0700)
//! directory and set to 0600; the pipe gets a DACL granting access to the current user's SID and
//! nobody else, and refuses remote clients. A local connection is not trusted for being local;
//! the daemon still asks for its token.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{Listener, Stream, connect, peer_pid};
#[cfg(windows)]
pub use windows::{Listener, Stream, connect, peer_pid};

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    /// The daemon's side of a real connection reports the process on the other end: here this
    /// one, which made both ends. The pipe's expected server is this process too.
    #[tokio::test]
    async fn the_accepted_end_knows_its_peers_process_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut listener = Listener::bind(dir.path()).unwrap();
        let address = listener.address();
        let accepting = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            (listener, stream)
        });
        let client = connect(&address, Some(std::process::id())).await.unwrap();
        let (_listener, server) = accepting.await.unwrap();
        assert_eq!(peer_pid(&server), Some(std::process::id()));
        // The client's own end does not know its peer through this call on a pipe; on a Unix
        // socket it does, and it is this process too.
        #[cfg(unix)]
        assert_eq!(peer_pid(&client), Some(std::process::id()));
        #[cfg(windows)]
        assert_eq!(peer_pid(&client), None);
    }
}
