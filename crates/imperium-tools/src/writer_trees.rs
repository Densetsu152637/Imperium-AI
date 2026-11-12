//! Git worktrees for parallel writing subagents. The tools know nothing about git: a
//! [`WriterTrees`] (implemented by the daemon) makes the worktrees, and a workspace carries it
//! so the engine can give each writer subagent folders of its own.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// Where a writer subagent works instead of the session's folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterStart {
    /// The writer's folders, the working directory first: each of the session's folders inside a
    /// git repository replaced by the same place in a new worktree, the others as they were.
    pub folders: Vec<PathBuf>,
    /// The branch the worktrees are on.
    pub branch: String,
    /// Said to the writer and the delegator, such as a warning about uncommitted changes in the
    /// session's folders that the worktree does not hold. Empty when there is nothing to say.
    pub notice: String,
}

/// Makes and finishes writer subagents' worktrees.
pub trait WriterTrees: fmt::Debug + Send + Sync {
    /// Gives writer `task` worktrees of `folders` (the delegator's). `Ok(None)` when no folder
    /// is in a git repository on this machine: the writer then shares the delegator's folders.
    fn start(&self, task: &str, folders: &[PathBuf]) -> Result<Option<WriterStart>, String>;

    /// The folders writer `task` works in, when it has worktrees (also after a restart, for a
    /// writer being resumed).
    fn folders(&self, task: &str) -> Option<Vec<PathBuf>>;

    /// Called when writer `task` stops working: commits what it changed on its branch and says
    /// how that integrates with the delegator's checkout (the branch, the files, and whether it
    /// merges cleanly). The result is added to the writer's report.
    fn finish(&self, task: &str) -> String;
}

/// A [`WriterTrees`] a workspace carries; clones share it.
#[derive(Clone)]
pub(crate) struct WriterHandle(pub Arc<dyn WriterTrees>);

impl fmt::Debug for WriterHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq for WriterHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for WriterHandle {}
