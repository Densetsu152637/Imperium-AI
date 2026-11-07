use std::collections::BTreeMap;

use imperium_models::Pricing;
use serde::Deserialize;

use crate::SecretName;

/// Built-in defaults: the first configuration layer.
pub(crate) const DEFAULTS: &str = r#"
[daemon]
# Port for the local WebSocket listener; 0 picks a free port at each start.
port = 0

[agents]
# Model calls an agent may make in one turn before it stops.
max_steps = 50
"#;

/// Typed view of the merged configuration. Keys this build does not know are ignored, so a
/// config written for a newer version still loads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Settings {
    pub daemon: DaemonSettings,
    pub agents: AgentSettings,
    #[serde(default)]
    pub models: ModelSettings,
    /// OpenAI-compatible endpoints by id.
    #[serde(default)]
    pub endpoints: BTreeMap<String, EndpointSettings>,
    #[serde(default)]
    pub routing: RoutingSettings,
}

impl Settings {
    /// Checks rules that span keys: model table ids are unique, and routing rules name models
    /// that exist in the table.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.models.catalog {
            if !seen.insert(entry.id.as_str()) {
                return Err(format!("models.catalog: id {:?} is used twice", entry.id));
            }
        }
        for (index, rule) in self.routing.rules.iter().enumerate() {
            if let Some(unknown) = rule
                .use_models
                .iter()
                .find(|id| !seen.contains(id.as_str()))
            {
                return Err(format!(
                    "routing.rules[{index}]: {unknown:?} is not an id in models.catalog"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DaemonSettings {
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentSettings {
    pub max_steps: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ModelSettings {
    /// Model for new sessions, as `<endpoint>:<model>`, for example `openrouter:qwen/qwen3-coder`.
    #[serde(default)]
    pub default: Option<String>,
    /// Models subagents may be routed to.
    #[serde(default)]
    pub catalog: Vec<CatalogEntry>,
}

/// One row of the model table, written as `[[models.catalog]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogEntry {
    /// Short, unique name that delegation rules and agents use to refer to the model.
    pub id: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    /// Whether the model runs on this machine (or one you control) or at a hosted provider.
    pub location: Location,
    pub tier: Tier,
    /// Free-form labels, such as `code` or `vision`, that rules can match.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Where a model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    Local,
    Cloud,
}

impl Location {
    pub fn as_str(self) -> &'static str {
        match self {
            Location::Local => "local",
            Location::Cloud => "cloud",
        }
    }
}

impl std::str::FromStr for Location {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "local" => Ok(Location::Local),
            "cloud" => Ok(Location::Cloud),
            other => Err(format!("unknown location {other:?}; use local or cloud")),
        }
    }
}

/// How subagents are assigned to models.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RoutingSettings {
    /// Tie-breaker among suitable models: the cheapest, or local or cloud models first.
    #[serde(default)]
    pub prefer: Preference,
    /// Checked in order; the first rule matching a task decides its candidates.
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preference {
    #[default]
    Cheapest,
    Local,
    Cloud,
}

impl Preference {
    pub fn as_str(self) -> &'static str {
        match self {
            Preference::Cheapest => "cheapest",
            Preference::Local => "local",
            Preference::Cloud => "cloud",
        }
    }
}

/// A delegation rule, written as `[[routing.rules]]`. Absent conditions match every task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RoutingRule {
    /// Matches tasks asking for exactly this tier.
    #[serde(default)]
    pub when_tier: Option<Tier>,
    /// Matches tasks carrying this tag.
    #[serde(default)]
    pub when_tag: Option<String>,
    /// Restricts matching tasks to local or cloud models.
    #[serde(default)]
    pub location: Option<Location>,
    /// Model table ids to use, in order of preference.
    #[serde(default, rename = "use")]
    pub use_models: Vec<String>,
}

/// How capable a model is; tasks ask for a minimum tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Basic,
    #[default]
    Standard,
    Strong,
    Frontier,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Basic => "basic",
            Tier::Standard => "standard",
            Tier::Strong => "strong",
            Tier::Frontier => "frontier",
        }
    }
}

impl std::str::FromStr for Tier {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "basic" => Ok(Tier::Basic),
            "standard" => Ok(Tier::Standard),
            "strong" => Ok(Tier::Strong),
            "frontier" => Ok(Tier::Frontier),
            other => Err(format!(
                "unknown tier {other:?}; use basic, standard, strong, or frontier"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EndpointSettings {
    /// Base URL ending before `/chat/completions`, for example `https://openrouter.ai/api/v1`.
    pub base_url: String,
    /// Keychain secret holding the API key, written as `api_key = { secret = "name" }`.
    #[serde(default)]
    pub api_key: Option<SecretRef>,
    /// Prices per model id, for cost accounting. Models without prices report no cost.
    #[serde(default)]
    pub pricing: BTreeMap<String, Pricing>,
}

/// A reference to a keychain secret by name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SecretRef {
    pub secret: SecretName,
}
