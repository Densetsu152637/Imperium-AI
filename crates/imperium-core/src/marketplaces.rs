//! `plugins.marketplace_*` and `plugins.browse`: the plugin marketplaces the user (or Imperium,
//! for the official one) lists, and what they offer.
//!
//! A marketplace is `[plugins.marketplaces.<name>]` in the user's config. A git source is cloned
//! into `<data>/marketplaces/<name>/` (made on the first browse or refresh, replaced as a whole
//! when a refresh finds a new commit); a folder is read in place. The daemon's own record of what
//! it read (commit, time, a hash of each entry, problems) is `<data>/marketplaces.json`.
//!
//! Adding, refreshing and browsing fetch and parse; none of them runs a hook, a server, a build
//! step, an install script or a git hook (`extension_install`'s git call turns the `ext::`
//! transport off and never prompts). A plugin's files are not fetched here at all: browsing
//! reads only the marketplace's own repository. The official marketplace is built into the
//! settings, so listing it needs no network.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use imperium_api::encoding::hex;
use imperium_api::time::unix_ms;
use imperium_config::{
    MarketplaceSettings, OFFICIAL_MARKETPLACE, OFFICIAL_MARKETPLACE_SOURCE, Settings,
    same_marketplace_source,
};
use imperium_extensions::marketplace::{self, Entry, Marketplace, Source as PluginSource};
use imperium_extensions::plugins::{Declared, inside};
use imperium_protocol::methods::{
    MarketplaceInfo, MarketplaceKind, MarketplaceRefresh, PluginComponents, PluginListing,
    PluginsBrowseParams, PluginsBrowseResult, PluginsMarketplaceAddParams,
    PluginsMarketplaceListResult, PluginsMarketplaceRefreshParams, PluginsMarketplaceRefreshResult,
    PluginsMarketplaceRemoveParams, PluginsMarketplaceRemoveResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::extension_install::{self, Staging, move_tree, remove_tree};
use crate::state::DaemonState;

/// What the daemon last read of one marketplace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    /// The commit of the clone; none for a folder.
    commit: Option<String>,
    read_unix_ms: u64,
    /// SHA-256 of each entry's canonical JSON, by plugin name: what a refresh compares.
    plugins: BTreeMap<String, String>,
    problems: Vec<String>,
}

type Store = BTreeMap<String, Record>;

/// Serialises read-modify-write of the record file.
static STORE_LOCK: Mutex<()> = Mutex::new(());

/// One network operation at a time: two clones of the same marketplace would race on its folder.
static OPERATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn store_path(state: &DaemonState) -> PathBuf {
    state.paths().data_dir().join("marketplaces.json")
}

fn clones_dir(state: &DaemonState) -> PathBuf {
    state.paths().data_dir().join("marketplaces")
}

/// The folder name a marketplace's clone is kept under: its name in lower case, so two names
/// that differ only by letter case (one folder on Windows and macOS) can never share a clone,
/// and a digest for a name that is not a safe folder name at all (it cannot get here from the
/// settings, which refuse such names; this keeps the path inside the clones folder regardless).
fn clone_key(name: &str) -> String {
    if imperium_config::valid_marketplace_name(name) {
        name.to_ascii_lowercase()
    } else {
        format!("invalid-{}", &hex(&Sha256::digest(name.as_bytes()))[..16])
    }
}

fn clone_dir(state: &DaemonState, name: &str) -> PathBuf {
    clones_dir(state).join(clone_key(name))
}

/// The records: empty when there is no file, or when it cannot be read (it is only a cache of
/// what the marketplaces said, rebuilt by the next refresh).
fn load_store(state: &DaemonState) -> Store {
    match std::fs::read_to_string(store_path(state)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(%error, "the marketplace record file is corrupt");
            Store::new()
        }),
        Err(_) => Store::new(),
    }
}

/// Changes the records under the lock and writes them back.
fn update_store(state: &DaemonState, change: impl FnOnce(&mut Store)) -> Result<(), String> {
    let _guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = load_store(state);
    change(&mut store);
    let path = store_path(state);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&store).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text).map_err(|error| format!("{}: {error}", path.display()))?;
    std::fs::rename(&temporary, &path).map_err(|error| format!("{}: {error}", path.display()))
}

/// The settings as the files and environment say now (the daemon's own copy is from its start).
fn current_settings(state: &DaemonState) -> Result<Settings, String> {
    imperium_config::load(state.paths(), None, std::env::vars())
        .and_then(|config| config.settings())
        .map_err(|error| error.to_string())
}

/// Where a marketplace is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    Git(String),
    Folder(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Resolved {
    kind: MarketplaceKind,
    place: Place,
}

/// `owner/repo`: two name segments, no scheme, not a path.
fn is_shorthand(source: &str) -> bool {
    let part = |text: &str| {
        !text.is_empty()
            && !text.starts_with(['.', '-'])
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    source
        .split_once('/')
        .is_some_and(|(owner, repo)| part(owner) && part(repo))
}

fn github(source: &str) -> Resolved {
    Resolved {
        kind: MarketplaceKind::Github,
        place: Place::Git(format!("https://github.com/{source}.git")),
    }
}

/// A folder as text, without the `\\?\` prefix `canonicalize` gives on Windows.
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\")
        .map_or(text.clone(), str::to_string)
}

/// Reads a source. With a `base`, a folder (relative ones against it) that exists wins over
/// `owner/repo`; without one (a source already configured) `owner/repo` is always GitHub, so a
/// folder that appears later cannot take over a configured marketplace.
fn resolve(source: &str, base: Option<&Path>) -> Result<Resolved, String> {
    let source = source.trim();
    let Some(base) = base else {
        if is_shorthand(source) {
            return Ok(github(source));
        }
        return from_parse(source, Path::new(""));
    };
    match from_parse(source, base) {
        Err(_) if is_shorthand(source) => Ok(github(source)),
        other => other,
    }
}

fn from_parse(source: &str, base: &Path) -> Result<Resolved, String> {
    match extension_install::Source::parse(source, base)? {
        extension_install::Source::Git(url) => Ok(Resolved {
            kind: MarketplaceKind::Git,
            place: Place::Git(url),
        }),
        extension_install::Source::Path(path) => {
            let path = path
                .canonicalize()
                .map_err(|error| format!("{}: {error}", path.display()))?;
            Ok(Resolved {
                kind: MarketplaceKind::Folder,
                place: Place::Folder(path),
            })
        }
    }
}

fn hashes(market: &Marketplace) -> BTreeMap<String, String> {
    market
        .plugins
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                hex(&Sha256::digest(entry.canonical_json().as_bytes())),
            )
        })
        .collect()
}

/// A marketplace as clients see it. Reads no network; a folder is read where it lies.
fn info(name: &str, config: &MarketplaceSettings, store: &Store) -> MarketplaceInfo {
    let mut problems = Vec::new();
    let resolved = resolve(&config.source, None);
    let kind = match &resolved {
        Ok(resolved) => resolved.kind,
        Err(error) => {
            problems.push(error.clone());
            MarketplaceKind::Git
        }
    };
    let mut commit = None;
    let mut refreshed = None;
    let mut count = None;
    match resolved {
        Ok(Resolved {
            place: Place::Folder(folder),
            ..
        }) => match marketplace::read(&folder) {
            Ok(market) => {
                count = u32::try_from(market.plugins.len()).ok();
                problems.extend(market.problems);
            }
            Err(error) => problems.push(error),
        },
        _ => {
            if let Some(record) = store.get(name) {
                commit.clone_from(&record.commit);
                refreshed = Some(record.read_unix_ms);
                count = u32::try_from(record.plugins.len()).ok();
                problems.extend(record.problems.iter().cloned());
            }
        }
    }
    MarketplaceInfo {
        name: name.to_string(),
        source: config.source.clone(),
        revision: config.revision.clone(),
        kind,
        built_in: name == OFFICIAL_MARKETPLACE
            && same_marketplace_source(&config.source, OFFICIAL_MARKETPLACE_SOURCE),
        enabled: config.enabled,
        commit,
        refreshed_unix_ms: refreshed,
        plugin_count: count,
        problems,
    }
}

/// `plugins.marketplace_list`.
pub(crate) fn list(state: &DaemonState) -> Result<PluginsMarketplaceListResult, String> {
    let settings = current_settings(state)?;
    let store = load_store(state);
    Ok(PluginsMarketplaceListResult {
        marketplaces: settings
            .plugins
            .marketplaces
            .iter()
            .map(|(name, config)| info(name, config, &store))
            .collect(),
    })
}

/// `plugins.marketplace_add`.
pub(crate) async fn add(
    state: &DaemonState,
    params: PluginsMarketplaceAddParams,
) -> Result<MarketplaceInfo, String> {
    let base = std::env::current_dir().ok();
    add_with_base(state, params, base.as_deref()).await
}

/// [`add`], with the folder that relative sources are looked for in.
async fn add_with_base(
    state: &DaemonState,
    params: PluginsMarketplaceAddParams,
    base: Option<&Path>,
) -> Result<MarketplaceInfo, String> {
    let _operation = OPERATIONS.lock().await;
    let settings = current_settings(state)?;
    let source = params.source.trim().to_string();
    if source.is_empty() {
        return Err("source is empty".into());
    }
    let resolved = resolve(&source, base)?;
    let revision = params
        .revision
        .map(|revision| revision.trim().to_string())
        .filter(|revision| !revision.is_empty());
    let staging = Staging::new(state.paths().data_dir())?;
    let (root, commit, configured) = match &resolved.place {
        Place::Git(url) => {
            let dest = staging.path().join("clone");
            let commit = extension_install::git_fetch(url, revision.as_deref(), &dest).await?;
            (dest, Some(commit), source.clone())
        }
        Place::Folder(folder) => {
            if revision.is_some() {
                return Err("a revision applies to a git source, not to a folder".into());
            }
            (folder.clone(), None, display_path(folder))
        }
    };
    let market = marketplace::read(&root).map_err(|error| {
        format!("{source} is not a plugin marketplace (it needs .claude-plugin/marketplace.json): {error}")
    })?;
    let name = market.name.clone();
    if !imperium_config::valid_marketplace_name(&name) {
        return Err(format!(
            "the marketplace name {name:?} is not usable: use 1-64 letters, digits, '.', '_' or \
             '-', not starting or ending with a dot"
        ));
    }
    if name != OFFICIAL_MARKETPLACE && name.eq_ignore_ascii_case(OFFICIAL_MARKETPLACE) {
        return Err(format!(
            "the name {name} is reserved for the official marketplace, in any letter case"
        ));
    }
    if name == OFFICIAL_MARKETPLACE {
        if !same_marketplace_source(&configured, OFFICIAL_MARKETPLACE_SOURCE) {
            return Err(format!(
                "the name {name} is reserved for {OFFICIAL_MARKETPLACE_SOURCE}; {source} cannot \
                 take it"
            ));
        }
        if settings
            .plugins
            .marketplaces
            .get(&name)
            .is_some_and(|official| official.enabled)
        {
            return Err(format!("{name} is already added (it is built in)"));
        }
    } else if settings
        .plugins
        .marketplaces
        .keys()
        .any(|existing| existing.eq_ignore_ascii_case(&name))
    {
        return Err(format!(
            "a marketplace named {name} is already added; remove it first to add another"
        ));
    }
    if commit.is_some() {
        install_clone(state, &name, &root)?;
    }
    let written = if name == OFFICIAL_MARKETPLACE {
        imperium_config::set_marketplace_enabled(state.paths(), &name, true, std::env::vars())
    } else {
        imperium_config::save_marketplace(
            state.paths(),
            &name,
            &configured,
            revision.as_deref(),
            std::env::vars(),
        )
    };
    let written = match written {
        Ok(written) => written,
        Err(error) => {
            if commit.is_some() {
                remove_tree(&clone_dir(state, &name));
            }
            return Err(error.to_string());
        }
    };
    let record = Record {
        commit,
        read_unix_ms: unix_ms(),
        plugins: hashes(&market),
        problems: market.problems.clone(),
    };
    update_store(state, |store| {
        store.insert(name.clone(), record);
    })?;
    tracing::info!(marketplace = %name, "a client added a plugin marketplace");
    Ok(info(&name, &written, &load_store(state)))
}

/// Puts the clone made in `from` in place as `<data>/marketplaces/<name>`, replacing a copy that
/// is there so that a failure leaves the old one.
fn install_clone(state: &DaemonState, name: &str, from: &Path) -> Result<(), String> {
    let dir = clone_dir(state, name);
    let parent = clones_dir(state);
    std::fs::create_dir_all(&parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    if !dir.exists() {
        return move_tree(from, &dir);
    }
    let old = parent.join(format!(".old-{}-{}", clone_key(name), std::process::id()));
    remove_tree(&old);
    std::fs::rename(&dir, &old).map_err(|error| format!("{}: {error}", dir.display()))?;
    if let Err(error) = move_tree(from, &dir) {
        remove_tree(&dir);
        let _ = std::fs::rename(&old, &dir);
        return Err(error);
    }
    remove_tree(&old);
    Ok(())
}

/// `plugins.marketplace_refresh`.
pub(crate) async fn refresh(
    state: &DaemonState,
    params: PluginsMarketplaceRefreshParams,
) -> Result<PluginsMarketplaceRefreshResult, String> {
    let _operation = OPERATIONS.lock().await;
    let settings = current_settings(state)?;
    let names: Vec<String> = match params.name {
        Some(name) => {
            let config = settings
                .plugins
                .marketplaces
                .get(&name)
                .ok_or_else(|| format!("no marketplace named {name}"))?;
            if !config.enabled {
                return Err(format!("{name} is turned off"));
            }
            vec![name]
        }
        None => settings
            .plugins
            .marketplaces
            .iter()
            .filter(|(_, config)| config.enabled)
            .map(|(name, _)| name.clone())
            .collect(),
    };
    let mut refreshed = Vec::new();
    for name in names {
        let mut result = MarketplaceRefresh {
            name: name.clone(),
            commit_before: None,
            commit_after: None,
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            problems: Vec::new(),
            error: None,
        };
        if let Err(error) = read_again(
            state,
            &name,
            &settings.plugins.marketplaces[&name],
            &mut result,
        )
        .await
        {
            result.error = Some(error);
        }
        refreshed.push(result);
    }
    Ok(PluginsMarketplaceRefreshResult { refreshed })
}

/// Fetches (or re-reads) one marketplace, records it, and fills `result` with what changed. The
/// clone in place is replaced only when the new one reads as a marketplace.
async fn read_again(
    state: &DaemonState,
    name: &str,
    config: &MarketplaceSettings,
    result: &mut MarketplaceRefresh,
) -> Result<Marketplace, String> {
    let before = load_store(state).remove(name);
    result.commit_before = before.as_ref().and_then(|record| record.commit.clone());
    let (market, commit) = match resolve(&config.source, None)?.place {
        Place::Folder(folder) => (marketplace::read(&folder)?, None),
        Place::Git(url) => {
            let staging = Staging::new(state.paths().data_dir())?;
            let dest = staging.path().join("clone");
            let commit =
                extension_install::git_fetch(&url, config.revision.as_deref(), &dest).await?;
            let current = clone_dir(state, name);
            let have_clone = current.join(marketplace::MARKETPLACE_FILE).is_file();
            if have_clone && result.commit_before.as_deref() == Some(commit.as_str()) {
                // Nothing new: keep the copy that is there.
                (marketplace::read(&current)?, Some(commit))
            } else {
                let market = marketplace::read(&dest)?;
                install_clone(state, name, &dest)?;
                (market, Some(commit))
            }
        }
    };
    let now = hashes(&market);
    let old = before.map(|record| record.plugins).unwrap_or_default();
    result.added = now
        .keys()
        .filter(|key| !old.contains_key(*key))
        .cloned()
        .collect();
    result.removed = old
        .keys()
        .filter(|key| !now.contains_key(*key))
        .cloned()
        .collect();
    result.changed = now
        .iter()
        .filter(|(key, hash)| old.get(*key).is_some_and(|before| before != *hash))
        .map(|(key, _)| key.clone())
        .collect();
    result.commit_after.clone_from(&commit);
    result.problems.clone_from(&market.problems);
    let record = Record {
        commit,
        read_unix_ms: unix_ms(),
        plugins: now,
        problems: market.problems.clone(),
    };
    update_store(state, |store| {
        store.insert(name.to_string(), record);
    })?;
    Ok(market)
}

/// `plugins.marketplace_remove`.
pub(crate) async fn remove(
    state: &DaemonState,
    params: PluginsMarketplaceRemoveParams,
) -> Result<PluginsMarketplaceRemoveResult, String> {
    // Not while a clone or refresh of it is under way.
    let _operation = OPERATIONS.lock().await;
    let name = params.name;
    let settings = current_settings(state)?;
    if !settings.plugins.marketplaces.contains_key(&name) {
        return Err(format!("no marketplace named {name}"));
    }
    let disabled = name == OFFICIAL_MARKETPLACE;
    if disabled {
        imperium_config::set_marketplace_enabled(state.paths(), &name, false, std::env::vars())
            .map_err(|error| error.to_string())?;
    } else if !imperium_config::remove_marketplace(state.paths(), &name, std::env::vars())
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "{name} is not in your config file (an environment variable may define it)"
        ));
    }
    remove_tree(&clone_dir(state, &name));
    update_store(state, |store| {
        store.remove(&name);
    })?;
    Ok(PluginsMarketplaceRemoveResult { name, disabled })
}

/// `plugins.browse`.
pub(crate) async fn browse(
    state: &DaemonState,
    params: PluginsBrowseParams,
) -> Result<PluginsBrowseResult, String> {
    let _operation = OPERATIONS.lock().await;
    let settings = current_settings(state)?;
    let names: Vec<String> = match &params.marketplace {
        Some(name) => {
            let config = settings
                .plugins
                .marketplaces
                .get(name)
                .ok_or_else(|| format!("no marketplace named {name}"))?;
            if !config.enabled {
                return Err(format!("{name} is turned off"));
            }
            vec![name.clone()]
        }
        None => settings
            .plugins
            .marketplaces
            .iter()
            .filter(|(_, config)| config.enabled)
            .map(|(name, _)| name.clone())
            .collect(),
    };
    let words: Vec<String> = params
        .query
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let mut plugins = Vec::new();
    let mut problems = Vec::new();
    for name in names {
        match opened(state, &name, &settings.plugins.marketplaces[&name]).await {
            Ok((root, market)) => {
                problems.extend(
                    market
                        .problems
                        .iter()
                        .map(|problem| format!("{name}: {problem}")),
                );
                for entry in &market.plugins {
                    let listing = listing(&name, &root, entry);
                    if matches(&listing, &words) {
                        plugins.push(listing);
                    }
                }
            }
            Err(error) if params.marketplace.is_some() => return Err(error),
            Err(error) => problems.push(format!("{name}: {error}")),
        }
    }
    plugins.sort_by(|a, b| (&a.marketplace, &a.name).cmp(&(&b.marketplace, &b.name)));
    Ok(PluginsBrowseResult { plugins, problems })
}

/// Held while a plugin is copied out of a marketplace's own repository, so a refresh cannot
/// replace the clone under the copy.
pub(crate) async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
    OPERATIONS.lock().await
}

/// One plugin of a marketplace, with where its marketplace stands.
pub(crate) struct Located {
    /// The marketplace's name as configured.
    pub(crate) marketplace: String,
    /// Its source as configured.
    pub(crate) source: String,
    /// The folder its plugins are read from: the cached clone, or the folder itself.
    pub(crate) root: PathBuf,
    /// The commit of the clone; none for a folder.
    pub(crate) commit: Option<String>,
    pub(crate) entry: Entry,
}

/// The entry `plugin` of the marketplace `marketplace` (matched ignoring letter case). The
/// caller holds [`lock`].
pub(crate) async fn locate(
    state: &DaemonState,
    marketplace: &str,
    plugin: &str,
) -> Result<Located, String> {
    let settings = current_settings(state)?;
    let (name, config) = settings
        .plugins
        .marketplaces
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(marketplace))
        .ok_or_else(|| format!("no marketplace named {marketplace}"))?;
    if !config.enabled {
        return Err(format!("{name} is turned off"));
    }
    let (root, market) = opened(state, name, config).await?;
    let entry = market
        .plugin(plugin)
        .cloned()
        .ok_or_else(|| format!("{name} lists no plugin named {plugin}"))?;
    Ok(Located {
        marketplace: name.clone(),
        source: config.source.clone(),
        commit: load_store(state)
            .remove(name.as_str())
            .and_then(|record| record.commit),
        root,
        entry,
    })
}

/// The marketplace's folder and what it lists: the cached clone, made now when there is none.
async fn opened(
    state: &DaemonState,
    name: &str,
    config: &MarketplaceSettings,
) -> Result<(PathBuf, Marketplace), String> {
    if let Place::Folder(folder) = resolve(&config.source, None)?.place {
        let market = marketplace::read(&folder)?;
        return Ok((folder, market));
    }
    let dir = clone_dir(state, name);
    if !dir.join(marketplace::MARKETPLACE_FILE).is_file() {
        let mut ignored = MarketplaceRefresh {
            name: name.to_string(),
            commit_before: None,
            commit_after: None,
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            problems: Vec::new(),
            error: None,
        };
        read_again(state, name, config, &mut ignored).await?;
    }
    let market = marketplace::read(&dir)?;
    Ok((dir, market))
}

fn matches(listing: &PluginListing, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let text = [
        listing.name.as_str(),
        listing.description.as_str(),
        listing.category.as_deref().unwrap_or_default(),
    ]
    .into_iter()
    .chain(listing.keywords.iter().map(String::as_str))
    .chain(listing.tags.iter().map(String::as_str))
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase();
    words.iter().all(|word| text.contains(word.as_str()))
}

fn listing(marketplace: &str, root: &Path, entry: &Entry) -> PluginListing {
    let (source_kind, source, components) = match &entry.source {
        PluginSource::Path(path) => (
            "path",
            if path.is_empty() {
                ".".to_string()
            } else {
                path.clone()
            },
            components(root, path, entry),
        ),
        PluginSource::Github { repo, .. } => ("github", repo.clone(), None),
        PluginSource::Url { url, .. } => ("url", url.clone(), None),
        PluginSource::Unsupported(kind) => ("unsupported", kind.clone(), None),
    };
    PluginListing {
        id: format!("{}@{marketplace}", entry.name),
        name: entry.name.clone(),
        marketplace: marketplace.to_string(),
        description: entry.fields.description.clone(),
        version: entry.fields.version.clone(),
        author: entry
            .fields
            .author
            .as_ref()
            .map(|author| author.name.clone()),
        homepage: entry.fields.homepage.clone(),
        license: entry.fields.license.clone(),
        category: entry.category.clone(),
        keywords: entry.fields.keywords.clone(),
        tags: entry.tags.clone(),
        source_kind: source_kind.to_string(),
        source,
        components,
    }
}

/// What the plugin at `rel` inside the marketplace's own copy holds, read from folder and file
/// names and two small JSON files. `None` when the folder is not there or is reached through a
/// link; nothing in it runs.
fn components(root: &Path, rel: &str, entry: &Entry) -> Option<PluginComponents> {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        inside(root, rel).ok()?
    };
    if !dir.is_dir() {
        return None;
    }
    let folder = |name: &str| inside(&dir, name).ok().filter(|path| path.is_dir());
    let json = |name: &str| {
        inside(&dir, name)
            .ok()
            .filter(|path| path.is_file())
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    };
    let mut skills = folder("skills")
        .map(|skills| skill_names(&skills))
        .unwrap_or_default();
    skills.sort();
    let mut hooks = json("hooks/hooks.json").map_or(0, |value| count_hooks(&value));
    let mut servers = json(".mcp.json")
        .map(|value| server_names(&value))
        .unwrap_or_default();
    for declared in &entry.fields.components.hooks {
        if let Declared::Inline(value) = declared {
            hooks += count_hooks(value);
        }
    }
    for declared in &entry.fields.components.mcp_servers {
        if let Declared::Inline(value) = declared {
            servers.extend(server_names(value));
        }
    }
    servers.sort();
    servers.dedup();
    Some(PluginComponents {
        skills,
        commands: folder("commands")
            .map(|path| markdown_names(&path))
            .unwrap_or_default(),
        agents: folder("agents")
            .map(|path| markdown_names(&path))
            .unwrap_or_default(),
        hooks,
        mcp_servers: servers,
    })
}

pub(crate) fn skill_names(folder: &Path) -> Vec<String> {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.path().join("SKILL.md").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

pub(crate) fn markdown_names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().is_some_and(|ext| ext == "md"))
                .then(|| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                })
                .flatten()
        })
        .collect();
    names.sort();
    names
}

/// Hook commands in a `hooks.json` (with or without its outer `hooks` key).
fn count_hooks(value: &Value) -> u32 {
    let map = value
        .get("hooks")
        .and_then(Value::as_object)
        .or_else(|| value.as_object());
    let total: usize = map
        .into_iter()
        .flat_map(|map| map.values())
        .filter_map(Value::as_array)
        .flatten()
        .map(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum();
    u32::try_from(total).unwrap_or(u32::MAX)
}

/// Server names in a `.mcp.json` (with or without its outer `mcpServers` key).
fn server_names(value: &Value) -> Vec<String> {
    value
        .get("mcpServers")
        .and_then(Value::as_object)
        .or_else(|| value.as_object())
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::Arc;

    use imperium_config::{Config, Layer, Paths};

    use super::*;

    fn state(home: &Path) -> Arc<DaemonState> {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        DaemonState::new(Paths::under(home), settings, None)
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
            .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    /// A marketplace named `name` listing `plugins` (name, source JSON, description), with a
    /// plugin folder `p1` that holds a hook which would write `RAN` if anything ran it.
    fn write_market(dir: &Path, name: &str, plugins: &[(&str, &str, &str)]) {
        let entries: Vec<String> = plugins
            .iter()
            .map(|(plugin, source, description)| {
                format!(
                    r#"{{"name": "{plugin}", "source": {source}, "description": "{description}"}}"#
                )
            })
            .collect();
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/marketplace.json"),
            format!(
                r#"{{"name": "{name}", "owner": {{"name": "Owner"}}, "plugins": [{}]}}"#,
                entries.join(",")
            ),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("p1/hooks")).unwrap();
        std::fs::create_dir_all(dir.join("p1/commands")).unwrap();
        std::fs::create_dir_all(dir.join("p1/skills/review")).unwrap();
        std::fs::write(dir.join("p1/commands/fix.md"), "Fix it").unwrap();
        std::fs::write(
            dir.join("p1/skills/review/SKILL.md"),
            "---\nname: review\n---\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("p1/hooks/hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "echo RAN > ran.marker"}]}]}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("p1/.mcp.json"),
            r#"{"mcpServers": {"srv": {"command": "touch", "args": ["server.marker"]}}}"#,
        )
        .unwrap();
    }

    fn commit_repo(dir: &Path) {
        if !dir.join(".git").exists() {
            git(dir, &["init", "-q", "-b", "main"]);
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "update"]);
    }

    fn file_url(dir: &Path) -> String {
        let text = dir.display().to_string().replace('\\', "/");
        if text.starts_with('/') {
            format!("file://{text}")
        } else {
            format!("file:///{text}")
        }
    }

    /// No hook ran and no server started anywhere under `dir`.
    fn assert_nothing_ran(dir: &Path) {
        for entry in walk(dir) {
            let name = entry.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name != "ran.marker" && name != "server.marker",
                "{} was written",
                entry.display()
            );
        }
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    fn add_params(source: &str) -> PluginsMarketplaceAddParams {
        PluginsMarketplaceAddParams {
            source: source.to_string(),
            revision: None,
        }
    }

    #[test]
    fn the_built_in_marketplace_is_listed_without_a_fetch() {
        let home = tempfile::tempdir().unwrap();
        let state = state(home.path());
        let listed = list(&state).unwrap().marketplaces;
        assert_eq!(listed.len(), 1);
        let official = &listed[0];
        assert_eq!(official.name, OFFICIAL_MARKETPLACE);
        assert_eq!(official.source, OFFICIAL_MARKETPLACE_SOURCE);
        assert!(official.built_in && official.enabled);
        assert_eq!(official.kind, MarketplaceKind::Github);
        assert_eq!(official.commit, None);
        assert_eq!(official.plugin_count, None);
        assert!(official.problems.is_empty());
        // Nothing was cloned or written: no data folder content at all.
        assert!(!clones_dir(&state).exists());
        assert!(!store_path(&state).exists());
    }

    #[test]
    fn sources_resolve_to_github_git_or_a_folder() {
        let base = tempfile::tempdir().unwrap();
        let github = resolve("acme/plugins", Some(base.path())).unwrap();
        assert_eq!(github.kind, MarketplaceKind::Github);
        assert_eq!(
            github.place,
            Place::Git("https://github.com/acme/plugins.git".into())
        );
        let url = resolve("https://example.test/m.git", Some(base.path())).unwrap();
        assert_eq!(url.kind, MarketplaceKind::Git);
        // A folder that exists wins over owner/repo, and the result says so.
        std::fs::create_dir_all(base.path().join("acme/plugins")).unwrap();
        let folder = resolve("acme/plugins", Some(base.path())).unwrap();
        assert_eq!(folder.kind, MarketplaceKind::Folder);
        // A configured source is never re-read as a folder.
        assert_eq!(
            resolve("acme/plugins", None).unwrap().kind,
            MarketplaceKind::Github
        );
        assert!(resolve("not a source", Some(base.path())).is_err());
        assert!(resolve("ext::sh -c evil", Some(base.path())).is_err());
    }

    #[tokio::test]
    async fn a_folder_marketplace_is_added_listed_browsed_and_removed() {
        let home = tempfile::tempdir().unwrap();
        let market = tempfile::tempdir().unwrap();
        write_market(
            market.path(),
            "local-market",
            &[
                ("fmt", r#""./p1""#, "Formats code"),
                (
                    "far",
                    r#"{"source": "github", "repo": "acme/far"}"#,
                    "A far plugin",
                ),
                (
                    "npm-one",
                    r#"{"source": "npm", "package": "x"}"#,
                    "From npm",
                ),
            ],
        );
        let state = state(home.path());
        let params = add_params(&market.path().display().to_string());
        let added = add_with_base(&state, params.clone(), Some(Path::new("")))
            .await
            .unwrap();
        assert_eq!(added.name, "local-market");
        assert_eq!(added.kind, MarketplaceKind::Folder);
        assert!(!added.built_in && added.enabled);
        assert_eq!(added.plugin_count, Some(3));
        // The same name again is refused.
        let again = add_with_base(&state, params, Some(Path::new(""))).await;
        assert!(again.unwrap_err().contains("already added"));

        let listed = list(&state).unwrap().marketplaces;
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .any(|m| m.name == "local-market" && m.plugin_count == Some(3))
        );

        let browsed = browse(
            &state,
            PluginsBrowseParams {
                marketplace: Some("local-market".into()),
                query: None,
            },
        )
        .await
        .unwrap();
        let by_name = |name: &str| browsed.plugins.iter().find(|p| p.name == name).unwrap();
        assert_eq!(browsed.plugins.len(), 3);
        let fmt = by_name("fmt");
        assert_eq!(fmt.id, "fmt@local-market");
        assert_eq!(fmt.source_kind, "path");
        let components = fmt.components.as_ref().unwrap();
        assert_eq!(components.skills, ["review"]);
        assert_eq!(components.commands, ["fix"]);
        assert_eq!(components.hooks, 1);
        assert_eq!(components.mcp_servers, ["srv"]);
        assert_eq!(by_name("far").source_kind, "github");
        assert!(by_name("far").components.is_none());
        assert_eq!(by_name("npm-one").source_kind, "unsupported");
        assert!(by_name("npm-one").components.is_none());

        let found = browse(
            &state,
            PluginsBrowseParams {
                marketplace: Some("local-market".into()),
                query: Some("FORMATS code".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(found.plugins.len(), 1);
        assert_eq!(found.plugins[0].name, "fmt");

        // Nothing ran, in the marketplace or anywhere under the daemon's folders.
        assert_nothing_ran(market.path());
        assert_nothing_ran(home.path());

        let removed = remove(
            &state,
            PluginsMarketplaceRemoveParams {
                name: "local-market".into(),
            },
        )
        .await
        .unwrap();
        assert!(!removed.disabled);
        assert_eq!(list(&state).unwrap().marketplaces.len(), 1);
        assert!(
            remove(
                &state,
                PluginsMarketplaceRemoveParams {
                    name: "local-market".into()
                }
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_git_marketplace_is_cloned_refreshed_and_removed() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_market(
            repo.path(),
            "git-market",
            &[("fmt", r#""./p1""#, "Formats")],
        );
        commit_repo(repo.path());
        let state = state(home.path());
        let url = file_url(repo.path());
        let added = add_with_base(&state, add_params(&url), Some(Path::new("")))
            .await
            .unwrap();
        assert_eq!(added.name, "git-market");
        assert_eq!(added.kind, MarketplaceKind::Git);
        let first = added.commit.clone().unwrap();
        assert_eq!(added.plugin_count, Some(1));
        assert!(
            clone_dir(&state, "git-market")
                .join(".claude-plugin/marketplace.json")
                .is_file()
        );

        // No new commit: nothing is reported as changed.
        let same = refresh(
            &state,
            PluginsMarketplaceRefreshParams {
                name: Some("git-market".into()),
            },
        )
        .await
        .unwrap();
        let same = &same.refreshed[0];
        assert_eq!(same.error, None);
        assert_eq!(same.commit_before.as_deref(), Some(first.as_str()));
        assert_eq!(same.commit_after.as_deref(), Some(first.as_str()));
        assert!(same.added.is_empty() && same.removed.is_empty() && same.changed.is_empty());

        // A new commit that changes one entry, adds one and keeps none out.
        write_market(
            repo.path(),
            "git-market",
            &[
                ("fmt", r#""./p1""#, "Formats better"),
                ("new", r#""./p1""#, "New one"),
            ],
        );
        commit_repo(repo.path());
        let result = refresh(
            &state,
            PluginsMarketplaceRefreshParams {
                name: Some("git-market".into()),
            },
        )
        .await
        .unwrap();
        let changed = &result.refreshed[0];
        assert_eq!(changed.error, None);
        assert_ne!(changed.commit_after.as_deref(), Some(first.as_str()));
        assert_eq!(changed.changed, ["fmt"]);
        assert_eq!(changed.added, ["new"]);
        assert!(changed.removed.is_empty());
        let listed = list(&state).unwrap().marketplaces;
        let info = listed.iter().find(|m| m.name == "git-market").unwrap();
        assert_eq!(info.plugin_count, Some(2));
        assert_eq!(info.commit, changed.commit_after);

        // The clone's own files were never run.
        assert_nothing_ran(&clones_dir(&state));
        assert_nothing_ran(repo.path());

        remove(
            &state,
            PluginsMarketplaceRemoveParams {
                name: "git-market".into(),
            },
        )
        .await
        .unwrap();
        assert!(!clone_dir(&state, "git-market").exists());
        assert!(!load_store(&state).contains_key("git-market"));
    }

    #[tokio::test]
    async fn a_malformed_marketplace_lists_its_problems_and_a_bad_source_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let market = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(market.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            market.path().join(".claude-plugin/marketplace.json"),
            r#"{"name": "messy", "plugins": [{"name": "ok", "source": "./ok"},
                {"name": "../evil", "source": "./x"}, {"name": "far", "source": "../outside"}]}"#,
        )
        .unwrap();
        let state = state(home.path());
        let added = add_with_base(
            &state,
            add_params(&market.path().display().to_string()),
            Some(Path::new("")),
        )
        .await
        .unwrap();
        assert_eq!(added.plugin_count, Some(1));
        assert_eq!(added.problems.len(), 2, "{:?}", added.problems);

        let empty = tempfile::tempdir().unwrap();
        let error = add_with_base(
            &state,
            add_params(&empty.path().display().to_string()),
            Some(Path::new("")),
        )
        .await
        .unwrap_err();
        assert!(error.contains("marketplace.json"), "{error}");
        let revision = add_with_base(
            &state,
            PluginsMarketplaceAddParams {
                source: market.path().display().to_string(),
                revision: Some("main".into()),
            },
            Some(Path::new("")),
        )
        .await
        .unwrap_err();
        assert!(revision.contains("git source"), "{revision}");
    }

    #[tokio::test]
    async fn the_official_name_is_reserved_and_removing_it_turns_it_off() {
        let home = tempfile::tempdir().unwrap();
        let impostor = tempfile::tempdir().unwrap();
        write_market(
            impostor.path(),
            OFFICIAL_MARKETPLACE,
            &[("x", r#""./p1""#, "x")],
        );
        let state = state(home.path());
        let params = add_params(&impostor.path().display().to_string());
        let on = add_with_base(&state, params.clone(), Some(Path::new("")))
            .await
            .unwrap_err();
        assert!(on.contains("reserved"), "{on}");

        let removed = remove(
            &state,
            PluginsMarketplaceRemoveParams {
                name: OFFICIAL_MARKETPLACE.into(),
            },
        )
        .await
        .unwrap();
        assert!(removed.disabled);
        let text = std::fs::read_to_string(state.paths().user_config_file()).unwrap();
        assert!(text.contains("enabled = false"), "{text}");
        let listed = list(&state).unwrap().marketplaces;
        assert!(!listed[0].enabled && listed[0].built_in);

        // Still reserved while off.
        let off = add_with_base(&state, params, Some(Path::new("")))
            .await
            .unwrap_err();
        assert!(off.contains("reserved"), "{off}");
        // Turned off, it is skipped by refresh and refused by name.
        assert!(
            refresh(
                &state,
                PluginsMarketplaceRefreshParams {
                    name: Some(OFFICIAL_MARKETPLACE.into())
                }
            )
            .await
            .unwrap_err()
            .contains("turned off")
        );
        assert!(
            refresh(&state, PluginsMarketplaceRefreshParams::default())
                .await
                .unwrap()
                .refreshed
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_pinned_source_under_another_name_is_not_built_in() {
        let home = tempfile::tempdir().unwrap();
        let state = state(home.path());
        imperium_config::save_marketplace(
            state.paths(),
            "mirror",
            "https://github.com/Anthropics/claude-plugins-official.git",
            None,
            [],
        )
        .unwrap();
        let listed = list(&state).unwrap().marketplaces;
        let mirror = listed.iter().find(|m| m.name == "mirror").unwrap();
        assert!(!mirror.built_in);
        assert!(
            listed
                .iter()
                .find(|m| m.name == OFFICIAL_MARKETPLACE)
                .unwrap()
                .built_in
        );
    }

    #[test]
    fn counting_hooks_and_servers_reads_either_shape() {
        let hooks: Value = serde_json::from_str(
            r#"{"hooks": {"PreToolUse": [{"hooks": [{}, {}]}], "Stop": [{"hooks": [{}]}]}}"#,
        )
        .unwrap();
        assert_eq!(count_hooks(&hooks), 3);
        let bare: Value = serde_json::from_str(r#"{"Stop": [{"hooks": [{}]}]}"#).unwrap();
        assert_eq!(count_hooks(&bare), 1);
        let servers: Value = serde_json::from_str(r#"{"mcpServers": {"b": {}, "a": {}}}"#).unwrap();
        assert_eq!(server_names(&servers), ["a", "b"]);
    }

    #[tokio::test]
    async fn a_name_that_differs_only_by_case_or_a_dot_cannot_take_another_ones_folder() {
        let home = tempfile::tempdir().unwrap();
        let state = state(home.path());
        // The official marketplace's clone, as a first browse would have made it.
        let official = clone_dir(&state, OFFICIAL_MARKETPLACE);
        std::fs::create_dir_all(&official).unwrap();
        std::fs::write(official.join("marker"), "official").unwrap();
        for name in [
            "Claude-Plugins-Official",
            "CLAUDE-PLUGINS-OFFICIAL",
            "claude-plugins-official.",
        ] {
            let impostor = tempfile::tempdir().unwrap();
            write_market(impostor.path(), name, &[("x", r#""./p1""#, "x")]);
            let error = add_with_base(
                &state,
                add_params(&impostor.path().display().to_string()),
                Some(Path::new("")),
            )
            .await
            .unwrap_err();
            assert!(
                error.contains("reserved")
                    || error.contains("not usable")
                    || error.contains("not valid"),
                "{name}: {error}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(official.join("marker")).unwrap(),
            "official"
        );
        assert!(!state.paths().user_config_file().exists());

        // acme and ACME are one name.
        let lower = tempfile::tempdir().unwrap();
        write_market(lower.path(), "acme", &[("x", r#""./p1""#, "x")]);
        add_with_base(
            &state,
            add_params(&lower.path().display().to_string()),
            Some(Path::new("")),
        )
        .await
        .unwrap();
        let upper = tempfile::tempdir().unwrap();
        write_market(upper.path(), "ACME", &[("y", r#""./p1""#, "y")]);
        let error = add_with_base(
            &state,
            add_params(&upper.path().display().to_string()),
            Some(Path::new("")),
        )
        .await
        .unwrap_err();
        assert!(error.contains("already added"), "{error}");
        // A trailing dot is not a usable name.
        let dotted = tempfile::tempdir().unwrap();
        write_market(dotted.path(), "acme2.", &[("y", r#""./p1""#, "y")]);
        assert!(
            add_with_base(
                &state,
                add_params(&dotted.path().display().to_string()),
                Some(Path::new("")),
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn a_clone_folder_never_leaves_the_clones_folder_and_ignores_case() {
        let home = tempfile::tempdir().unwrap();
        let state = state(home.path());
        for name in ["../../..", "a/b", "..", "x.", "", "C:\\x"] {
            let dir = clone_dir(&state, name);
            assert_eq!(dir.parent(), Some(clones_dir(&state).as_path()), "{name:?}");
            assert!(
                dir.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("invalid-"),
                "{name:?}"
            );
        }
        assert_eq!(clone_dir(&state, "Acme"), clone_dir(&state, "acme"));
    }

    #[tokio::test]
    async fn the_first_browse_of_a_git_marketplace_clones_it_and_leaves_no_staging() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_market(repo.path(), "lazy", &[("fmt", r#""./p1""#, "Formats")]);
        commit_repo(repo.path());
        let state = state(home.path());
        add_with_base(
            &state,
            add_params(&file_url(repo.path())),
            Some(Path::new("")),
        )
        .await
        .unwrap();
        // As for the built-in marketplace before its first browse: no clone yet.
        remove_tree(&clone_dir(&state, "lazy"));
        assert!(!clone_dir(&state, "lazy").exists());
        let browsed = browse(
            &state,
            PluginsBrowseParams {
                marketplace: Some("lazy".into()),
                query: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(browsed.plugins.len(), 1);
        assert_eq!(browsed.plugins[0].components.as_ref().unwrap().hooks, 1);
        assert!(
            clone_dir(&state, "lazy")
                .join(".claude-plugin/marketplace.json")
                .is_file()
        );
        assert_nothing_ran(&clones_dir(&state));
        assert_nothing_ran(repo.path());
        assert_nothing_ran(home.path());
        assert!(!state.paths().data_dir().join(".staging").exists());
        assert!(!state.paths().config_dir().join(".staging").exists());
    }
}
