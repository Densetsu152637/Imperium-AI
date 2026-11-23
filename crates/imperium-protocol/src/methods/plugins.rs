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

// ---- Inspecting and installing plugins ----

/// `plugins.inspect`: fetches one plugin of a marketplace into a staging folder and says exactly
/// what it would run, with the content hash to pass to `plugins.install`. Nothing in it runs: no
/// hook, server, build step, install script or git hook. `revision` is a branch, tag or commit
/// for a plugin that lives in its own git repository (a plugin inside the marketplace's
/// repository has none).
#[derive(Debug)]
pub struct PluginsInspect;

impl Method for PluginsInspect {
    const NAME: &'static str = "plugins.inspect";
    type Params = PluginsInspectParams;
    type Result = PluginPreview;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsInspectParams {
    // `name@marketplace`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revision: Option<String>,
}

/// What `plugins.inspect` found: what the trust prompt shows before a plugin is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginPreview {
    // `name@marketplace`.
    pub id: String,
    pub name: String,
    pub marketplace: String,
    // The marketplace's source as configured, and the commit its clone is at.
    pub marketplace_source: String,
    pub marketplace_commit: Option<String>,
    // `path` (inside the marketplace's repository), `github` or `url`.
    pub source_kind: String,
    // The folder inside the marketplace, or the git URL.
    pub source: String,
    // The exact commit the plugin was fetched at (a path plugin has the marketplace's).
    pub commit: Option<String>,
    // The plugin's own claims about itself, unverified.
    pub description: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    // Every hook, with `${CLAUDE_PLUGIN_ROOT}` expanded to where the plugin would be installed.
    pub hooks: Vec<PluginHookRun>,
    pub servers: Vec<PluginServerRun>,
    // A hook or server starts a launcher that fetches more code (`npx`, `uvx`, `@latest`). A
    // missing flag is no guarantee: any script can download and run code on its own.
    pub launcher: bool,
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub agents: Vec<String>,
    // Commands with `!` shell lines or `allowed-tools`, which Imperium does not run itself.
    pub commands_with_shell: Vec<String>,
    // SHA-256 over the plugin folder's files: pass it to `plugins.install`.
    pub sha256: String,
    // The digest of what the plugin would run, which an approval is bound to.
    pub runs_digest: String,
    // What is installed under the plugin's name now, if anything.
    pub installed: Option<PluginInstalled>,
    // Problems found reading it.
    pub notes: Vec<String>,
}

/// One hook a plugin would run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginHookRun {
    pub event: String,
    pub matcher: String,
    // The full command line; empty for an HTTP hook.
    pub command: String,
    pub url: Option<String>,
    pub launcher: bool,
}

/// One MCP server a plugin would start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginServerRun {
    // `<plugin>_<server>`.
    pub name: String,
    // `stdio` or `http`.
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env_names: Vec<String>,
    pub url: Option<String>,
    pub header_names: Vec<String>,
    // `${VAR}` references other than `CLAUDE_PLUGIN_ROOT`.
    pub variables: Vec<String>,
    pub launcher: bool,
}

/// What is already installed under a plugin's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginInstalled {
    pub path: String,
    // The recorded content hash, when Imperium has a record of it.
    pub recorded_sha256: Option<String>,
    // The installed content is the content inspected.
    pub same_content: bool,
}

/// `plugins.install`: fetches the plugin again, refuses it if its content hash is not `sha256`
/// (what `plugins.inspect` showed), a folder of that name exists (compared ignoring letter
/// case), its manifest name differs from the marketplace entry's, or the source holds a link, then
/// installs it under `<config>/plugins/<name>` and records it. It never approves: a plugin with
/// hooks or MCP servers is installed withheld until the desktop app approves it.
#[derive(Debug)]
pub struct PluginsInstall;

impl Method for PluginsInstall {
    const NAME: &'static str = "plugins.install";
    type Params = PluginsInstallParams;
    type Result = PluginsInstallResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsInstallParams {
    // `name@marketplace`.
    pub id: String,
    pub sha256: String,
    // The same `revision` the inspection was given, if it was given one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revision: Option<String>,
    // Not used until plugins can be turned on per environment; noted in the result when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment: Option<String>,
    // Never read: an install cannot approve. Noted in the result when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(skip)]
    pub approve: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsInstallResult {
    pub name: String,
    pub path: String,
    // The plugin is installed but does not load (it has hooks or servers and no approval).
    pub withheld: bool,
    pub reason: Option<String>,
    pub notes: Vec<String>,
}

// ---- Listing and uninstalling plugins ----

/// `plugins.list`: every folder under `<config>/plugins` that holds a plugin, recorded or not,
/// with where it came from, its approval kind and whether the load-time gate withholds it (and
/// why). A `dev` approval is listed as `dev`, whether or not this build honours it.
#[derive(Debug)]
pub struct PluginsList;

impl Method for PluginsList {
    const NAME: &'static str = "plugins.list";
    type Params = PluginsListParams;
    type Result = PluginsListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsListParams {
    // An environment (id or name): `enabled` and `decided_by` are for its sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsListResult {
    pub plugins: Vec<InstalledPlugin>,
    // Folders that are not plugins, and records that could not be read.
    pub problems: Vec<String>,
}

/// One plugin folder under `<config>/plugins`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InstalledPlugin {
    pub name: String,
    pub folder: String,
    // `name@marketplace` for a marketplace install.
    pub id: Option<String>,
    // `marketplace`, `agent`, `user` or `preexisting`; none when there is no record.
    pub origin: Option<String>,
    // The marketplace's source (a folder or git URL) and the commit installed, from the record.
    pub source: Option<String>,
    pub commit: Option<String>,
    // `counted`, `dialog-only`, `carried-over` or `dev`; none when nothing was approved.
    pub approval: Option<String>,
    // The plugin is turned on (the switch only: a withheld plugin is off whatever this says).
    pub enabled: bool,
    // What decided `enabled`: `default`, `user` (`extensions.disabled_plugins`), `environment`
    // (the environment's lists) or `repository` (a trusted repository's file).
    pub decided_by: String,
    // The gate withholds the whole plugin from new sessions.
    pub withheld: bool,
    pub reason: Option<String>,
    pub hooks: u32,
    pub servers: Vec<String>,
    // The `${VAR}` names its servers need a saved secret for (only for a plugin installed from
    // a marketplace or by an agent), and which of them have none yet.
    pub required_secrets: Vec<String>,
    pub missing_secrets: Vec<String>,
}

/// `plugins.uninstall`: moves a plugin's folder to `<data>/config-trash` and removes its record,
/// journalled so `config.undo` puts both back. `name` is the plugin's folder name or recorded name
/// or `name@marketplace` (letter case does not matter). Its secrets are not touched.
#[derive(Debug)]
pub struct PluginsUninstall;

impl Method for PluginsUninstall {
    const NAME: &'static str = "plugins.uninstall";
    type Params = PluginsUninstallParams;
    type Result = PluginsUninstallResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsUninstallParams {
    pub name: String,
    // Also delete the plugin's saved secrets (the folder's `${VAR}` names).
    #[serde(default)]
    pub delete_secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsUninstallResult {
    pub name: String,
    // The folder it was removed from.
    pub path: String,
    // Where the folder is kept.
    pub kept_at: String,
    // The `config.changes` entry that `config.undo` takes.
    #[ts(type = "number")]
    pub change_id: u64,
    pub notes: Vec<String>,
}

// ---- Turning plugins on and off, and updating them ----

/// `plugins.set_enabled`: turns a plugin on or off for the user (`extensions.disabled_plugins`) or,
/// with `environment`, for one environment (its `plugins` lists). Enabling a plugin the load-time
/// gate withholds is refused with the reason; turning a plugin off and on again never revokes
/// its approval. A trusted repository's file that disables the plugin still wins.
#[derive(Debug)]
pub struct PluginsSetEnabled;

impl Method for PluginsSetEnabled {
    const NAME: &'static str = "plugins.set_enabled";
    type Params = PluginsSetEnabledParams;
    type Result = PluginsSetEnabledResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsSetEnabledParams {
    pub name: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsSetEnabledResult {
    pub name: String,
    // The plugin's switch afterwards, for the user or the environment asked about.
    pub enabled: bool,
    pub decided_by: String,
    pub notes: Vec<String>,
}

/// `plugins.update`: fetches a marketplace plugin again from the source its record names (never
/// from the folder) and replaces the installed folder when the content hash is `sha256` (what
/// `plugins.inspect` showed) and differs from the installed one. The previous folder is kept in
/// `<data>/config-trash`. It never approves: new content with hooks or servers is withheld until
/// approved afresh. An equal hash answers `up_to_date`; another marketplace or source is refused.
#[derive(Debug)]
pub struct PluginsUpdate;

impl Method for PluginsUpdate {
    const NAME: &'static str = "plugins.update";
    type Params = PluginsUpdateParams;
    type Result = PluginsUpdateResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsUpdateParams {
    // The plugin's name, or `name@marketplace` (which must be the marketplace it came from).
    pub name: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsUpdateResult {
    pub name: String,
    pub path: String,
    // The installed content already has the hash: nothing was changed.
    pub up_to_date: bool,
    // The folder was replaced.
    pub replaced: bool,
    // Where the previous folder is kept.
    pub kept_at: Option<String>,
    pub withheld: bool,
    pub reason: Option<String>,
    pub notes: Vec<String>,
}

// ---- Plugin secrets ----

/// `plugins.set_secret`: saves the value of one `${VAR}` a plugin's server needs in the secret
/// store, as `plugin.<plugin>.<var>` (lowercased). Only a plugin installed from a marketplace or
/// by an agent reads saved secrets, and only a variable its servers name. The value is never
/// returned, logged or written anywhere but the secret store. Setting a secret is not an approval.
#[derive(Debug)]
pub struct PluginsSetSecret;

impl Method for PluginsSetSecret {
    const NAME: &'static str = "plugins.set_secret";
    type Params = PluginsSetSecretParams;
    type Result = PluginsSecretResult;
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsSetSecretParams {
    pub name: String,
    pub variable: String,
    pub value: String,
}

// The value stays out of any `{:?}` of the request.
impl std::fmt::Debug for PluginsSetSecretParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginsSetSecretParams")
            .field("name", &self.name)
            .field("variable", &self.variable)
            .field("value", &"<hidden>")
            .finish()
    }
}

/// `plugins.delete_secret`: removes one saved secret of a plugin.
#[derive(Debug)]
pub struct PluginsDeleteSecret;

impl Method for PluginsDeleteSecret {
    const NAME: &'static str = "plugins.delete_secret";
    type Params = PluginsDeleteSecretParams;
    type Result = PluginsSecretResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsDeleteSecretParams {
    pub name: String,
    pub variable: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsSecretResult {
    pub name: String,
    pub variable: String,
    // The secret store's name for it, `plugin.<plugin>.<var>`.
    pub secret: String,
    // For a delete: whether there was one.
    pub changed: bool,
}

// ---- Repository suggestions ----

/// `plugins.suggestions`: the marketplaces and plugins a trusted project's own
/// `.imperium/config.toml` offers (`[plugins] suggest_marketplaces`, `suggest_plugins`). The text
/// of an offer is **repository data**: show it to the user as text, never run it, and never give
/// it to a model. An offer adds, fetches, installs and enables nothing.
#[derive(Debug)]
pub struct PluginsSuggestions;

impl Method for PluginsSuggestions {
    const NAME: &'static str = "plugins.suggestions";
    type Params = PluginsSuggestionsParams;
    type Result = PluginsSuggestionsResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsSuggestionsParams {
    // An environment (id or name).
    pub environment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsSuggestionsResult {
    // The environment's project folder the offers come from, canonical; none when it has none.
    pub source_folder: Option<String>,
    // The folder is in `mcp.trusted_projects`. Offers of an untrusted folder are refused (see
    // `environments.config`) and none are listed.
    pub trusted: bool,
    // The user muted this folder ("no suggestions from this project"): nothing is offered.
    pub muted: bool,
    pub marketplaces: Vec<SuggestedMarketplace>,
    pub plugins: Vec<SuggestedPlugin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SuggestedMarketplace {
    // What `plugins.dismiss_suggestion` takes.
    pub key: String,
    // A git URL, `owner/repo` or a folder, as the repository wrote it (untrusted text).
    pub source: String,
    pub revision: Option<String>,
    // The file the offer is from.
    pub file: String,
    // A marketplace with this source is already added.
    pub added: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SuggestedPlugin {
    // What `plugins.dismiss_suggestion` takes.
    pub key: String,
    // `name@marketplace`, as the repository wrote it (untrusted text).
    pub id: String,
    pub file: String,
    // The marketplace the id names is already added.
    pub marketplace_added: bool,
    // A plugin with this id is already installed.
    pub installed: bool,
}

/// `plugins.dismiss_suggestion`: dismisses one offer (`key`), or, with `source_folder` (the
/// canonical folder `plugins.suggestions` gave), mutes every suggestion from that project. Give
/// exactly one of the two. Neither adds, fetches, installs or enables anything.
#[derive(Debug)]
pub struct PluginsDismissSuggestion;

impl Method for PluginsDismissSuggestion {
    const NAME: &'static str = "plugins.dismiss_suggestion";
    type Params = PluginsDismissSuggestionParams;
    type Result = PluginsDismissSuggestionResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsDismissSuggestionParams {
    pub environment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PluginsDismissSuggestionResult {
    // The offer was dismissed, or the project muted.
    pub dismissed: bool,
    pub muted: bool,
}
