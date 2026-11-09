//! Slot state saved to disk when a model is swapped out, so each agent's context can be restored
//! into the model when it comes back instead of being read again.
//!
//! llama.cpp writes and reads the files itself (`POST /slots/{id}?action=save|restore` into
//! `--slot-save-path`); this keeps the index of what is there, keyed by model and agent, and
//! bounds it: past `swap_cache_mb` the files of agents that can no longer come back (their turn
//! is over) go first, then the oldest files of the session holding the most, so one busy session
//! cannot push every other session's saved context out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use imperium_config::ModelLaunch;

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
    /// servers that are gone and to agents this run no longer tracks. Fails when the folder
    /// cannot be created, so slots are not saved at all.
    pub fn open(dir: PathBuf, capacity_bytes: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == EXTENSION) {
                    remove(&path);
                }
            }
        }
        Ok(SlotFiles {
            dir,
            capacity_bytes,
            saved: Vec::new(),
        })
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
    /// folder to its capacity. `present` says whether a holder can still come back.
    pub fn record(&mut self, saved: Saved, present: impl Fn(&str) -> bool) {
        self.saved
            .retain(|old| !(old.holder == saved.holder && old.key == saved.key));
        self.saved.push(saved);
        self.trim(present);
    }

    /// `holder`'s saved slot on the model with `key`, to restore it; it stays recorded until
    /// [`SlotFiles::forget`] removes it once read.
    pub fn find(&self, holder: &str, key: u64) -> Option<Saved> {
        self.saved
            .iter()
            .find(|saved| saved.holder == holder && saved.key == key)
            .cloned()
    }

    /// Removes `holder`'s saved slot on the model with `key` from the index and its file from
    /// the folder, whether or not it was recorded: a save that failed or was cut short may have
    /// left a partial file behind.
    pub fn forget(&mut self, holder: &str, key: u64) {
        self.saved
            .retain(|saved| !(saved.holder == holder && saved.key == key));
        remove(&self.dir.join(Self::file_name(key, holder)));
    }

    /// The slots saved from the model with `key`, to give their agents back on reload.
    pub fn saved_for(&self, key: u64) -> impl Iterator<Item = &Saved> {
        self.saved.iter().filter(move |saved| saved.key == key)
    }

    /// Removes files until the folder fits: those of holders that cannot come back first, oldest
    /// first, then the oldest files of the session holding the most.
    fn trim(&mut self, present: impl Fn(&str) -> bool) {
        while self.saved.iter().map(|saved| saved.bytes).sum::<u64>() > self.capacity_bytes {
            let absent = self.saved.iter().position(|saved| !present(&saved.holder));
            let Some(at) = absent.or_else(|| self.largest_sessions_oldest()) else {
                return;
            };
            let dropped = self.saved.remove(at);
            tracing::info!(holder = %dropped.holder, "dropping a saved slot to stay within swap_cache_mb");
            remove(&self.dir.join(&dropped.file));
        }
    }

    /// The oldest file of the session holding the most bytes.
    fn largest_sessions_oldest(&self) -> Option<usize> {
        let mut by_session: HashMap<&str, u64> = HashMap::new();
        for saved in &self.saved {
            *by_session.entry(session(&saved.holder)).or_default() += saved.bytes;
        }
        let (largest, _) = by_session.into_iter().max_by_key(|(_, bytes)| *bytes)?;
        self.saved
            .iter()
            .position(|saved| session(&saved.holder) == largest)
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
/// same file with the same context, cache types, and (when a setting fixes it) pool. A pool that
/// grows on demand is left out, so an agent's prompt survives the restart that grows it; a slot
/// that no longer fits a smaller pool fails to restore and is read again. The file is identified
/// by its path, size, and modification time rather than a hash of its contents, which would mean
/// reading gigabytes on every load.
pub(crate) fn model_key(model: &LocalModel, launch: &ModelLaunch) -> u64 {
    let modified = std::fs::metadata(&model.path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    let identity = format!(
        "{}\0{}\0{}\0{modified}\0{}\0{}\0{}\0{}",
        model.id,
        model.path.display(),
        model.size_bytes,
        launch.context,
        if launch.pool_fixed {
            launch.pool_tokens
        } else {
            0
        },
        launch.cache_type_k,
        launch.cache_type_v
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
    fn a_newer_save_replaces_the_agents_older_one_and_is_forgotten_with_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = SlotFiles::open(dir.path().to_path_buf(), u64::MAX).unwrap();
        files.record(saved("s/root", 1, 10), |_| true);
        files.record(saved("s/root", 1, 20), |_| true);
        files.record(saved("s/root", 2, 30), |_| true);
        assert_eq!(files.saved_for(1).count(), 1);
        let file = dir.path().join(SlotFiles::file_name(1, "s/root"));
        std::fs::write(&file, b"kv").unwrap();
        assert_eq!(files.find("s/root", 1).unwrap().bytes, 20);
        files.forget("s/root", 1);
        assert_eq!(files.find("s/root", 1), None);
        assert!(!file.exists());
        assert!(files.find("s/root", 2).is_some());
    }

    #[test]
    fn past_capacity_the_largest_session_loses_its_oldest_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = SlotFiles::open(dir.path().to_path_buf(), 100).unwrap();
        let everyone = |_: &str| true;
        files.record(saved("quiet/root", 1, 30), everyone);
        files.record(saved("busy/root", 1, 40), everyone);
        files.record(saved("busy/t1", 1, 20), everyone);
        // 120 bytes: busy holds 90 of them, so its oldest goes, not quiet's older file.
        files.record(saved("busy/t2", 1, 30), everyone);
        let left: Vec<_> = files.saved.iter().map(|s| s.holder.as_str()).collect();
        assert_eq!(left, ["quiet/root", "busy/t1", "busy/t2"]);
    }

    #[test]
    fn files_of_agents_that_cannot_come_back_go_before_any_other() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = SlotFiles::open(dir.path().to_path_buf(), 100).unwrap();
        let alive = |holder: &str| holder != "s/t1";
        files.record(saved("s/root", 1, 40), alive);
        files.record(saved("s/t1", 1, 40), alive);
        // The finished subagent's newer file goes, not the waiting orchestrator's older one.
        files.record(saved("s/t2", 1, 40), alive);
        let left: Vec<_> = files.saved.iter().map(|s| s.holder.as_str()).collect();
        assert_eq!(left, ["s/root", "s/t2"]);
    }

    #[test]
    fn a_folder_that_cannot_be_created_turns_saving_off() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-folder");
        std::fs::write(&file, b"").unwrap();
        assert!(SlotFiles::open(file.join("swap"), 100).is_err());
    }

    #[test]
    fn opening_clears_slot_files_from_a_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.slot"), b"kv").unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"other").unwrap();
        SlotFiles::open(dir.path().to_path_buf(), 1).unwrap();
        assert!(!dir.path().join("old.slot").exists());
        assert!(dir.path().join("keep.txt").exists());
    }
}
