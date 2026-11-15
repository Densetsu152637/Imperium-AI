use super::*;

/// `config.describe`: the settings an app may show and change, with their effective values.
/// Structured lists and maps (endpoints, the model table, routing rules, MCP servers, projects,
/// schedules, hooks) are not included; they are edited in the config file.
#[derive(Debug)]
pub struct ConfigDescribe;

impl Method for ConfigDescribe {
    const NAME: &'static str = "config.describe";
    type Params = ConfigDescribeParams;
    type Result = ConfigDescribeResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigDescribeParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigDescribeResult {
    pub entries: Vec<ConfigEntry>,
    /// The user config file that `config.set` writes.
    pub user_file: String,
}

/// One setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigEntry {
    /// Dotted path, such as `runtime.llamacpp.context`.
    pub key: String,
    /// The top-level table, for grouping.
    pub section: String,
    pub label: String,
    pub description: String,
    #[ts(
        type = "\"bool\" | \"integer\" | \"number\" | \"string\" | \"path\" | \"path_list\" | \"string_list\" | \"enum\" | \"secret_ref\""
    )]
    pub kind: String,
    /// Allowed values of an `enum`; empty otherwise.
    pub options: Vec<String>,
    /// The built-in default; null when unset by default.
    #[ts(type = "unknown")]
    pub default: serde_json::Value,
    /// The effective value. A `secret_ref` shows the secret's name, never its value.
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
    /// The layer that set `value`.
    #[ts(type = "\"default\" | \"user\" | \"app\" | \"project\" | \"env\"")]
    pub source: String,
    /// Whether the daemon must restart before a change applies.
    pub restart: bool,
}

/// `config.set`: writes one setting to the user config file (never a project's), or removes it
/// with a null value so the default applies. The whole configuration is validated first; an
/// invalid change is not saved.
#[derive(Debug)]
pub struct ConfigSet;

impl Method for ConfigSet {
    const NAME: &'static str = "config.set";
    type Params = ConfigSetParams;
    type Result = ConfigSetResult;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigSetParams {
    pub key: String,
    /// The new value, matching the entry's kind; null removes the key from the user file.
    #[serde(default)]
    #[ts(type = "unknown")]
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigSetResult {
    /// The setting after the change, as the daemon will load it at its next start.
    pub entry: ConfigEntry,
    /// Restart the daemon to apply the change.
    pub restart: bool,
    /// An environment variable (or project file) sets this key, so the saved value does not
    /// take effect while it does.
    pub overridden: bool,
}

/// `personalities.list`: the personalities `policy.personality` may name, for a picker. Local
/// personalities (`<config>/personalities/`) come first in precedence, then workflow packs',
/// then the built-in ones; each id is listed once, as the one that wins.
#[derive(Debug)]
pub struct PersonalitiesList;

impl Method for PersonalitiesList {
    const NAME: &'static str = "personalities.list";
    type Params = PersonalitiesListParams;
    type Result = PersonalitiesListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PersonalitiesListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PersonalitiesListResult {
    /// Sorted by name.
    pub personalities: Vec<PersonalitySummary>,
    /// The id used when `policy.personality` is unset or names one that is not found.
    pub default: String,
}

/// One selectable personality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PersonalitySummary {
    /// The file stem, which `policy.personality` takes.
    pub id: String,
    /// Its display name, from the file's heading.
    pub name: String,
    /// Where the winning file is.
    #[ts(type = "\"local\" | \"pack\" | \"builtin\"")]
    pub source: String,
    /// For `pack`: the pack's folder name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pack: Option<String>,
    /// A local personality that hides a pack or built-in one with the same id.
    pub overrides: bool,
}
