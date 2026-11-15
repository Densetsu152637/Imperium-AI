use super::*;

/// `api.get`: the OpenAI-compatible local API server's state and the models it serves.
#[derive(Debug)]
pub struct ApiGet;

impl Method for ApiGet {
    const NAME: &'static str = "api.get";
    type Params = ApiGetParams;
    type Result = ApiStatus;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiGetParams {}

/// `api.set`: changes the API server's settings (saved to the user config) and applies them at
/// once: enabling starts the server, disabling stops it. The first enable picks and saves a
/// free port. A `bind` is refused unless it is loopback or `allow_lan` is set.
#[derive(Debug)]
pub struct ApiSet;

impl Method for ApiSet {
    const NAME: &'static str = "api.set";
    type Params = ApiSetParams;
    type Result = ApiStatus;
}

/// What to change; absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiSetParams {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 0 to have a free port chosen.
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub bind: Option<String>,
    #[serde(default)]
    pub allow_lan: Option<bool>,
    /// Specs of the hosted models to serve (replaces the list).
    #[serde(default)]
    pub expose: Option<Vec<String>>,
}

/// `api.token`: the API token, made on first use. With `rotate` a new one replaces it, and the
/// old one stops working at once. The token is kept in the secret store, never in the config.
#[derive(Debug)]
pub struct ApiToken;

impl Method for ApiToken {
    const NAME: &'static str = "api.token";
    type Params = ApiTokenParams;
    type Result = ApiTokenResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiTokenParams {
    #[serde(default)]
    pub rotate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiTokenResult {
    pub token: String,
}

/// The API server as `api.get` and `api.set` report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiStatus {
    pub enabled: bool,
    /// Whether the server is listening now.
    pub running: bool,
    pub bind: String,
    /// 0 until a port is chosen.
    pub port: u16,
    pub allow_lan: bool,
    /// Where other tools point at (`http://127.0.0.1:<port>/v1`), while the server runs.
    pub base_url: Option<String>,
    /// The hosted models' specs the user chose to serve.
    pub expose: Vec<String>,
    /// Why the server is not running although it is enabled.
    pub error: Option<String>,
    /// Every model the server serves or could serve with an opt-in.
    pub models: Vec<ApiModel>,
}

/// A model the API server serves, or could.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiModel {
    /// The id other tools use: the model's spec.
    pub id: String,
    /// Its alias, else its name.
    pub name: String,
    /// A hosted model spends the user's keys and money; it is served only when listed in
    /// `expose`.
    pub hosted: bool,
    /// Whether the server serves it now.
    pub exposed: bool,
}
