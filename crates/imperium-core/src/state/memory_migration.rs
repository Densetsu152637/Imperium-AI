//! The daemon's side of folding the older project memory into environments' `Memory.md` (the
//! merge itself is [`imperium_memory::migrate`]; docs/17-memory.md#environment-memory-implemented).
//!
//! An environment's working directory (its first folder) is the project its sessions used to keep
//! private and shared project memory for. At every start, and again when a session starts in
//! an environment (so a folder added to an environment later is picked up), the entries of those
//! two folders are merged into that environment's `Memory.md`. A folder that is in no
//! environment is left exactly as it is, and merged the moment an environment has it: sessions
//! started in a bare folder make an environment for it (`Environments::for_folder`),
//! so that is the normal way such a folder arrives. Home and environments whose folders are
//! paths inside a runtime are skipped: Home has no project, and a remote runtime's folders were
//! never keyed on this machine.

use std::path::{Path, PathBuf};

use imperium_memory::environment::EnvironmentMemory;
use imperium_memory::migrate;
use imperium_protocol::methods::EnvironmentInfo;

use super::DaemonState;

impl DaemonState {
    /// Merges older project memory into every environment's `Memory.md`. Run when the daemon
    /// starts, after the working folders are in place. It changes nothing once a folder is
    /// merged, so it is safe to run again.
    pub fn migrate_memory(&self) {
        let mut claimed: Vec<PathBuf> = Vec::new();
        for environment in self.environments.list() {
            if let Some(folder) = self.merge_project_memory(&environment) {
                claimed.push(folder);
            }
        }
        let left = migrate::unclaimed(self.paths.data_dir(), &claimed);
        if !left.is_empty() {
            tracing::info!(
                folders = left.len(),
                "private project memory of folders that are in no environment is left where it is; \
                 it is merged into an environment's Memory.md as soon as one has the folder"
            );
        }
    }

    /// The same for one environment, when a session starts in it.
    pub(super) fn migrate_environment_memory(&self, environment: &str) {
        if let Some(environment) = self.environments.get(environment) {
            self.merge_project_memory(&environment);
        }
    }

    /// Merges the older project memory of the environment's working directory into its
    /// `Memory.md`, and returns that directory when the environment has one on this machine.
    fn merge_project_memory(&self, environment: &EnvironmentInfo) -> Option<PathBuf> {
        if environment.builtin || environment.runtime.is_remote() {
            return None;
        }
        let folder = PathBuf::from(environment.folders.first()?);
        let file = crate::workspaces::memory_file(&self.workspaces_root(), &environment.id)?;
        let memory = EnvironmentMemory::new(file);
        for source in migrate::sources_for_folder(self.paths.data_dir(), &folder) {
            log(
                &environment.id,
                &source.dir,
                migrate::migrate(self.paths.data_dir(), &environment.id, &memory, &source),
            );
        }
        Some(folder)
    }
}

fn log(
    environment: &str,
    source: &Path,
    outcome: Result<Option<migrate::Migrated>, imperium_memory::MemoryError>,
) {
    match outcome {
        Ok(Some(done)) => tracing::info!(
            %environment,
            source = %source.display(),
            entries = done.entries,
            added = done.added,
            duplicates = done.duplicates,
            kept_both = done.kept_both,
            archived = done.archived,
            backup = %done.backup,
            "merged older project memory into the environment's Memory.md"
        ),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            %environment,
            source = %source.display(),
            %error,
            "could not merge older project memory into the environment's Memory.md; it stays where it is"
        ),
    }
}
