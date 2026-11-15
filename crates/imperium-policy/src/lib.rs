//! What agents may do: permission rules and modes, the safety floor, and the profile of a
//! persistent agent. The tools in `imperium-tools` ask this crate before they act, and the
//! engine and the daemon use the same decisions.

mod floor;
pub mod permissions;
pub mod profile;

pub use profile::{AgentProfile, Allowlist, ProfileRules, SHELL_TOOLS, ToolSource};
