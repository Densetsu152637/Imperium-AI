//! Built-in agent tools. File tools are confined to a [`Workspace`] root (isolation tier 0:
//! a policy check, not an OS sandbox — commands run with the user's full rights). A remote
//! workspace runs them inside a Docker container, WSL, or over SSH ([`runtime`]).

mod exec;
mod files;
mod floor;
mod invocation;
pub mod permissions;
mod processes;
pub mod profile;
mod project_worktrees;
mod remote_files;
pub mod runtime;
mod sandbox;
mod search;
mod workspace;
mod writer_trees;

pub use exec::shell;
pub use invocation::{Access, Invocation, ToolError, specs};
pub use processes::Processes;
pub use profile::{AgentProfile, Allowlist, ProfileRules, SHELL_TOOLS, ToolSource};
pub use runtime::{ProcessRunner, ProcessSpec, Remote, RunOutput, Runner};
pub use sandbox::Sandbox;
pub use search::{GrepQuery, Scope};
pub use workspace::{GrantedPath, Workspace, plain as plain_path};
pub use writer_trees::{WriterStart, WriterTrees};
