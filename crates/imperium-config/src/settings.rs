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

/// A model available to the router, written as `[[models.catalog]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogEntry {
    /// `<endpoint>:<model>`.
    pub spec: String,
    pub tier: Tier,
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
