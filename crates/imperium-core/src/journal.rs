use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use imperium_protocol::session::SessionEvent;
use serde::Serialize;

/// Append-only JSON Lines record of a session: one object per event, with a timestamp.
#[derive(Debug)]
pub struct Journal {
    file: File,
}

#[derive(Serialize)]
struct Line<'a> {
    at_ms: u128,
    #[serde(flatten)]
    event: &'a SessionEvent,
}

impl Journal {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Journal { file })
    }

    pub fn record(&mut self, event: &SessionEvent) -> std::io::Result<()> {
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let mut line = serde_json::to_vec(&Line { at_ms, event })?;
        line.push(b'\n');
        self.file.write_all(&line)
    }
}
