//! Built-in agent tools. File tools are confined to a [`Workspace`] root (isolation tier 0:
//! a policy check, not an OS sandbox — commands run with the user's full rights). A remote
//! workspace runs them inside a Docker container, WSL, or over SSH ([`runtime`]).

mod exec;
mod files;
mod invocation;
pub mod permissions;
mod project_worktrees;
mod remote_files;
pub mod runtime;
mod sandbox;
mod workspace;

pub use exec::shell;
pub use invocation::{Access, Invocation, ToolError, specs};
pub use runtime::{ProcessRunner, ProcessSpec, Remote, RunOutput, Runner};
pub use sandbox::Sandbox;
pub use workspace::{GrantedPath, Workspace, plain as plain_path};
