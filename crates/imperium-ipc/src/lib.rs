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
pub use unix::{Listener, Stream, connect};
#[cfg(windows)]
pub use windows::{Listener, Stream, connect};
