//! The browser's debugger connection: Chrome DevTools Protocol messages over the pipe the browser
//! inherits (`--remote-debugging-pipe`, see [`crate::launch`]), one JSON message per frame, each
//! frame ended by a NUL byte. The browser opens no TCP port for it, so no other process on the
//! machine can attach to the signed-in profile.
//!
//! The pipe's two ends are ordinary blocking pipes, read and written by a thread each; the
//! [`Pipe`] hands their frames to async code through channels, so reading it is cancel-safe (a
//! timed-out wait loses nothing).

use std::io::{ErrorKind, Read, Write};

use tokio::sync::mpsc;

/// Largest message the browser may send: a screenshot is a few megabytes of base64. A longer
/// one ends the connection rather than growing without bound.
const MAX_FRAME: usize = 64 * 1024 * 1024;
/// Frames read ahead of the caller before the reader waits.
const QUEUE: usize = 256;

/// The daemon's ends of the browser's debugging pipe.
pub(crate) struct Pipe {
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
    incoming: mpsc::Receiver<Vec<u8>>,
}

impl Pipe {
    /// A pipe over the daemon's ends: `reader` is what the browser writes (its fd 4), `writer`
    /// what it reads (its fd 3). The threads end when the browser closes its end, or when this
    /// [`Pipe`] is dropped and the browser has gone.
    pub(crate) fn over(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> std::io::Result<Self> {
        let (outgoing, to_write) = mpsc::unbounded_channel();
        let (frames, incoming) = mpsc::channel(QUEUE);
        std::thread::Builder::new()
            .name("imperium-cdp-write".into())
            .spawn(move || write_frames(writer, to_write))?;
        std::thread::Builder::new()
            .name("imperium-cdp-read".into())
            .spawn(move || read_frames(reader, &frames))?;
        Ok(Pipe { outgoing, incoming })
    }

    /// Sends one message. An error means the browser's end is gone.
    pub(crate) fn send(&self, message: &serde_json::Value) -> Result<(), String> {
        self.outgoing
            .send(message.to_string().into_bytes())
            .map_err(|_| "the browser closed its debugging pipe".to_string())
    }

    /// The next message the browser sent; `None` once it closed the pipe.
    pub(crate) async fn next(&mut self) -> Option<Vec<u8>> {
        self.incoming.recv().await
    }

    /// A pipe whose other end is a [`Remote`] in the same process, for tests that stand in for
    /// the browser.
    #[cfg(test)]
    pub(crate) fn pair() -> (Self, Remote) {
        let (outgoing, commands) = mpsc::unbounded_channel();
        let (replies, incoming) = mpsc::channel(QUEUE);
        (Pipe { outgoing, incoming }, Remote { commands, replies })
    }
}

/// The browser's side of a [`Pipe::pair`].
#[cfg(test)]
pub(crate) struct Remote {
    /// What the daemon sent, one message each.
    pub commands: mpsc::UnboundedReceiver<Vec<u8>>,
    /// What the browser says, one message each; dropping it closes the pipe.
    pub replies: mpsc::Sender<Vec<u8>>,
}

/// Writes each message with its NUL terminator until the sender is dropped or the pipe fails.
fn write_frames(mut writer: impl Write, mut messages: mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(mut frame) = messages.blocking_recv() {
        frame.push(0);
        if writer
            .write_all(&frame)
            .and_then(|()| writer.flush())
            .is_err()
        {
            break;
        }
    }
}

/// Splits what `reader` gives at NUL bytes and passes each frame on, until the pipe ends, fails,
/// a frame grows past [`MAX_FRAME`], or nobody listens any more.
fn read_frames(mut reader: impl Read, frames: &mpsc::Sender<Vec<u8>>) {
    let mut pending = Vec::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let mut rest = &chunk[..read];
        while let Some(end) = rest.iter().position(|byte| *byte == 0) {
            pending.extend_from_slice(&rest[..end]);
            if frames.blocking_send(std::mem::take(&mut pending)).is_err() {
                return;
            }
            rest = &rest[end + 1..];
        }
        pending.extend_from_slice(rest);
        if pending.len() > MAX_FRAME {
            tracing::warn!("the browser sent a message longer than the debugging pipe accepts");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn frames_cross_a_real_pipe_in_both_directions_split_at_nul_bytes() {
        // The daemon's ends of two OS pipes, and the browser's.
        let (ours_read, mut browser_writes) = std::io::pipe().unwrap();
        let (browser_reads, ours_write) = std::io::pipe().unwrap();
        let mut pipe = Pipe::over(ours_read, ours_write).unwrap();

        pipe.send(&serde_json::json!({"id": 1, "method": "Browser.getVersion"}))
            .unwrap();
        let sent = tokio::task::spawn_blocking(move || {
            let mut reader = browser_reads;
            let mut got = Vec::new();
            let mut byte = [0_u8; 1];
            while reader.read_exact(&mut byte).is_ok() && byte[0] != 0 {
                got.push(byte[0]);
            }
            got
        })
        .await
        .unwrap();
        assert_eq!(sent, br#"{"id":1,"method":"Browser.getVersion"}"#);

        // Two frames in one write, and one split across writes.
        browser_writes
            .write_all(b"{\"id\":1}\0{\"id\":2}\0{\"id\"")
            .unwrap();
        browser_writes.write_all(b":3}\0").unwrap();
        for want in [r#"{"id":1}"#, r#"{"id":2}"#, r#"{"id":3}"#] {
            let frame = tokio::time::timeout(Duration::from_secs(10), pipe.next())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(frame, want.as_bytes());
        }
        // The browser closing its end ends the stream.
        drop(browser_writes);
        let ended = tokio::time::timeout(Duration::from_secs(10), pipe.next())
            .await
            .unwrap();
        assert_eq!(ended, None);
    }

    #[tokio::test]
    async fn a_wait_that_times_out_loses_no_frame() {
        let (mut pipe, remote) = Pipe::pair();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), pipe.next())
                .await
                .is_err()
        );
        remote.replies.send(b"late".to_vec()).await.unwrap();
        assert_eq!(pipe.next().await.as_deref(), Some(&b"late"[..]));
    }
}
