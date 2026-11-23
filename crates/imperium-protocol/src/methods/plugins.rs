use super::*;

// ---- Plugin marketplaces ----

/// `plugins.marketplace_add`: adds a marketplace from a git URL, a folder on the daemon's
/// machine (an absolute path, or one starting with `~`, read in place) or `owner/repo` on
/// GitHub (two name segments and no scheme; a folder of that name that exists wins, and the
/// result's `kind` says which was chosen). The source is cloned (or read), its
/// `.claude-plugin/marketplace.json` parsed, and the marketplace named by that file is written to
/// `[plugins.marketplaces.<name>]` in the user's config. Nothing in it runs. A clone can take
/// minutes, so the answer comes when it is done. A name already added, or the official
/// marketplace's name with another source, is refused.
#[derive(Debug)]
pub struct PluginsMarketplaceAdd;

impl Method for PluginsMarketplaceAdd {
    const NAME: &'static str = "plugins.marketplace_add";
    type Params = PluginsMarketplaceAddParams;
    type Result = MarketplaceInfo;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsMarketplaceAddParams {
    pub source: String,
    // A branch, tag, or commit (git only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revision: Option<String>,
}

/// How a marketplace's source was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MarketplaceKind {
    Github,
    Git,
    Folder,
}

/// A marketplace the daemon knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MarketplaceInfo {
    pub name: String,
    // The source as configured.
    pub source: String,
    pub revision: Option<String>,
    pub kind: MarketplaceKind,
    // Its source is the one built into Imperium (Anthropic's official marketplace).
    pub built_in: bool,
    pub enabled: bool,
    // The commit of the last clone; none for a folder or before the first clone.
    pub commit: Option<String>,
    // When it was last read or refreshed; none before the first.
    #[ts(type = "number | null")]
    pub refreshed_unix_ms: Option<u64>,
    // How many plugins it lists; none before it has been read.
    pub plugin_count: Option<u32>,
    // Entries left out of the listing and why, or why the file could not be read.
    pub problems: Vec<String>,
}

/// `plugins.marketplace_list`: the marketplaces, the built-in one included. Reads no network:
/// the official marketplace is listed before it was ever cloned, with no commit or count.
#[derive(Debug)]
pub struct PluginsMarketplaceList;

impl Method for PluginsMarketplaceList {
    const NAME: &'static str = "plugins.marketplace_list";
    type Params = PluginsMarketplaceListParams;
    type Result = PluginsMarketplaceListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsMarketplaceListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsMarketplaceListResult {
    pub marketplaces: Vec<MarketplaceInfo>,
}

/// `plugins.marketplace_refresh`: fetches a marketplace again (every enabled one without a
/// `name`) and reports which plugins were added, removed or changed since the last read. A
/// marketplace that cannot be refreshed keeps its last copy and carries an `error`.
#[derive(Debug)]
pub struct PluginsMarketplaceRefresh;

impl Method for PluginsMarketplaceRefresh {
    const NAME: &'static str = "plugins.marketplace_refresh";
    type Params = PluginsMarketplaceRefreshParams;
    type Result = PluginsMarketplaceRefreshResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsMarketplaceRefreshParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsMarketplaceRefreshResult {
    pub refreshed: Vec<MarketplaceRefresh>,
}

/// What a refresh found in one marketplace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MarketplaceRefresh {
    pub name: String,
    pub commit_before: Option<String>,
    pub commit_after: Option<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub problems: Vec<String>,
    // Why it could not be refreshed.
    pub error: Option<String>,
}

/// `plugins.marketplace_remove`: removes a marketplace the user added, with its cached clone.
/// The built-in marketplace is turned off (`enabled = false`), not freed: its name stays
/// reserved.
#[derive(Debug)]
pub struct PluginsMarketplaceRemove;

impl Method for PluginsMarketplaceRemove {
    const NAME: &'static str = "plugins.marketplace_remove";
    type Params = PluginsMarketplaceRemoveParams;
    type Result = PluginsMarketplaceRemoveResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsMarketplaceRemoveParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsMarketplaceRemoveResult {
    pub name: String,
    // The built-in marketplace was turned off instead of removed.
    pub disabled: bool,
}

/// `plugins.browse`: the plugins the enabled marketplaces list (one marketplace with
/// `marketplace`), narrowed by `query` (words matched against name, description, keywords, tags
/// and category). Reads the cached clone, making it first when there is none (a first browse of
/// the official marketplace); runs nothing from it.
#[derive(Debug)]
pub struct PluginsBrowse;

impl Method for PluginsBrowse {
    const NAME: &'static str = "plugins.browse";
    type Params = PluginsBrowseParams;
    type Result = PluginsBrowseResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsBrowseParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub marketplace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsBrowseResult {
    pub plugins: Vec<PluginListing>,
    // Marketplaces that could not be read, with the reason.
    pub problems: Vec<String>,
}

/// One plugin a marketplace lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginListing {
    // `name@marketplace`.
    pub id: String,
    pub name: String,
    pub marketplace: String,
    pub description: String,
    pub version: Option<String>,
    // The plugin's own claims about itself, unverified.
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub category: Option<String>,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    // `path` (inside the marketplace), `github`, `url` or `unsupported`.
    pub source_kind: String,
    // The folder, `owner/repo`, URL or source type.
    pub source: String,
    // What the plugin holds, read from the marketplace's own copy; none when its source is
    // elsewhere.
    pub components: Option<PluginComponents>,
}

/// What a plugin inside a marketplace's repository holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginComponents {
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub agents: Vec<String>,
    pub hooks: u32,
    pub mcp_servers: Vec<String>,
}
