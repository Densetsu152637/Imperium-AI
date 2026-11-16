//! Permission rules and path grants: the `permissions.*` methods and the shapes environments
//! and approval requests carry (see docs/05-tools-and-isolation.md#permissions).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::methods::Method;

/// How much a grant opens a path outside an environment's folders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PathMode {
    /// Readable and listable; writes and edits are refused.
    #[default]
    Read,
    /// Readable and writable, like one of the environment's folders.
    ReadWrite,
}

/// A path outside the environment's folders the tools may use. `~` at the start means the home
/// folder; for a remote runtime the path is inside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PathGrant {
    pub path: String,
    #[serde(default)]
    pub mode: PathMode,
}

/// What a matching rule does with a tool call. Deny wins over ask, and ask over allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RuleAction {
    Allow,
    Ask,
    Deny,
}

/// A rule on tool calls: the tool's name (`*` wildcards allowed; `*` alone is every tool) and a
/// pattern: a command prefix for `run_command`, a path glob for the file tools. An empty pattern
/// matches every call of the tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionRule {
    pub tool: String,
    #[serde(default)]
    pub pattern: String,
    pub action: RuleAction,
}

/// Path grants and rules at one scope (global, or an environment's).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionSet {
    #[serde(default)]
    pub grants: Vec<PathGrant>,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
}

/// A common path to grant in one click, resolved for the daemon's operating system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PathPreset {
    /// A stable id, such as `cargo`.
    pub id: String,
    pub label: String,
    /// The path on the daemon's machine.
    pub path: String,
    /// The mode the preset suggests: writable for caches, read-only for toolchains.
    pub mode: PathMode,
    /// Whether the path exists now (grants of missing paths are skipped).
    pub exists: bool,
}

/// `permissions.get`: the global grants and rules (from the user config) and the presets.
#[derive(Debug)]
pub struct PermissionsGet;

impl Method for PermissionsGet {
    const NAME: &'static str = "permissions.get";
    type Params = PermissionsGetParams;
    type Result = PermissionsResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionsGetParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionsResult {
    pub global: PermissionSet,
    pub presets: Vec<PathPreset>,
}

/// `permissions.set`: replaces the global grants and rules in the user config. Rules apply to
/// running sessions from their next call; grants to sessions started afterwards.
#[derive(Debug)]
pub struct PermissionsSet;

impl Method for PermissionsSet {
    const NAME: &'static str = "permissions.set";
    type Params = PermissionSet;
    type Result = PermissionsResult;
}

/// `permissions.add_rule`: adds one rule (replacing a rule for the same tool and pattern), as
/// "always allow" on an approval does. The scope is `environment_id` when given, else the
/// environment of `session_id`, else global.
#[derive(Debug)]
pub struct PermissionsAddRule;

impl Method for PermissionsAddRule {
    const NAME: &'static str = "permissions.add_rule";
    type Params = PermissionAddRuleParams;
    type Result = PermissionAddRuleResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionAddRuleParams {
    pub rule: PermissionRule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionAddRuleResult {
    /// The environment the rule was added to; `None` for a global rule.
    pub environment_id: Option<String>,
}
