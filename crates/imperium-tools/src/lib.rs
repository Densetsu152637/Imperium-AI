//! Built-in agent tools. File tools are confined to a [`Workspace`] root (isolation tier 0:
//! a policy check, not an OS sandbox — commands run with the user's full rights).

mod exec;
mod files;
mod invocation;
mod workspace;

pub use invocation::{Access, Invocation, ToolError, specs};
pub use workspace::Workspace;
