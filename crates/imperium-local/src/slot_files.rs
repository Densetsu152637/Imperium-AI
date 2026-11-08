//! Slot state saved to disk when a model is swapped out, so each agent's context can be restored
//! into the model when it comes back instead of being read again.
//!
//! llama.cpp writes and reads the files itself (`POST /slots/{id}?action=save|restore` into
//! `--slot-save-path`); this keeps the index of what is there, keyed by model and agent, and
//! bounds it: past `swap_cache_mb` the oldest files go first, taken from the session holding
//! the most, so one busy session cannot push every other session's saved context out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::scan::LocalModel;

/// Extension of the files this module manages; others in the folder are left alone.
const EXTENSION: &str = "slot";

/// One slot saved to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Saved {
    /// The agent whose context it is.
    pub holder: String,
    /// The model and settings it was saved from ([`model_key`]).
    pub key: u64,
    /// The slot it was saved from, which the agent is given back when it is free.
    pub slot: u32,
    /// The file's name inside the folder.
    pub file: String,
    pub bytes: u64,
}

/// The saved slots in one folder, oldest first.
#[derive(Debug)]
pub(crate) struct SlotFiles {
    dir: PathBuf,
    capacity_bytes: u64,
    saved: Vec<Saved>,
}

impl SlotFiles {
    /// Uses `dir`, creating it and removing slot files a previous run left: they belong to
    /// servers that are gone and to agents this run no longer tracks.
    pub fn open(dir: PathBuf, capacity_bytes: u64) -> Self {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            tracing::warn!(dir = %dir.display(), %error, "could not create the slot folder");
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == EXTENSION) {
                    remove(&path);
                }
            }
        }
        SlotFiles {
            dir,
            capacity_bytes,
            saved: Vec::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file `holder`'s slot on the model with `key` is saved to; only hex digits, a dash,
    /// and the extension, which llama.cpp accepts as a file name.
    pub fn file_name(key: u64, holder: &str) -> String {
        format!("{key:016x}-{:016x}.{EXTENSION}", fnv1a(holder.as_bytes()))
    }

    /// Records a saved slot, replacing `holder`'s earlier one on the same model, and trims the
    /// folder to its capacity.
    pub fn record(&mut self, saved: Saved) {
        self.saved
            .retain(|old| !(old.holder == saved.holder && old.key == saved.key));
        self.saved.push(saved);
        self.trim();
    }

    /// Takes `holder`'s saved slot on the model with `key`, to restore it; the caller removes
    /// the file once it is read.
    pub fn take(&mut self, holder: &str, key: u64) -> Option<Saved> {
        let at = self
            .saved
            .iter()
            .position(|saved| saved.holder == holder && saved.key == key)?;
        Some(self.saved.remove(at))
    }

    /// The slots saved from the model with `key`, to give their agents back on reload.
    pub fn saved_for(&self, key: u64) -> impl Iterator<Item = &Saved> {
        self.saved.iter().filter(move |saved| saved.key == key)
    }

    pub fn path(&self, saved: &Saved) -> PathBuf {
        self.dir.join(&saved.file)
    }

    /// Removes the oldest files of the session holding the most until the folder fits.
    fn trim(&mut self) {
        while self.saved.iter().map(|saved| saved.bytes).sum::<u64>() > self.capacity_bytes {
            let mut by_session: HashMap<&str, u64> = HashMap::new();
            for saved in &self.saved {
                *by_session.entry(session(&saved.holder)).or_default() += saved.bytes;
            }
            let Some((largest, _)) = by_session.into_iter().max_by_key(|(_, bytes)| *bytes) else {
                return;
            };
            let largest = largest.to_string();
            let Some(at) = self
                .saved
                .iter()
                .position(|saved| session(&saved.holder) == largest)
            else {
                return;
            };
            let dropped = self.saved.remove(at);
            tracing::info!(holder = %dropped.holder, "dropping a saved slot to stay within swap_cache_mb");
            remove(&self.dir.join(&dropped.file));
        }
    }
}

/// Removes a slot file, which may already be gone.
pub(crate) fn remove(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "could not remove a slot file");
    }
}

/// The session part of a holder (`<session>/<agent>`).
fn session(holder: &str) -> &str {
    holder.split('/').next().unwrap_or(holder)
}

/// Identifies the model a slot was saved from: a saved slot only fits a server started from the
/// same file with the same per-slot context and cache type. The file is identified by its path,
/// size, and modification time rather than a hash of its contents, which would mean reading
/// gigabytes on every load.
pub(crate) fn model_key(model: &LocalModel, slot_context: u32, cache_type: &str) -> u64 {
    let modified = std::fs::metadata(&model.path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    let identity = format!(
        "{}\0{}\0{}\0{modified}\0{slot_context}\0{cache_type}",
        model.id,
        model.path.display(),
        model.size_bytes
    );
    fnv1a(identity.as_bytes())
}

/// FNV-1a, a stable hash, so names stay the same across builds.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(holder: &str, key: u64, bytes: u64) -> Saved {
        Saved {
            holder: holder.into(),
            key,
            slot: 0,
            file: SlotFiles::file_name(key, holder),
            bytes,
        }
    }

    #[test]
    fn files_are_named_by_model_and_agent() {
        let name = SlotFiles::file_name(0xab, "session/root");
        assert!(name.starts_with("00000000000000ab-"), "{name}");
        assert!(name.ends_with(".slot"));
        assert_ne!(name, SlotFiles::file_name(0xab, "session/t1"));
    }

    #[test]
    fn a_newer_save_replaces_the_agents_older_one_and_is_taken_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = SlotFiles::open(dir.path().to_path_buf(), u64::MAX);
        files.record(saved("s/root", 1, 10));
        files.record(saved("s/root", 1, 20));
        files.record(saved("s/root", 2, 30));
        assert_eq!(files.saved_for(1).count(), 1);
        assert_eq!(files.take("s/root", 1).unwrap().bytes, 20);
        assert_eq!(files.take("s/root", 1), None);
        assert!(files.take("s/root", 2).is_some());
    }

    #[test]
    fn past_capacity_the_largest_session_loses_its_oldest_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = SlotFiles::open(dir.path().to_path_buf(), 100);
        files.record(saved("quiet/root", 1, 30));
        files.record(saved("busy/root", 1, 40));
        files.record(saved("busy/t1", 1, 20));
        // 120 bytes: busy holds 60 of them, so its oldest goes, not quiet's older file.
        files.record(saved("busy/t2", 1, 30));
        let left: Vec<_> = files.saved.iter().map(|s| s.holder.as_str()).collect();
        assert_eq!(left, ["quiet/root", "busy/t1", "busy/t2"]);
    }

    #[test]
    fn opening_clears_slot_files_from_a_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.slot"), b"kv").unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"other").unwrap();
        SlotFiles::open(dir.path().to_path_buf(), 1);
        assert!(!dir.path().join("old.slot").exists());
        assert!(dir.path().join("keep.txt").exists());
    }
}
