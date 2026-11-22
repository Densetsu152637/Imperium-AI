//! Built-in agent tools. File tools are confined to a [`Workspace`] root (isolation tier 0:
//! a policy check, not an OS sandbox — commands run with the user's full rights). A remote
//! workspace runs them inside a Docker container, WSL, or over SSH ([`runtime`]).

#![warn(unreachable_pub)]

mod exec;
mod files;
mod invocation;
mod mentions;
mod processes;
mod project_worktrees;
mod remote_files;
pub mod runtime;
mod sandbox;
mod search;
mod workspace;
mod writer_trees;

pub use exec::{SPAWNED_MARK, shell};
pub use invocation::{Access, Invocation, ToolError, specs};
pub use mentions::{
    FileMatch, FileSearch, MAX_MATCHES, find_files, fuzzy_score, mention_withheld, read_remote_file,
};
pub use processes::Processes;
pub use runtime::{ProcessRunner, ProcessSpec, Remote, RunOutput, Runner};
pub use sandbox::{
    Sandbox, guard_from_commands, hide_from_os_sandbox, hide_loopback_port,
    leave_writable_in_guards,
};
pub use search::{GrepQuery, Scope};
pub use workspace::{GrantedPath, Workspace, plain as plain_path};
pub use writer_trees::{WriterStart, WriterTrees};
