use super::*;

// ---- Connectors: MCP servers, plugins, and skills ----

/// `mcp.list`: the MCP servers in the user config with their last known status, plus the
/// plugins and skills found in the user's folders.
#[derive(Debug)]
pub struct McpList;

impl Method for McpList {
    const NAME: &'static str = "mcp.list";
    type Params = McpListParams;
    type Result = McpListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpListResult {
    pub servers: Vec<McpServerInfo>,
    pub plugins: Vec<PluginInfo>,
    pub skills: Vec<SkillInfo>,
    // Named subagents found in the user's `agents/` folder and plugins (a project's are loaded
    // per session, for trusted projects only).
    #[serde(default)]
    pub agents: Vec<AgentFileInfo>,
    /// The user config file servers are written to.
    pub user_file: String,
    /// Problems found while reading the config, plugins, and skills.
    pub problems: Vec<String>,
}

/// One `[mcp.servers.<name>]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServerInfo {
    pub name: String,
    pub config: McpServerConfig,
    pub status: McpStatus,
}

/// A server's settings as written in the config. Secrets appear only as secret names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpServerConfig {
    /// A local command speaking MCP on stdio; exactly one of `command` and `url` is set.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Environment variables read from the secret store: variable name to secret name.
    #[serde(default)]
    pub secret_env: BTreeMap<String, String>,
    /// A streamable-HTTP endpoint.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The secret sent as `Authorization: Bearer` to an HTTP server.
    #[serde(default)]
    pub bearer_secret: Option<String>,
    /// Tools that run without asking, or `*` for all.
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub timeout_s: Option<u64>,
    /// The catalogue entry it was added from.
    #[serde(default)]
    pub catalog: Option<String>,
}

/// What the daemon last learned about a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpStatus {
    #[ts(type = "\"untested\" | \"connected\" | \"error\" | \"disabled\"")]
    pub state: String,
    /// The server's tools, from the last successful connection.
    pub tools: Vec<String>,
    pub error: Option<String>,
    /// When the status was learned, in Unix milliseconds.
    #[ts(type = "number | null")]
    pub checked_at: Option<u64>,
    /// Secrets the server refers to that are not in the secret store.
    pub missing_secrets: Vec<String>,
    /// Where a remote server's OAuth sign-in stands; `null` for servers that take no sign-in (a
    /// local command, or a URL with a fixed token or `Authorization` header).
    #[serde(default)]
    #[ts(type = "\"signed_in\" | \"signing_in\" | \"needs_sign_in\" | null")]
    pub sign_in: Option<String>,
}

/// A plugin folder (Claude Code layout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInfo {
    pub name: String,
    pub root: String,
    /// False when listed in `extensions.disabled_plugins`.
    pub enabled: bool,
    pub skills: u32,
    pub commands: u32,
    pub hooks: u32,
    /// MCP servers the plugin declares.
    pub servers: Vec<String>,
    /// Where it was installed from, when an agent installed it (`install_extension`).
    #[serde(default)]
    pub installed_from: Option<InstallSource>,
    /// `name@marketplace` for a plugin installed from a marketplace.
    #[serde(default)]
    pub id: Option<String>,
    /// Where it came from, from its record: `marketplace`, `agent`, `user` or `preexisting`;
    /// none when Imperium has no record of the folder.
    #[serde(default)]
    pub origin: Option<String>,
    /// The load-time gate withholds the whole plugin from new sessions.
    #[serde(default)]
    pub withheld: bool,
    /// Why it is withheld.
    #[serde(default)]
    pub withheld_reason: Option<String>,
}

/// A skill in the user's skills folder or a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// `user` or `plugin:<name>`.
    pub origin: String,
    pub dir: String,
    /// Where it was installed from, when an agent installed it (`install_extension`).
    #[serde(default)]
    pub installed_from: Option<InstallSource>,
}

/// A named subagent defined by a Markdown file in the user's `agents/` folder or a plugin: the
/// root agent delegates to it by `name` (`spawn_agent`'s `agent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentFileInfo {
    pub name: String,
    pub description: String,
    /// `user` or `plugin:<name>`.
    pub origin: String,
    /// The file.
    pub path: String,
    /// The tools the file narrows the agent to, as written; `null` when it takes the parent's.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// The model the file asks for (`inherit` for the delegating agent's); `null` when it leaves
    /// the choice to routing.
    #[serde(default)]
    pub model: Option<String>,
}

/// `mcp.save`: adds or replaces `[mcp.servers.<name>]` in the user config. Secret values in
/// `secrets` are stored in the secret store first, under their names; they are never written
/// to the config or returned. New sessions use the change; running sessions keep their servers.
#[derive(Debug)]
pub struct McpSave;

impl Method for McpSave {
    const NAME: &'static str = "mcp.save";
    type Params = McpSaveParams;
    type Result = McpSaveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpSaveParams {
    pub name: String,
    pub config: McpServerConfig,
    /// Secret name to value.
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    /// Refuse to replace an existing server of the same name.
    #[serde(default)]
    pub create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpSaveResult {
    pub server: McpServerInfo,
}

/// `mcp.install`: adds a server from the connector catalogue, filling its settings from
/// `values` (field id to value). Secret fields go to the secret store as `mcp.<name>.<field>`.
#[derive(Debug)]
pub struct McpInstall;

impl Method for McpInstall {
    const NAME: &'static str = "mcp.install";
    type Params = McpInstallParams;
    type Result = McpSaveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpInstallParams {
    pub catalog_id: String,
    /// The server's name; the catalogue id when empty.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

/// `mcp.remove`: removes a server from the user config.
#[derive(Debug)]
pub struct McpRemove;

impl Method for McpRemove {
    const NAME: &'static str = "mcp.remove";
    type Params = McpRemoveParams;
    type Result = McpRemoveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpRemoveParams {
    pub name: String,
    /// Also delete the secrets the server referred to.
    #[serde(default)]
    pub forget_secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpRemoveResult {
    pub removed: bool,
}

/// `mcp.set_enabled`: turns a server on or off without removing it.
#[derive(Debug)]
pub struct McpSetEnabled;

impl Method for McpSetEnabled {
    const NAME: &'static str = "mcp.set_enabled";
    type Params = McpSetEnabledParams;
    type Result = McpSaveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpSetEnabledParams {
    pub name: String,
    pub enabled: bool,
}

/// `mcp.test`: starts a server, lists its tools, and stops it. The result is also what
/// `mcp.list` shows until the next test.
#[derive(Debug)]
pub struct McpTest;

impl Method for McpTest {
    const NAME: &'static str = "mcp.test";
    type Params = McpTestParams;
    type Result = McpStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpTestParams {
    pub name: String,
}

/// `mcp.login`: starts an OAuth sign-in to a remote server. The result is the URL the user opens
/// in their own browser (Imperium never drives a browser for this); the sign-in finishes when
/// the browser returns to the daemon on `127.0.0.1`, and shows in the server's `sign_in` and
/// `status` from `mcp.list`. A second `mcp.login` for the same server replaces the first.
#[derive(Debug)]
pub struct McpLogin;

impl Method for McpLogin {
    const NAME: &'static str = "mcp.login";
    type Params = McpLoginParams;
    type Result = McpLoginResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpLoginParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpLoginResult {
    /// Where the user signs in; open it in a browser.
    pub authorization_url: String,
}

/// `mcp.logout`: signs out of a remote server: revokes its token where the server allows it and
/// forgets what the keychain held, and cancels a sign-in still waiting for the browser.
#[derive(Debug)]
pub struct McpLogout;

impl Method for McpLogout {
    const NAME: &'static str = "mcp.logout";
    type Params = McpLogoutParams;
    type Result = McpLogoutResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpLogoutParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpLogoutResult {
    /// Whether there was a sign-in to remove.
    pub signed_out: bool,
}

/// `mcp.catalog`: the connector catalogue, built in and extended or overridden by
/// `<config>/connectors.json`.
#[derive(Debug)]
pub struct McpCatalog;

impl Method for McpCatalog {
    const NAME: &'static str = "mcp.catalog";
    type Params = McpCatalogParams;
    type Result = McpCatalogResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpCatalogParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct McpCatalogResult {
    pub entries: Vec<CatalogServer>,
    /// The file that extends the built-in catalogue (it may not exist).
    pub override_file: String,
    /// Problems reading the override file.
    pub problems: Vec<String>,
}

/// A connector in the catalogue. `{{field}}` in `args`, `env` values, `url`, and `headers`
/// values is replaced by that field's value; an argument that is exactly `{{field}}` becomes
/// one argument per line of a `path_list` field, and is left out when the field is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CatalogServer {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    /// A Lucide icon name, such as `database`.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// A secret field sent as `Authorization: Bearer`, written `{{field}}`.
    #[serde(default)]
    pub bearer: Option<String>,
    #[serde(default)]
    pub fields: Vec<CatalogField>,
}

/// A setting the user fills in when adding a catalogue connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CatalogField {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// `secret` values go to the secret store and may only fill a whole `env` value or `bearer`.
    #[ts(type = "\"text\" | \"secret\" | \"path\" | \"path_list\" | \"url\"")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
}
