//! Permission vocabulary shared by the configuration, the tools, and the agent loop: path
//! grants outside an environment's folders, and allow / ask / deny rules on tool calls. The
//! matching lives in `imperium-tools`; these are only the shapes, as written in `config.toml`
//! and `environments.json`.

use serde::{Deserialize, Serialize};

/// How much a grant opens a path outside the environment's folders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMode {
    /// Readable and listable; writes and edits are refused.
    #[default]
    Read,
    /// Readable and writable, like one of the environment's folders.
    ReadWrite,
}

/// A path outside the environment's folders that the agent's tools may use, such as a global
/// package cache or a toolchain. `~` at the start means the home folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PathGrant {
    pub path: String,
    #[serde(default)]
    pub mode: PathMode,
}

/// What a matching rule does with a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// Run without asking.
    Allow,
    /// Ask the user, even for a call that would otherwise run without asking.
    Ask,
    /// Refuse without asking.
    Deny,
}

/// A permission rule: a tool name (`*` wildcards allowed, `*` alone for every tool) and an
/// optional argument pattern: a command prefix for `run_command`, a path glob for the file
/// tools. An empty pattern matches every call of the tool.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    #[serde(default)]
    pub pattern: String,
    pub action: RuleAction,
}

/// How a session's calls are approved, beside the rules. Deny rules and the built-in safety
/// floor apply in every mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Ask before every call that is not read-only, whatever the allow rules say.
    Ask,
    /// Reads run; writes, edits, and commands ask unless a rule allows them.
    #[default]
    Default,
    /// File writes and edits run without asking; commands follow the rules.
    AcceptEdits,
    /// Read and plan only: writes, edits, commands, and tools that would ask are refused.
    Plan,
    /// Everything runs without asking, except what deny rules, ask rules, or the safety floor
    /// stop.
    Full,
}

impl PermissionMode {
    /// Every mode, most cautious first.
    pub const ALL: [PermissionMode; 5] = [
        PermissionMode::Plan,
        PermissionMode::Ask,
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::Full,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Ask => "ask",
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "accept_edits",
            PermissionMode::Plan => "plan",
            PermissionMode::Full => "full",
        }
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A permission mode that is not one of [`PermissionMode::ALL`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMode(pub String);

impl std::fmt::Display for UnknownMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown permission mode {:?}; use ask, default, accept_edits, plan or full",
            self.0
        )
    }
}

impl std::error::Error for UnknownMode {}

impl std::str::FromStr for PermissionMode {
    type Err = UnknownMode;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "ask" => Ok(PermissionMode::Ask),
            "default" => Ok(PermissionMode::Default),
            "accept_edits" | "edits" => Ok(PermissionMode::AcceptEdits),
            "plan" => Ok(PermissionMode::Plan),
            "full" | "full_access" => Ok(PermissionMode::Full),
            _ => Err(UnknownMode(text.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip_through_their_names() {
        for mode in PermissionMode::ALL {
            assert_eq!(mode.as_str().parse::<PermissionMode>(), Ok(mode));
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{}\"", mode.as_str())
            );
        }
        assert_eq!("Accept-Edits".parse(), Ok(PermissionMode::AcceptEdits));
        assert!("yolo".parse::<PermissionMode>().is_err());
    }
}
