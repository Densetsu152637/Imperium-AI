use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use imperium_protocol::session::AgentEvent;
use serde::{Deserialize, Serialize};

/// Append-only JSON Lines record of a session: one object per event, with a timestamp.
#[derive(Debug)]
pub struct Journal {
    file: File,
}

#[derive(Serialize)]
struct Line<'a> {
    at_ms: u128,
    #[serde(flatten)]
    event: &'a AgentEvent,
}

impl Journal {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Journal { file })
    }

    pub fn record(&mut self, event: &AgentEvent) -> std::io::Result<()> {
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let mut line = serde_json::to_vec(&Line { at_ms, event })?;
        line.push(b'\n');
        self.file.write_all(&line)
    }
}

/// The events recorded in a journal, oldest first; unreadable lines are skipped.
pub fn read_events(path: &Path) -> Vec<AgentEvent> {
    #[derive(Deserialize)]
    struct Stored {
        #[serde(flatten)]
        event: AgentEvent,
    }
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Stored>(line).ok())
        .map(|stored| stored.event)
        .collect()
}

/// Where a session's root conversation is kept between turns, beside its journal.
pub fn messages_path(journal: &Path) -> std::path::PathBuf {
    journal.with_extension("messages.json")
}
