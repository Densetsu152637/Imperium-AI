//! Plugins in the Claude Code layout: a folder with `.claude-plugin/plugin.json` contributing
//! `skills/`, `commands/`, `agents/` (named subagents, see [`crate::agents`]),
//! `hooks/hooks.json`, and `.mcp.json`. `${CLAUDE_PLUGIN_ROOT}` in hook commands and server
//! definitions is the plugin folder.
//!
//! A manifest may also name more components. `commands` and `agents` take a folder path or a
//! list of them; `hooks` and `mcpServers` take a file path, a list of paths, or an inline object
//! (the contents of `hooks.json` and `.mcp.json`, with or without their outer `hooks` or
//! `mcpServers` key). **Declared components add to the default folders and files**, they never
//! replace them, and a name that two sources define is taken from the first (default first).
//! Every declared path must stay inside the plugin folder: an absolute path, a `..`, or a path
//! that passes through a symbolic link or junction is refused and reported. A declared `commands`
//! or `agents` path must be a folder; a file there is reported and skipped.
//!
//! A folder with `.claude-plugin/marketplace-entry.json` (the marketplace entry an install
//! records, see the `marketplace` module) and no `plugin.json` is a plugin whose manifest is that
//! entry. With both, `plugin.json` wins for name and metadata (the entry fills what it leaves
//! out) and the entry's components add to the manifest's.
//!
//! Reading is limited to the given folders and the files inside each plugin; nothing is run.
//!
//! `${VAR}` in `.mcp.json` falls back to the daemon's environment, except for a plugin in the
//! `downloaded` set the caller passes ([`discover`]): downloaded content must not read the
//! daemon's secrets into a server's command, arguments, headers, or URL, so there only
//! `CLAUDE_PLUGIN_ROOT` expands and any other variable is empty (or its `:-default`). Hooks of a
//! downloaded plugin may block but never rewrite (`may_rewrite = false`). The daemon fills the
//! set with the folders [`agent_installed`] names until marketplace installs join it, and
//! `discover` itself always adds those agent-installed folders, so a caller that passes an empty
//! set cannot widen what such a plugin reads.
//!
//! [`runs`] describes what a plugin would run (each hook and each server) as a [`RunSummary`]
//! that can be shown, compared and digested. It describes the declared text: `${VAR}`
//! references stay as written and no daemon environment value is ever read into it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use imperium_api::encoding::hex;
use imperium_mcp::{ServerSpec, mcp_json};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::hooks::{Hook, parse_hooks_json};

/// The provenance file the daemon writes into each extension an agent installs.
pub const SOURCE_FILE: &str = ".imperium-source.json";

const MANIFEST_FILE: &str = ".claude-plugin/plugin.json";

/// Where a marketplace install records its entry inside the plugin folder.
pub const ENTRY_FILE: &str = ".claude-plugin/marketplace-entry.json";

#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    /// The manifest's description; empty when it has none.
    pub description: String,
    pub version: Option<String>,
    /// The plugin's own claims about itself: unverified.
    pub author: Option<Author>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    pub root: PathBuf,
    pub hooks: Vec<Hook>,
    pub servers: Vec<ServerSpec>,
    /// Folders the manifest declared besides `commands/` and `agents/`, inside the plugin.
    pub extra_commands: Vec<PathBuf>,
    pub extra_agents: Vec<PathBuf>,
    /// Whether the plugin was in the `downloaded` set when loaded.
    pub downloaded: bool,
    /// Whether `.claude-plugin/marketplace-entry.json` contributed.
    pub from_entry: bool,
    /// What each server would run, as written (see [`runs`]).
    declared: Vec<ServerRun>,
}

impl Plugin {
    pub fn origin(&self) -> String {
        format!("plugin:{}", self.name)
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    pub fn commands_dir(&self) -> PathBuf {
        self.root.join("commands")
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }
}

/// Who wrote a plugin or owns a marketplace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Author {
    pub name: String,
    pub email: Option<String>,
    pub url: Option<String>,
}

impl Author {
    /// A string is a name; an object has `name`, `email` and `url`. `None` for anything else or
    /// when there is no name.
    pub fn from_value(value: &Value) -> Option<Author> {
        let author = match value {
            Value::String(name) => Author {
                name: name.clone(),
                ..Author::default()
            },
            Value::Object(object) => {
                let text = |key: &str| {
                    object
                        .get(key)
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string)
                };
                Author {
                    name: text("name").unwrap_or_default(),
                    email: text("email"),
                    url: text("url"),
                }
            }
            _ => return None,
        };
        (!author.name.is_empty()).then_some(author)
    }
}

/// One declared `hooks` or `mcpServers` source.
#[derive(Debug, Clone, PartialEq)]
pub enum Declared {
    /// A path inside the plugin, as written.
    Path(String),
    /// The contents themselves.
    Inline(Value),
}

/// The components a manifest or marketplace entry names beyond the default folders.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Components {
    pub commands: Vec<String>,
    pub agents: Vec<String>,
    pub hooks: Vec<Declared>,
    pub mcp_servers: Vec<Declared>,
}

impl Components {
    fn extend(&mut self, other: Components) {
        self.commands.extend(other.commands);
        self.agents.extend(other.agents);
        self.hooks.extend(other.hooks);
        self.mcp_servers.extend(other.mcp_servers);
    }
}

/// What a `plugin.json` or a marketplace entry says about a plugin. Unknown fields are ignored.
/// A component field (`commands`, `agents`, `hooks`, `mcpServers`) of the wrong type is reported
/// and left out; a metadata field of the wrong type is left out without a report.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Fields {
    pub name: Option<String>,
    pub description: String,
    pub version: Option<String>,
    pub author: Option<Author>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Vec<String>,
    pub components: Components,
}

impl Fields {
    pub fn from_value(value: &Value, notes: &mut Vec<String>) -> Fields {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        let repository = match value.get("repository") {
            // npm style: {"type": "git", "url": "..."}.
            Some(Value::Object(object)) => object
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => text("repository"),
        };
        Fields {
            name: text("name"),
            description: text("description").unwrap_or_default(),
            version: text("version"),
            author: value.get("author").and_then(Author::from_value),
            homepage: text("homepage"),
            repository,
            license: text("license"),
            keywords: value
                .get("keywords")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            components: Components {
                commands: paths(value, "commands", notes),
                agents: paths(value, "agents", notes),
                hooks: declared(value, "hooks", notes),
                mcp_servers: declared(value, "mcpServers", notes),
            },
        }
    }
}

/// `key` as a path or a list of paths.
fn paths(value: &Value, key: &str, notes: &mut Vec<String>) -> Vec<String> {
    let mut found = Vec::new();
    for item in declared(value, key, notes) {
        match item {
            Declared::Path(path) => found.push(path),
            Declared::Inline(_) => notes.push(format!("{key}: expected a path or a list of paths")),
        }
    }
    found
}

/// `key` as a path, an inline object, or a list of either.
fn declared(value: &Value, key: &str, notes: &mut Vec<String>) -> Vec<Declared> {
    let mut found = Vec::new();
    let mut add = |item: &Value, notes: &mut Vec<String>| match item {
        Value::String(path) if !path.is_empty() => found.push(Declared::Path(path.clone())),
        Value::Object(_) => found.push(Declared::Inline(item.clone())),
        _ => notes.push(format!("{key}: expected a path or an object")),
    };
    match value.get(key) {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => items.iter().for_each(|item| add(item, notes)),
        Some(item) => add(item, notes),
    }
    found
}

/// Loads each plugin folder in `dirs` (folders that themselves hold plugins are searched one
/// level down). Returns the plugins and problems worth telling the user about. A plugin whose
/// folder is in `downloaded` reads no daemon environment and its hooks never rewrite.
pub fn discover(
    dirs: &[PathBuf],
    server_timeout: Duration,
    downloaded: &[PathBuf],
) -> (Vec<Plugin>, Vec<String>) {
    discover_gated(dirs, server_timeout, downloaded, None)
}

/// [`discover`] with the load-time gate: for `Some((gated, allowed))`, a plugin folder inside
/// `gated` (or `gated` itself) is read only if it is in `allowed` (matched as named or by
/// canonical path); any other is skipped without being read. Folders outside `gated` are
/// unaffected.
pub fn discover_gated(
    dirs: &[PathBuf],
    server_timeout: Duration,
    downloaded: &[PathBuf],
    gate: Option<(PathBuf, &[PathBuf])>,
) -> (Vec<Plugin>, Vec<String>) {
    let mut plugins = Vec::new();
    let mut problems = Vec::new();
    for root in plugin_folders(dirs) {
        if let Some((gated, allowed)) = &gate {
            // Both checks are needed. A folder that is a link or junction into `gated` resolves
            // outside it when it points elsewhere (`gated/x -> /outside/x`), so only the path as
            // listed shows it is in `gated`; and a `plugin_dirs` entry that reaches `gated`
            // through another path (a link to `gated`, `..`) is only seen by its canonical path.
            let listed_inside = root.starts_with(gated);
            let resolved_inside = matches!(
                (root.canonicalize(), gated.canonicalize()),
                (Ok(root), Ok(gated)) if root.starts_with(gated)
            );
            let inside = listed_inside || resolved_inside;
            if inside && !allowed.iter().any(|folder| same_folder(folder, &root)) {
                continue;
            }
        }
        // An agent-installed folder is downloaded whatever the caller passed.
        let is_downloaded =
            downloaded.iter().any(|folder| same_folder(folder, &root)) || agent_installed(&root);
        match load(&root, server_timeout, is_downloaded) {
            Ok((plugin, mut notes)) => {
                problems.append(&mut notes);
                if plugins.iter().any(|p: &Plugin| p.name == plugin.name) {
                    problems.push(format!(
                        "plugin {} at {} is ignored: the name is already used",
                        plugin.name,
                        root.display()
                    ));
                } else {
                    plugins.push(plugin);
                }
            }
            Err(error) => problems.push(format!("plugin at {}: {error}", root.display())),
        }
    }
    (plugins, problems)
}

/// The plugin folders [`discover`] would load from `dirs`, in order: each of `dirs` that is a
/// plugin, else its children that are.
pub fn plugin_folders(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for dir in dirs {
        if is_plugin(dir) {
            roots.push(dir.clone());
        } else if let Ok(entries) = std::fs::read_dir(dir) {
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| is_plugin(path))
                .collect();
            children.sort();
            roots.extend(children);
        }
    }
    roots
}

/// The plugin folders under `dirs` that an agent installed ([`agent_installed`]): what the
/// daemon passes as the `downloaded` set until marketplace installs join it.
pub fn agent_installed_folders(dirs: &[PathBuf]) -> Vec<PathBuf> {
    plugin_folders(dirs)
        .into_iter()
        .filter(|folder| agent_installed(folder))
        .collect()
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// Whether an agent installed the plugin in `root`: its provenance file records a session. A
/// provenance file that cannot be read or parsed counts as agent-installed, so a damaged record
/// never widens what the plugin may read. The caller adds such folders to the `downloaded` set.
pub fn agent_installed(root: &Path) -> bool {
    let path = root.join(SOURCE_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return path.exists();
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        // Anything but an object (`[]`, `null`, `123`) is a damaged record too.
        Ok(value) => value.as_object().is_none_or(|record| {
            record
                .get("session_id")
                .is_some_and(|session| !session.is_null())
        }),
        Err(_) => true,
    }
}

fn is_plugin(dir: &Path) -> bool {
    dir.join(MANIFEST_FILE).is_file() || dir.join(ENTRY_FILE).is_file()
}

/// A path declared in a manifest, resolved inside `root`: relative, no `..`, and no existing
/// component of it a symbolic link or junction. The path need not exist.
pub fn inside(root: &Path, declared: &str) -> Result<PathBuf, String> {
    if declared.starts_with(['/', '\\']) || declared.as_bytes().get(1) == Some(&b':') {
        return Err(format!("{declared}: a path must be relative to the plugin"));
    }
    let mut path = root.to_path_buf();
    let mut checking = true;
    for part in declared.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return Err(format!("{declared}: leaves the plugin folder")),
            // A colon in any part would make `push` restart from a drive on Windows.
            part if part.contains([':', '\0']) => {
                return Err(format!("{declared}: not a valid path"));
            }
            part => path.push(part),
        }
        if checking {
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(format!("{declared}: goes through a link"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => checking = false,
                Err(error) => return Err(format!("{declared}: {error}")),
            }
        }
    }
    if !path.starts_with(root) {
        return Err(format!("{declared}: leaves the plugin folder"));
    }
    Ok(path)
}

/// The folders declared for `commands` or `agents`: inside the plugin, existing, and folders.
fn declared_folders(root: &Path, declared: &[String], notes: &mut Vec<String>) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::new();
    for item in declared {
        match inside(root, item) {
            Ok(path) if path.is_dir() => {
                if !folders.contains(&path) {
                    folders.push(path);
                }
            }
            Ok(_) => notes.push(format!("{item}: is not a folder in the plugin, skipped")),
            Err(error) => notes.push(error),
        }
    }
    folders
}

/// The JSON each `hooks` or `mcpServers` source holds: the default file when it exists, then the
/// declared paths, then the inline objects. Declared paths that are refused or missing are
/// reported; a file that is not JSON is an error.
fn sources(
    root: &Path,
    default: &str,
    declared: &[Declared],
    notes: &mut Vec<String>,
) -> Result<Vec<Value>, String> {
    let mut files = vec![(root.join(default), false)];
    let mut inline = Vec::new();
    for item in declared {
        match item {
            Declared::Path(path) => match inside(root, path) {
                Ok(path) => files.push((path, true)),
                Err(error) => notes.push(error),
            },
            Declared::Inline(value) => inline.push(value.clone()),
        }
    }
    let mut values = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for (path, declared) in files {
        if seen.contains(&path) {
            continue;
        }
        let shown = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        match std::fs::read_to_string(&path) {
            Ok(text) => values.push(
                serde_json::from_str::<Value>(&text)
                    .map_err(|error| format!("{shown}: {error}"))?,
            ),
            Err(_) if !declared => {}
            Err(error) => notes.push(format!("{shown}: {error}")),
        }
        seen.push(path);
    }
    values.extend(inline);
    Ok(values)
}

/// Reads one manifest file into its fields; `None` when the file is not there.
fn read_fields(root: &Path, file: &str, notes: &mut Vec<String>) -> Result<Option<Fields>, String> {
    let path = root.join(file);
    if !path.is_file() {
        return Ok(None);
    }
    let shown = file.trim_start_matches(".claude-plugin/");
    let text = std::fs::read_to_string(&path).map_err(|error| format!("{shown}: {error}"))?;
    let value: Value = serde_json::from_str(&text).map_err(|error| format!("{shown}: {error}"))?;
    if !value.is_object() {
        return Err(format!("{shown}: expected a JSON object"));
    }
    Ok(Some(Fields::from_value(&value, notes)))
}

fn load(
    root: &Path,
    server_timeout: Duration,
    downloaded: bool,
) -> Result<(Plugin, Vec<String>), String> {
    let mut notes = Vec::new();
    let manifest = read_fields(root, MANIFEST_FILE, &mut notes)?;
    let entry = match read_fields(root, ENTRY_FILE, &mut notes) {
        Ok(entry) => entry,
        // A damaged entry only matters when it is the manifest.
        Err(error) if manifest.is_some() => {
            notes.push(error);
            None
        }
        Err(error) => return Err(error),
    };
    let from_entry = entry.is_some();
    let (mut fields, entry_fields) = match (manifest, entry) {
        (Some(manifest), entry) => (manifest, entry),
        (None, Some(entry)) => (entry, None),
        (None, None) => return Err("plugin.json: not found".into()),
    };
    if let Some(entry) = entry_fields {
        if entry.name.is_some() && entry.name != fields.name {
            notes.push("marketplace-entry.json names another plugin; plugin.json wins".into());
        }
        if fields.description.is_empty() {
            fields.description = entry.description;
        }
        fields.version = fields.version.or(entry.version);
        fields.author = fields.author.or(entry.author);
        fields.homepage = fields.homepage.or(entry.homepage);
        fields.repository = fields.repository.or(entry.repository);
        fields.license = fields.license.or(entry.license);
        if fields.keywords.is_empty() {
            fields.keywords = entry.keywords;
        }
        fields.components.extend(entry.components);
    }
    let name = fields.name.take().ok_or("plugin.json: it has no name")?;
    let origin = format!("plugin:{name}");
    let components = fields.components;

    let mut hooks = Vec::new();
    for value in sources(root, "hooks/hooks.json", &components.hooks, &mut notes)? {
        let wrapped = if matches!(value.get("hooks"), Some(Value::Object(_))) {
            value
        } else {
            Value::Object(Map::from_iter([("hooks".to_string(), value)]))
        };
        let (mut loaded, mut skipped) = parse_hooks_json(&wrapped.to_string(), root, &origin)?;
        hooks.append(&mut loaded);
        notes.append(&mut skipped);
    }
    // A downloaded plugin may block with its hooks but never rewrite a call or a prompt:
    // downloaded code does not get to change what the model asked for.
    for hook in &mut hooks {
        hook.may_rewrite = !downloaded;
    }

    let mut entries: Map<String, Value> = Map::new();
    for value in sources(root, ".mcp.json", &components.mcp_servers, &mut notes)? {
        let map = match value.get("mcpServers") {
            Some(Value::Object(map)) => map.clone(),
            _ => value.as_object().cloned().unwrap_or_default(),
        };
        for (server, definition) in map {
            if entries.contains_key(&server) {
                notes.push(format!(
                    "{origin}: server {server} is defined twice, the first is used"
                ));
            } else {
                entries.insert(server, definition);
            }
        }
    }
    let mut servers = Vec::new();
    let mut declared_servers = Vec::new();
    if !entries.is_empty() {
        let plugin_root = root.display().to_string();
        let text = Value::Object(Map::from_iter([(
            "mcpServers".to_string(),
            Value::Object(entries.clone()),
        )]))
        .to_string();
        let specs = mcp_json::parse(&text, root, server_timeout, |variable| {
            if variable == "CLAUDE_PLUGIN_ROOT" {
                Some(plugin_root.clone())
            } else if downloaded {
                None
            } else {
                std::env::var(variable).ok()
            }
        })
        .map_err(|error| format!(".mcp.json: {error}"))?;
        // Plugin servers are namespaced so they cannot collide with the user's own.
        servers = specs
            .into_iter()
            .map(|mut spec| {
                spec.name = format!("{name}_{}", spec.name);
                spec
            })
            .collect();
        declared_servers = entries
            .iter()
            .map(|(server, definition)| {
                declare(&format!("{name}_{server}"), definition, &plugin_root)
            })
            .collect();
    }
    Ok((
        Plugin {
            description: fields.description,
            version: fields.version,
            author: fields.author,
            homepage: fields.homepage,
            repository: fields.repository,
            license: fields.license,
            keywords: fields.keywords,
            extra_commands: declared_folders(root, &components.commands, &mut notes),
            extra_agents: declared_folders(root, &components.agents, &mut notes),
            downloaded,
            from_entry,
            declared: declared_servers,
            name,
            root: root.to_path_buf(),
            hooks,
            servers,
        },
        notes,
    ))
}

/// What a plugin would run, for showing before it is installed, comparing with an approved
/// version, and digesting.
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub hooks: Vec<HookRun>,
    pub servers: Vec<ServerRun>,
    /// The plugin folder the commands and URLs above are expanded with. Shown strings keep it;
    /// the canonical JSON and the digest put `${CLAUDE_PLUGIN_ROOT}` back, so the same plugin
    /// in another folder has the same digest.
    #[serde(skip)]
    root: String,
}

/// Equal when the canonical JSON is: the plugin folder does not count, as for the digest.
impl PartialEq for RunSummary {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_json() == other.canonical_json()
    }
}

impl Eq for RunSummary {}

impl RunSummary {
    /// Whether any hook or server starts a launcher that fetches code (`npx`, `uvx`, `@latest`).
    /// A missing flag is no guarantee: any script can download and run code on its own.
    pub fn has_launcher(&self) -> bool {
        self.hooks.iter().any(|hook| hook.launcher)
            || self.servers.iter().any(|server| server.launcher)
    }

    /// The summary as canonical JSON (sorted keys, no insignificant whitespace), with the plugin
    /// folder written as `${CLAUDE_PLUGIN_ROOT}` so it does not depend on where the plugin is.
    pub fn canonical_json(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if !self.root.is_empty() {
            unroot(&mut value, &self.root);
        }
        canonical_json(&value)
    }

    /// The SHA-256 of [`RunSummary::canonical_json`], as lowercase hex.
    pub fn digest(&self) -> String {
        hex(&Sha256::digest(self.canonical_json().as_bytes()))
    }
}

fn unroot(value: &mut Value, root: &str) {
    match value {
        Value::String(text) => *text = text.replace(root, "${CLAUDE_PLUGIN_ROOT}"),
        Value::Array(items) => items.iter_mut().for_each(|item| unroot(item, root)),
        Value::Object(map) => map.values_mut().for_each(|item| unroot(item, root)),
        _ => {}
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookRun {
    /// The Imperium event name (`tool.before`).
    pub event: String,
    pub matcher: String,
    /// The command line with `${CLAUDE_PLUGIN_ROOT}` expanded; empty for an HTTP hook.
    pub command: String,
    pub url: Option<String>,
    /// Starts a launcher that fetches more code.
    pub launcher: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerRun {
    /// The namespaced name (`<plugin>_<server>`).
    pub name: String,
    /// `stdio` or `http`.
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Names, never values, of the environment a stdio server is given.
    pub env_names: Vec<String>,
    pub url: Option<String>,
    /// Names, never values, of the headers an HTTP server sends.
    pub header_names: Vec<String>,
    /// `${VAR}` references in the definition, other than `CLAUDE_PLUGIN_ROOT`, sorted.
    pub variables: Vec<String>,
    /// Starts a launcher that fetches more code.
    pub launcher: bool,
}

/// Everything `plugin` would run: each hook (event, matcher, expanded command or URL) and each
/// server (command, arguments, URL, header names and `${VAR}` references), with the launcher
/// flag. Two loads of the same content give equal summaries and equal digests.
pub fn runs(plugin: &Plugin) -> RunSummary {
    RunSummary {
        hooks: plugin
            .hooks
            .iter()
            .map(|hook| {
                let tokens: Vec<&str> = hook.command.split_whitespace().collect();
                HookRun {
                    event: hook.event.as_str().to_string(),
                    matcher: hook.matcher.clone(),
                    command: hook.command.clone(),
                    url: hook.http.as_ref().map(|http| http.url.clone()),
                    launcher: launches_fetched_code(&tokens),
                }
            })
            .collect(),
        servers: plugin.declared.clone(),
        root: plugin.root.display().to_string(),
    }
}

/// A server definition as written, with only `${CLAUDE_PLUGIN_ROOT}` expanded.
fn declare(name: &str, definition: &Value, plugin_root: &str) -> ServerRun {
    let mut variables = BTreeSet::new();
    let mut text = |value: &Value| -> Option<String> {
        let value = value.as_str()?;
        variables.extend(references(value));
        Some(value.replace("${CLAUDE_PLUGIN_ROOT}", plugin_root))
    };
    let command = definition.get("command").and_then(&mut text);
    let url = definition.get("url").and_then(&mut text);
    let args: Vec<String> = definition
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(&mut text).collect())
        .unwrap_or_default();
    let mut names = |key: &str| -> Vec<String> {
        let mut names = Vec::new();
        if let Some(map) = definition.get(key).and_then(Value::as_object) {
            for (name, value) in map {
                text(value);
                names.push(name.clone());
            }
        }
        names.sort();
        names
    };
    let env_names = names("env");
    let header_names = names("headers");
    let kind = definition.get("type").and_then(Value::as_str);
    let http = url.is_some()
        && command.is_none()
        && matches!(kind, None | Some("http" | "streamable-http"));
    let mut tokens: Vec<&str> = command.iter().map(String::as_str).collect();
    tokens.extend(args.iter().map(String::as_str));
    let launcher = launches_fetched_code(&tokens);
    ServerRun {
        name: name.to_string(),
        transport: if http { "http" } else { "stdio" }.to_string(),
        command,
        args,
        env_names,
        url,
        header_names,
        variables: variables.into_iter().collect(),
        launcher,
    }
}

/// The `${NAME}` and `${NAME:-default}` names in `text`, except `CLAUDE_PLUGIN_ROOT`.
fn references(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else { break };
        let name = after[..end].split(":-").next().unwrap_or_default();
        if !name.is_empty() && name != "CLAUDE_PLUGIN_ROOT" {
            found.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    found
}

/// Whether the command line starts a launcher that downloads code and runs it: `npx`, `uvx`,
/// `bunx`, `pnpx`, `pipx run`, `yarn dlx`, `pnpm dlx`, `npm exec`, `docker run` (or `podman run`)
/// without an image pinned by digest, or anything asking for `@latest`.
fn launches_fetched_code(tokens: &[&str]) -> bool {
    let cleaned: Vec<&str> = tokens
        .iter()
        .map(|token| {
            token.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '(' | ')' | ';' | '{' | '}'))
        })
        .collect();
    cleaned.iter().enumerate().any(|(index, token)| {
        if token.contains("@latest") {
            return true;
        }
        let later = &cleaned[index + 1..];
        let next = later.first().copied();
        match program_name(token).as_str() {
            "npx" | "uvx" | "bunx" | "pnpx" => true,
            "pipx" => next == Some("run"),
            "yarn" | "pnpm" => next == Some("dlx"),
            "npm" => next == Some("exec"),
            "docker" | "podman" => {
                later.contains(&"run") && !later.iter().any(|token| token.contains("@sha256:"))
            }
            _ => false,
        }
    })
}

/// The program a command token names: its last path part, lowercased, without a Windows
/// launcher extension.
fn program_name(token: &str) -> String {
    let base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase();
    for extension in [".cmd", ".exe", ".bat", ".ps1"] {
        if let Some(name) = base.strip_suffix(extension) {
            return name.to_string();
        }
    }
    base
}

/// `value` as JSON with object keys sorted at every level and no insignificant whitespace.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&object[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_code_plugin_contributes_hooks_servers_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("reviewer");
        for sub in [
            ".claude-plugin",
            "hooks",
            "skills/review",
            "commands",
            "agents",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name": "reviewer", "version": "1.0.0", "description": "Reviews code"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("hooks/hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/fmt.sh"}]}]}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers": {"lint": {"command": "${CLAUDE_PLUGIN_ROOT}/lint-server"}}}"#,
        )
        .unwrap();
        let (plugins, problems) =
            discover(&[dir.path().to_path_buf()], Duration::from_secs(5), &[]);
        assert_eq!(plugins.len(), 1);
        let plugin = &plugins[0];
        assert_eq!(plugin.description, "Reviews code");
        assert_eq!(plugin.hooks.len(), 1);
        assert!(plugin.hooks[0].command.ends_with("fmt.sh"));
        assert_eq!(plugin.servers[0].name, "reviewer_lint");
        assert!(matches!(&plugin.servers[0].launch,
            imperium_mcp::Launch::Stdio { command, .. } if command.contains("reviewer")));
        assert!(problems.is_empty(), "{problems:?}");
        assert!(plugin.agents_dir().ends_with("agents"));
        assert!(plugin.skills_dir().ends_with("skills"));
    }

    /// A plugin whose `.mcp.json` puts `${var}` into a server's URL and the plugin folder into
    /// a header; `provenance` is written as `.imperium-source.json` when given.
    fn plugin_with_vars(dir: &Path, var: &str, provenance: Option<&str>) -> Vec<Plugin> {
        let root = dir.join("vars");
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name": "vars"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            format!(
                r#"{{"mcpServers": {{"api": {{"type": "http", "url": "https://example.test/${{{var}}}/${{UNSET_VAR_FOR_TEST:-fallback}}", "headers": {{"X-Root": "${{CLAUDE_PLUGIN_ROOT}}"}}}}}}}}"#
            ),
        )
        .unwrap();
        if let Some(provenance) = provenance {
            std::fs::write(root.join(SOURCE_FILE), provenance).unwrap();
        }
        // The daemon passes the folders an agent installed as the downloaded set.
        let dirs = [dir.to_path_buf()];
        discover(
            &dirs,
            Duration::from_secs(5),
            &agent_installed_folders(&dirs),
        )
        .0
    }

    fn url_and_root(plugin: &Plugin) -> (String, String) {
        match &plugin.servers[0].launch {
            imperium_mcp::Launch::Http { url, headers } => (
                url.clone(),
                headers.iter().map(|(_, value)| value.clone()).collect(),
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_user_plugin_reads_the_daemon_environment_in_its_servers() {
        // PATH is set in every environment the tests run in.
        let dir = tempfile::tempdir().unwrap();
        let plugins = plugin_with_vars(dir.path(), "PATH", None);
        let (url, root) = url_and_root(&plugins[0]);
        assert_eq!(
            url,
            format!(
                "https://example.test/{}/fallback",
                std::env::var("PATH").unwrap()
            )
        );
        assert!(root.ends_with("vars"), "{root}");
    }

    #[test]
    fn an_agent_installed_plugin_expands_only_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provenance = r#"{"kind": "git", "location": "https://example.test/x.git", "installed_at_unix_ms": 1, "session_id": "s1"}"#;
        let plugins = plugin_with_vars(dir.path(), "PATH", Some(provenance));
        let (url, root) = url_and_root(&plugins[0]);
        // The daemon's PATH is not read; the default and the plugin folder still expand.
        assert_eq!(url, "https://example.test//fallback");
        assert!(root.ends_with("vars"), "{root}");
    }

    #[test]
    fn a_damaged_provenance_file_still_restricts_expansion() {
        for damaged in ["not json", "[]", "null", "123"] {
            let dir = tempfile::tempdir().unwrap();
            let plugins = plugin_with_vars(dir.path(), "PATH", Some(damaged));
            assert_eq!(
                url_and_root(&plugins[0]).0,
                "https://example.test//fallback",
                "{damaged}"
            );
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn load_one(dir: &Path, downloaded: &[PathBuf]) -> (Plugin, Vec<String>) {
        let (mut plugins, problems) =
            discover(&[dir.to_path_buf()], Duration::from_secs(5), downloaded);
        assert_eq!(plugins.len(), 1, "{problems:?}");
        (plugins.remove(0), problems)
    }

    #[test]
    fn the_full_manifest_is_read() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join(".claude-plugin/plugin.json"),
            r#"{"name": "full", "version": "2.0.0", "description": "d",
                "author": {"name": "Ann", "email": "a@x.test", "url": "https://a.test"},
                "homepage": "https://h.test", "repository": {"type": "git", "url": "https://r.test"},
                "license": "MIT", "keywords": ["a", "b"], "unknown": {"x": 1}}"#,
        );
        let (plugin, problems) = load_one(dir.path(), &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugin.version.as_deref(), Some("2.0.0"));
        let author = plugin.author.unwrap();
        assert_eq!(author.name, "Ann");
        assert_eq!(author.email.as_deref(), Some("a@x.test"));
        assert_eq!(author.url.as_deref(), Some("https://a.test"));
        assert_eq!(plugin.homepage.as_deref(), Some("https://h.test"));
        assert_eq!(plugin.repository.as_deref(), Some("https://r.test"));
        assert_eq!(plugin.license.as_deref(), Some("MIT"));
        assert_eq!(plugin.keywords, ["a", "b"]);
        // An author written as a string is a name.
        write(
            &dir.path().join(".claude-plugin/plugin.json"),
            r#"{"name": "full", "author": "Bob"}"#,
        );
        assert_eq!(load_one(dir.path(), &[]).0.author.unwrap().name, "Bob");
    }

    #[test]
    fn custom_paths_and_inline_components_add_to_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "custom",
                "commands": ["./extra-commands", "./also"],
                "agents": "./extra-agents",
                "hooks": ["./config/more-hooks.json",
                    {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/inline.sh"}]}]}],
                "mcpServers": ["./config/servers.json",
                    {"inline": {"command": "${CLAUDE_PLUGIN_ROOT}/inline-server"}}]}"#,
        );
        for folder in ["extra-commands", "also", "extra-agents"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        write(
            &root.join("hooks/hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "default.sh"}]}]}}"#,
        );
        write(
            &root.join("config/more-hooks.json"),
            r#"{"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "file.sh"}]}]}}"#,
        );
        write(
            &root.join(".mcp.json"),
            r#"{"mcpServers": {"default": {"command": "d"}}}"#,
        );
        write(
            &root.join("config/servers.json"),
            r#"{"mcpServers": {"filed": {"command": "f"}}}"#,
        );
        let (plugin, problems) = load_one(root, &[]);
        assert!(problems.is_empty(), "{problems:?}");
        let commands: Vec<&str> = plugin
            .hooks
            .iter()
            .map(|hook| hook.command.as_str())
            .collect();
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert!(commands.contains(&"default.sh") && commands.contains(&"file.sh"));
        assert!(
            commands
                .iter()
                .any(|command| command.ends_with("inline.sh"))
        );
        let mut servers: Vec<&str> = plugin.servers.iter().map(|s| s.name.as_str()).collect();
        servers.sort();
        assert_eq!(servers, ["custom_default", "custom_filed", "custom_inline"]);
        assert_eq!(plugin.extra_commands.len(), 2);
        assert_eq!(plugin.extra_agents.len(), 1);
        assert!(plugin.extra_agents[0].ends_with("extra-agents"));
    }

    #[test]
    fn a_wrapped_inline_object_and_a_default_listed_again_load_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "wrapped", "hooks": "./hooks/hooks.json",
                "mcpServers": {"mcpServers": {"s": {"command": "run"}}}}"#,
        );
        write(
            &root.join("hooks/hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "once.sh"}]}]}}"#,
        );
        let (plugin, problems) = load_one(root, &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugin.hooks.len(), 1);
        assert_eq!(plugin.servers[0].name, "wrapped_s");
    }

    #[test]
    fn paths_outside_the_plugin_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "p", "commands": ["../outside", "/etc", "x/C:"], "agents": ["./a/../../x"],
                "hooks": ["../hooks.json", "a/C:hooks.json"], "mcpServers": ["C:\\servers.json"]}"#,
        );
        write(&dir.path().join("hooks.json"), r#"{"hooks": {}}"#);
        let (plugin, problems) = load_one(&root, &[]);
        assert!(plugin.hooks.is_empty() && plugin.servers.is_empty());
        assert!(plugin.extra_commands.is_empty() && plugin.extra_agents.is_empty());
        assert_eq!(problems.len(), 7, "{problems:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_through_a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        let outside = dir.path().join("outside");
        write(
            &outside.join("hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "evil.sh"}]}]}}"#,
        );
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.join("hooks.json"), root.join("file-link.json"))
            .unwrap();
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "p", "hooks": ["./linked/hooks.json", "./file-link.json"], "commands": ["./linked"]}"#,
        );
        let (plugin, problems) = load_one(&root, &[]);
        assert!(plugin.hooks.is_empty());
        assert!(plugin.extra_commands.is_empty());
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems.iter().all(|problem| problem.contains("link")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_folder_with_only_a_marketplace_entry_is_a_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("fmt");
        write(
            &root.join(".claude-plugin/marketplace-entry.json"),
            r#"{"name": "fmt", "source": "./fmt", "description": "Formats", "version": "1.1.0",
                "strict": false, "mcpServers": {"s": {"command": "${CLAUDE_PLUGIN_ROOT}/s"}}}"#,
        );
        let (plugin, problems) = load_one(dir.path(), &[]);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(plugin.name, "fmt");
        assert_eq!(plugin.description, "Formats");
        assert_eq!(plugin.version.as_deref(), Some("1.1.0"));
        assert!(plugin.from_entry);
        assert_eq!(plugin.servers[0].name, "fmt_s");
    }

    #[test]
    fn with_both_files_the_manifest_wins_and_the_entry_components_add() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("both");
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "both", "description": "from manifest",
                "mcpServers": {"own": {"command": "own"}}}"#,
        );
        write(
            &root.join(".claude-plugin/marketplace-entry.json"),
            r#"{"name": "other", "source": "./both", "description": "from entry", "license": "MIT",
                "mcpServers": {"added": {"command": "added"}}}"#,
        );
        let (plugin, problems) = load_one(dir.path(), &[]);
        assert_eq!(plugin.name, "both");
        assert_eq!(plugin.description, "from manifest");
        assert_eq!(plugin.license.as_deref(), Some("MIT"));
        let mut servers: Vec<&str> = plugin.servers.iter().map(|s| s.name.as_str()).collect();
        servers.sort();
        assert_eq!(servers, ["both_added", "both_own"]);
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    fn http_url(plugin: &Plugin) -> String {
        match &plugin.servers[0].launch {
            imperium_mcp::Launch::Http { url, .. } => url.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_downloaded_plugin_never_reads_the_daemon_environment_or_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("dl");
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "dl"}"#,
        );
        write(
            &root.join("hooks/hooks.json"),
            r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "check.sh"}]}]}}"#,
        );
        write(
            &root.join(".mcp.json"),
            r#"{"mcpServers": {"api": {"type": "http", "url": "https://example.test/${PATH}/x"}}}"#,
        );
        let (own, _) = load_one(dir.path(), &[]);
        assert!(own.hooks[0].may_rewrite && !own.downloaded);
        assert!(http_url(&own).contains(&std::env::var("PATH").unwrap()));
        let (downloaded, _) = load_one(dir.path(), std::slice::from_ref(&root));
        assert!(downloaded.downloaded);
        assert!(!downloaded.hooks[0].may_rewrite);
        assert_eq!(http_url(&downloaded), "https://example.test//x");
        // The summary shows the reference, never the daemon's value.
        let summary = runs(&downloaded);
        assert_eq!(
            summary.servers[0].url.as_deref(),
            Some("https://example.test/${PATH}/x")
        );
        assert_eq!(summary.servers[0].variables, ["PATH"]);
        assert_eq!(runs(&own), summary);
    }

    #[test]
    fn runs_flags_launchers_and_equal_plugins_compare_equal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        write(&root.join(".claude-plugin/plugin.json"), r#"{"name": "p"}"#);
        write(
            &root.join("hooks/hooks.json"),
            r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/check.sh"},
                {"type": "command", "command": "uvx ruff check"}]}]}}"#,
        );
        write(
            &root.join(".mcp.json"),
            r#"{"mcpServers": {
                "fetch": {"command": "npx", "args": ["-y", "pkg@latest"], "env": {"TOKEN": "${TOKEN}"}},
                "local": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/serve", "args": ["--port", "${PORT:-80}"]},
                "remote": {"type": "http", "url": "https://x.test/mcp", "headers": {"Authorization": "Bearer ${KEY}"}}}}"#,
        );
        let a = runs(&load_one(dir.path(), &[]).0);
        assert!(a.has_launcher());
        assert_eq!(a.hooks.len(), 2);
        assert!(!a.hooks[0].launcher && a.hooks[1].launcher);
        assert_eq!(a.hooks[0].event, "tool.before");
        assert_eq!(a.hooks[0].matcher, "Bash");
        assert!(a.hooks[0].command.ends_with("/check.sh"));
        let by_name = |name: &str| a.servers.iter().find(|s| s.name == name).unwrap();
        let fetch = by_name("p_fetch");
        assert!(fetch.launcher);
        assert_eq!(fetch.args, ["-y", "pkg@latest"]);
        assert_eq!(fetch.env_names, ["TOKEN"]);
        assert_eq!(fetch.variables, ["TOKEN"]);
        let local = by_name("p_local");
        assert!(!local.launcher);
        assert_eq!(local.variables, ["PORT"]);
        let remote = by_name("p_remote");
        assert_eq!(remote.transport, "http");
        assert_eq!(remote.header_names, ["Authorization"]);
        assert_eq!(remote.variables, ["KEY"]);
        // A second load of the same content compares equal and shares the digest.
        let again = runs(&load_one(dir.path(), &[]).0);
        assert_eq!(a, again);
        assert_eq!(a.digest(), again.digest());
        assert_eq!(a.digest().len(), 64);
        // Any change moves the digest.
        write(
            &root.join(".mcp.json"),
            r#"{"mcpServers": {"fetch": {"command": "node"}}}"#,
        );
        let changed = runs(&load_one(dir.path(), &[]).0);
        assert_ne!(changed.digest(), a.digest());
        assert!(changed.hooks[1].launcher);
    }

    #[test]
    fn launcher_detection() {
        let flagged =
            |line: &str| launches_fetched_code(&line.split_whitespace().collect::<Vec<_>>());
        for yes in [
            "npx -y pkg@latest",
            "npx pkg",
            "sh -c \"cd x && npx pkg\"",
            "C:\\tools\\npx.cmd pkg",
            "uvx tool",
            "bunx tool",
            "pipx run tool",
            "yarn dlx tool",
            "npm exec tool",
            "docker run --rm image:1",
            "node server.js pkg@latest",
        ] {
            assert!(flagged(yes), "{yes}");
        }
        for no in [
            "node server.js",
            "pipx install tool",
            "docker run --rm image@sha256:abc",
            "docker ps",
            "./npx-notes/run.sh",
        ] {
            assert!(!flagged(no), "{no}");
        }
    }

    #[test]
    fn the_digest_does_not_depend_on_the_plugin_folder() {
        let build = |dir: &Path| {
            write(
                &dir.join("p/.claude-plugin/plugin.json"),
                r#"{"name": "p"}"#,
            );
            write(
                &dir.join("p/hooks/hooks.json"),
                r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/check.sh"}]}]}}"#,
            );
            write(
                &dir.join("p/.mcp.json"),
                r#"{"mcpServers": {"s": {"command": "${CLAUDE_PLUGIN_ROOT}/serve", "args": ["${CLAUDE_PLUGIN_ROOT}/x"]}}}"#,
            );
        };
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        build(first.path());
        build(second.path());
        let a = runs(&load_one(first.path(), &[]).0);
        let b = runs(&load_one(second.path(), &[]).0);
        // Shown strings carry the folder; the digest does not.
        assert_ne!(a.hooks[0].command, b.hooks[0].command);
        assert_eq!(a, b);
        assert!(
            a.canonical_json()
                .contains("${CLAUDE_PLUGIN_ROOT}/check.sh")
        );
        assert_eq!(a.canonical_json(), b.canonical_json());
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn an_agent_installed_folder_is_downloaded_even_if_the_caller_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vars");
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "vars"}"#,
        );
        write(
            &root.join(".mcp.json"),
            r#"{"mcpServers": {"api": {"type": "http", "url": "https://example.test/${PATH}/x"}}}"#,
        );
        write(&root.join(SOURCE_FILE), r#"{"session_id": "s1"}"#);
        let (plugin, _) = load_one(dir.path(), &[]);
        assert!(plugin.downloaded);
        assert_eq!(http_url(&plugin), "https://example.test//x");
    }
}
