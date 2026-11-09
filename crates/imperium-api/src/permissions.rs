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
