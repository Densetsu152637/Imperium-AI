//! `plugins.inspect` and `plugins.install`: fetching one plugin of a marketplace, showing what it
//! would run, and installing it, withheld when it has code.
//!
//! Inspecting copies the plugin (out of the marketplace's own clone, or from its own git
//! repository) into a staging folder under `<data>/.staging`, writes
//! `.claude-plugin/marketplace-entry.json` there, parses it and hashes it. Installing **fetches
//! again**, and installs only content with the hash the inspection showed. Neither runs a hook, a
//! server, a build step, an install script or a git hook: files are read and copied, nothing else.
//!
//! The content hash is the load-time gate's (`plugin_records::measure`): the files' paths, lengths
//! and bytes, without a top-level `.git` and `.imperium-source.json`, so the folder hashes the same
//! once installed. The copy refuses a link anywhere in the source (git writes a link as a plain
//! file under `core.symlinks=false`, so a git checkout has none on disk). An install writes the
//! record (`origin = marketplace`, no approval: nothing counts approvals yet), so a plugin with
//! hooks or servers does not load until the desktop app approves it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use imperium_api::time::unix_ms;
use imperium_extensions::marketplace::Source as PluginSource;
use imperium_extensions::plugins::{self, Plugin, inside};
use imperium_protocol::methods::{
    InstallSource, InstalledPlugin, PluginHookRun, PluginInstalled, PluginPreview, PluginServerRun,
    PluginsDeleteSecretParams, PluginsInspectFolderParams, PluginsInspectParams,
    PluginsInstallParams, PluginsInstallResult, PluginsListResult, PluginsSecretResult,
    PluginsSetEnabledParams, PluginsSetEnabledResult, PluginsSetSecretParams,
    PluginsUninstallParams, PluginsUninstallResult, PluginsUpdateParams, PluginsUpdateResult,
};

use crate::extension_install::{self, ExtensionKind, Staging, check_name, move_tree, remove_tree};
use crate::marketplaces::{self, Located};
use crate::plugin_records::{self, Origin, Record};
use crate::state::{DaemonState, plugin_switch};

const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// One install at a time: the name check and the move must not interleave.
static INSTALLS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A plugin copied into staging, parsed and hashed.
struct Staged {
    located: Located,
    /// The folder, `<staging>/out`.
    out: PathBuf,
    plugin: Plugin,
    hash: String,
    commit: Option<String>,
    /// `path`, `github` or `url`.
    source_kind: &'static str,
    /// The folder inside the marketplace, or the git URL.
    source: String,
    /// The branch, tag or commit asked for (git only).
    requested: Option<String>,
    notes: Vec<String>,
}

/// `name@marketplace` as `(name, marketplace)`.
fn split_id(id: &str) -> Result<(&str, &str), String> {
    match id.rsplit_once('@') {
        Some((name, marketplace)) if !name.is_empty() && !marketplace.is_empty() => {
            Ok((name, marketplace))
        }
        _ => Err(format!("{id:?} is not a plugin id: use name@marketplace")),
    }
}

/// Copies `from` into `to`: refuses a link (or junction) anywhere, leaves out the top level's
/// `.git` and `.imperium-source.json` (the gate leaves them out of the hash), within `budget`.
fn copy_checked(from: &Path, to: &Path, budget: &mut u64, top: bool) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    let entries =
        std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if plugin_records::is_link(&meta) {
            return Err(format!(
                "{} is a link: a plugin that holds a link is refused",
                path.display()
            ));
        }
        if top && (name == ".git" || name == extension_install::SOURCE_FILE) {
            continue;
        }
        let target = to.join(&name);
        if meta.is_dir() {
            copy_checked(&path, &target, budget, false)?;
        } else if meta.is_file() {
            *budget = budget
                .checked_sub(meta.len())
                .ok_or("the plugin is larger than 256 MiB")?;
            std::fs::copy(&path, &target)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    Ok(())
}

/// Clones `url` (at `revision`, or the default branch) into `<staging>/fetched` and copies it
/// to `out`; refuses a commit other than `pinned`. Returns the commit.
async fn fetch_git(
    url: &str,
    revision: Option<&str>,
    pinned: Option<&str>,
    staging: &Path,
    out: &Path,
) -> Result<String, String> {
    let fetched = staging.join("fetched");
    let commit = extension_install::git_fetch(url, revision, &fetched).await?;
    if let Some(pinned) = pinned
        && !commit.starts_with(&pinned.to_ascii_lowercase())
    {
        return Err(format!(
            "{url} is at {commit}, not the commit {pinned} the marketplace pins"
        ));
    }
    let mut budget = MAX_BYTES;
    copy_checked(&fetched, out, &mut budget, true)?;
    Ok(commit)
}

/// Fetches `id`'s plugin into `staging`, writes its marketplace entry, parses and hashes it.
async fn stage(
    state: &DaemonState,
    id: &str,
    revision: Option<&str>,
    staging: &Path,
) -> Result<Staged, String> {
    let (plugin_name, market_name) = split_id(id)?;
    let guard = marketplaces::lock().await;
    let located = marketplaces::locate(state, market_name, plugin_name).await?;
    let out = staging.join("out");
    let entry_source = located.entry.source.clone();
    let (source_kind, source, commit, requested) = match &entry_source {
        PluginSource::Path(rel) => {
            if revision.is_some() {
                return Err(
                    "a revision applies to a plugin in its own git repository, not one inside the marketplace"
                        .into(),
                );
            }
            let dir = if rel.is_empty() {
                located.root.clone()
            } else {
                inside(&located.root, rel)?
            };
            if !dir.is_dir() {
                return Err(format!("the marketplace has no folder {rel}"));
            }
            // Under the lock: a refresh must not replace the clone while it is copied.
            let mut budget = MAX_BYTES;
            copy_checked(&dir, &out, &mut budget, true)?;
            drop(guard);
            ("path", rel.clone(), located.commit.clone(), None)
        }
        PluginSource::Github { .. } | PluginSource::Url { .. } => {
            drop(guard);
            let url = entry_source
                .clone_url()
                .ok_or("the plugin has no git source")?;
            // A pinned commit holds whatever `revision` is asked for.
            let pinned = entry_source.sha();
            let requested = revision
                .or(entry_source.sha())
                .or(entry_source.git_ref())
                .map(str::to_string);
            let commit = fetch_git(&url, requested.as_deref(), pinned, staging, &out).await?;
            let kind = if matches!(entry_source, PluginSource::Github { .. }) {
                "github"
            } else {
                "url"
            };
            (kind, url, Some(commit), requested)
        }
        PluginSource::Unsupported(kind) => {
            return Err(format!("a {kind} plugin source is not supported"));
        }
    };
    let entry_file = out.join(plugins::ENTRY_FILE);
    if let Some(parent) = entry_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&entry_file, located.entry.canonical_json())
        .map_err(|error| format!("{}: {error}", entry_file.display()))?;
    // `downloaded`: the plugin reads no daemon environment, whatever it declares.
    let (found, problems) = plugins::discover(
        std::slice::from_ref(&out),
        Duration::from_secs(60),
        std::slice::from_ref(&out),
    );
    let plugin = found.into_iter().next().ok_or_else(|| {
        format!(
            "{id} is not a plugin: {}",
            problems
                .first()
                .map_or("it has no manifest", String::as_str)
        )
    })?;
    if plugin.name != located.entry.name {
        return Err(format!(
            "the plugin's manifest names it {} but the marketplace lists {}",
            plugin.name, located.entry.name
        ));
    }
    let seen = plugin_records::measure(&out)?;
    if let Some(link) = seen.link {
        return Err(format!("the plugin holds a link ({link})"));
    }
    Ok(Staged {
        located,
        out,
        plugin,
        hash: seen.hash,
        commit,
        source_kind,
        source,
        requested,
        notes: problems,
    })
}

fn names_in(folder: &Path, files: bool) -> Vec<String> {
    if files {
        marketplaces::markdown_names(folder)
    } else {
        marketplaces::skill_names(folder)
    }
}

/// Command files that hold `!` shell lines or `allowed-tools`, which Imperium does not run.
fn commands_with_shell(folders: &[PathBuf]) -> Vec<String> {
    let mut found = Vec::new();
    for folder in folders {
        for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let shell = text.lines().any(|line| line.trim_start().starts_with('!'))
                || text.contains("allowed-tools");
            if shell && let Some(stem) = path.file_stem() {
                found.push(stem.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

/// What a plugin would run and hold, as the trust prompt lists it.
struct Contents {
    hooks: Vec<PluginHookRun>,
    servers: Vec<PluginServerRun>,
    launcher: bool,
    skills: Vec<String>,
    commands: Vec<String>,
    agents: Vec<String>,
    commands_with_shell: Vec<String>,
    /// The digest of what the plugin would run, which an approval is bound to.
    runs_digest: String,
}

/// Reads what `plugin` would run and hold, with `${CLAUDE_PLUGIN_ROOT}` shown as `installed_at`.
/// Shared by an inspection of a marketplace plugin and of an installed folder.
fn contents(plugin: &Plugin, installed_at: &Path) -> Contents {
    let runs = plugins::runs(plugin);
    // The summary expands `${CLAUDE_PLUGIN_ROOT}` to where the plugin was read; show where it will be.
    let read_at = plugin.root.display().to_string();
    let there = installed_at.display().to_string();
    let show = |text: &str| text.replace(&read_at, &there);
    let mut command_dirs = vec![plugin.commands_dir()];
    command_dirs.extend(plugin.extra_commands.iter().cloned());
    let mut agent_dirs = vec![plugin.agents_dir()];
    agent_dirs.extend(plugin.extra_agents.iter().cloned());
    let collect = |dirs: &[PathBuf], files: bool| {
        let mut names: Vec<String> = dirs.iter().flat_map(|dir| names_in(dir, files)).collect();
        names.sort();
        names.dedup();
        names
    };
    Contents {
        hooks: runs
            .hooks
            .iter()
            .map(|hook| PluginHookRun {
                event: hook.event.clone(),
                matcher: hook.matcher.clone(),
                command: show(&hook.command),
                url: hook.url.as_deref().map(show),
                launcher: hook.launcher,
            })
            .collect(),
        servers: runs
            .servers
            .iter()
            .map(|server| PluginServerRun {
                name: server.name.clone(),
                transport: server.transport.clone(),
                command: server.command.as_deref().map(show),
                args: server.args.iter().map(|arg| show(arg)).collect(),
                env_names: server.env_names.clone(),
                url: server.url.as_deref().map(show),
                header_names: server.header_names.clone(),
                variables: server.variables.clone(),
                launcher: server.launcher,
            })
            .collect(),
        launcher: runs.has_launcher(),
        skills: names_in(&plugin.skills_dir(), false),
        commands: collect(&command_dirs, true),
        agents: collect(&agent_dirs, true),
        commands_with_shell: commands_with_shell(&command_dirs),
        runs_digest: runs.digest(),
    }
}

fn preview(state: &DaemonState, staged: &Staged, installed_at: &Path) -> PluginPreview {
    let plugin = &staged.plugin;
    let there = installed_at.display().to_string();
    let installed = installed_at.is_dir().then(|| {
        let record = plugin_records::record_for(state.paths(), installed_at);
        let same = installed_at
            .canonicalize()
            .ok()
            .and_then(|canon| plugin_records::look(&canon).ok())
            .is_some_and(|seen| seen.hash == staged.hash);
        PluginInstalled {
            path: there.clone(),
            recorded_sha256: record.map(|record| record.hash),
            same_content: same,
        }
    });
    let author = plugin.author.as_ref().map(|author| author.name.clone());
    let Contents {
        hooks,
        servers,
        launcher,
        skills,
        commands,
        agents,
        commands_with_shell,
        runs_digest,
    } = contents(plugin, installed_at);
    PluginPreview {
        id: format!("{}@{}", plugin.name, staged.located.marketplace),
        name: plugin.name.clone(),
        marketplace: staged.located.marketplace.clone(),
        marketplace_source: staged.located.source.clone(),
        marketplace_commit: staged.located.commit.clone(),
        source_kind: staged.source_kind.to_string(),
        source: staged.source.clone(),
        commit: staged.commit.clone(),
        description: plugin.description.clone(),
        version: plugin.version.clone(),
        author,
        homepage: plugin.homepage.clone(),
        license: plugin.license.clone(),
        hooks,
        servers,
        launcher,
        skills,
        commands,
        agents,
        commands_with_shell,
        sha256: staged.hash.clone(),
        runs_digest,
        installed,
        notes: staged.notes.clone(),
    }
}

/// `plugins.inspect`.
pub(crate) async fn inspect(
    state: &DaemonState,
    params: PluginsInspectParams,
) -> Result<PluginPreview, String> {
    let staging = Staging::new(state.paths().data_dir())?;
    let staged = stage(
        state,
        &params.id,
        params.revision.as_deref(),
        staging.path(),
    )
    .await?;
    let dest = state
        .paths()
        .config_dir()
        .join("plugins")
        .join(&staged.plugin.name);
    Ok(preview(state, &staged, &dest))
}

/// Windows device names, which name a device whatever extension follows (`con.txt`).
const DEVICE_NAMES: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM0",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "LPT0",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
    "CONIN$",
    "CONOUT$",
];

/// Whether `name` is one plain folder name: not empty, one path component that is not `.` or `..`
/// and holds no separator, drive or stream marker (`:`), wildcard or control character, does not
/// end in a dot or space (Windows drops them) and is not a Windows device name.
pub(crate) fn check_folder_name(name: &str) -> Result<(), String> {
    let refuse = |why: &str| {
        Err(format!(
            "{name:?} is not the name of a plugin folder: {why}"
        ))
    };
    if name.is_empty() || name.len() > 255 {
        return refuse("it is empty or too long");
    }
    if name != name.trim() {
        return refuse("it starts or ends with white space");
    }
    if name == "." || name.contains("..") {
        return refuse("it names a parent or the folder itself");
    }
    if name.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '/' | '\\' | '@' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
            )
    }) {
        return refuse("it holds a path separator or another character a folder name cannot have");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return refuse("it ends in a dot or a space");
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    if DEVICE_NAMES
        .iter()
        .any(|device| device.eq_ignore_ascii_case(stem))
    {
        return refuse("it is a reserved device name");
    }
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => refuse("it is not one relative name"),
    }
}

/// The installed plugin folder named exactly `name`, canonical. The name must be one plain folder
/// name (`check_folder_name`) and exactly that of an entry of `<config>/plugins`: a path, a plugin
/// name or an id never resolves. A link, or a folder that resolves outside `<config>/plugins`, is
/// refused rather than followed.
fn folder_named(state: &DaemonState, name: &str) -> Result<PathBuf, String> {
    check_folder_name(name)?;
    exact_folder(state, name)?
        .ok_or_else(|| format!("no plugin folder named {name:?} is installed"))
}

/// The folder an approval is asked for. A `name` that is exactly the name of an entry of
/// `<config>/plugins` (compared byte for byte, not trimmed or case-folded) is that folder, and
/// nothing else; a link is refused, not followed. Any other `name` is looked up as `resolve` does
/// (a plugin name or `name@marketplace`), and the folder it finds must not be a link either.
pub(crate) fn resolve_for_approval(state: &DaemonState, name: &str) -> Result<PathBuf, String> {
    if let Some(exact) = exact_folder(state, name)? {
        return Ok(exact);
    }
    let found = resolve(state, name)?;
    if std::fs::symlink_metadata(&found).is_ok_and(|meta| plugin_records::is_link(&meta)) {
        return Err(format!(
            "{name:?} is a link: a plugin folder that is a link is not approved"
        ));
    }
    Ok(found)
}

/// The canonical folder of the entry of `<config>/plugins` named exactly `name`, if there is one.
fn exact_folder(state: &DaemonState, name: &str) -> Result<Option<PathBuf>, String> {
    let dir = state.paths().config_dir().join("plugins");
    let missing = || format!("no plugin folder named {name:?} is installed");
    let Ok(root) = dir.canonicalize() else {
        return Ok(None);
    };
    let Some(entry) = std::fs::read_dir(&dir)
        .map_err(|_| missing())?
        .flatten()
        .find(|entry| entry.file_name().to_str() == Some(name))
    else {
        return Ok(None);
    };
    let path = entry.path();
    let meta = std::fs::symlink_metadata(&path).map_err(|_| missing())?;
    if plugin_records::is_link(&meta) {
        return Err(format!(
            "{name:?} is a link: a plugin folder that is a link is not inspected or approved"
        ));
    }
    if !meta.is_dir() {
        return Err(missing());
    }
    let canon = path.canonicalize().map_err(|_| missing())?;
    if canon.parent() != Some(root.as_path()) {
        return Err(format!(
            "{name:?} does not resolve to a folder of {}",
            dir.display()
        ));
    }
    Ok(Some(canon))
}

/// Whether `text` names a path under a `.git` folder (`x/.git/hooks/h.sh`, `x\.git`), which the
/// content hash leaves out when it is the plugin folder's own top-level `.git`.
pub(crate) fn mentions_dot_git(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.match_indices(".git").any(|(at, _)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + 4..].chars().next();
        matches!(before, Some('/' | '\\'))
            && after.is_none_or(|c| matches!(c, '/' | '\\' | '"' | '\'') || c.is_whitespace())
    })
}

/// `plugins.inspect_folder`: what the installed plugin folder `name` would run, read from the
/// folder itself with the gate's own hash and run digest (`plugin_records::content`), so an
/// approval of exactly these values is the one the gate honours. Nothing in it runs.
pub(crate) fn inspect_folder(
    state: &DaemonState,
    params: &PluginsInspectFolderParams,
) -> Result<PluginPreview, String> {
    if plugin_records::Subject::parse(params.kind.as_deref())? == plugin_records::Subject::Skill {
        return inspect_skill_folder(state, params);
    }
    let folder = folder_named(state, &params.name)?;
    let (canon, sha256, _, plugin) = plugin_records::content(&folder)?;
    // The folder read is the folder named, and it is still that folder, not a link, now that it was
    // hashed.
    if canon != folder || folder_named(state, &params.name)? != canon {
        return Err(format!("{:?} changed while it was read", params.name));
    }
    let record = plugin_records::record_for(state.paths(), &canon);
    let shown = extension_install::display_path(&canon);
    let shown_at = PathBuf::from(&shown);
    let Contents {
        hooks,
        servers,
        launcher,
        skills,
        commands,
        agents,
        commands_with_shell,
        runs_digest,
    } = contents(&plugin, &shown_at);
    let mut notes = Vec::new();
    // The hash leaves out the folder's own top-level `.git` and `.imperium-source.json`: a hook or
    // server that runs from `.git` can change without the approval being voided.
    let runs_from_git = hooks
        .iter()
        .flat_map(|hook| std::iter::once(&hook.command).chain(hook.url.iter()))
        .chain(servers.iter().flat_map(|server| {
            server
                .command
                .iter()
                .chain(server.args.iter())
                .chain(server.url.iter())
        }))
        .any(|text| mentions_dot_git(text));
    if runs_from_git {
        notes.push(
            "WARNING: a hook or server runs a path under .git, which the content hash does not cover: changing it will not withhold this plugin again"
                .to_string(),
        );
    }
    match &record {
        None => notes.push(
            "Imperium has no record of this folder: it was not installed by Imperium".to_string(),
        ),
        Some(record) if record.hash != sha256 => {
            notes.push("its content changed since Imperium recorded it".to_string());
        }
        Some(_) => {}
    }
    if let Some(record) = record
        .as_ref()
        .filter(|record| record.origin == Origin::Agent)
    {
        notes.push(format!(
            "An agent installed this plugin{}. It is withheld, with its skills, commands and agents, until you approve it.",
            record
                .source
                .as_deref()
                .map_or(String::new(), |source| format!(" from {source}"))
        ));
    }
    Ok(PluginPreview {
        id: params.name.clone(),
        name: plugin.name.clone(),
        marketplace: String::new(),
        marketplace_source: String::new(),
        marketplace_commit: None,
        source_kind: "folder".into(),
        source: shown.clone(),
        commit: record.as_ref().and_then(|record| record.commit.clone()),
        description: plugin.description.clone(),
        version: plugin.version.clone(),
        author: plugin.author.as_ref().map(|author| author.name.clone()),
        homepage: plugin.homepage.clone(),
        license: plugin.license.clone(),
        hooks,
        servers,
        launcher,
        skills,
        commands,
        agents,
        commands_with_shell,
        sha256,
        runs_digest,
        installed: Some(PluginInstalled {
            path: shown,
            recorded_sha256: record.map(|record| record.hash),
            same_content: true,
        }),
        notes,
    })
}

/// `plugins.inspect_folder` with `kind: "skill"`: what the skill folder installed under
/// `<config>/skills/<name>` holds, with the content hash the gate computes and the constant digest
/// an approval of a skill is bound to. Nothing in it runs.
fn inspect_skill_folder(
    state: &DaemonState,
    params: &PluginsInspectFolderParams,
) -> Result<PluginPreview, String> {
    let folder = crate::skill_records::exact_folder(state.paths(), &params.name)?;
    let (canon, sha256, runs_digest, name) = crate::skill_records::content(&folder)?;
    if canon != folder || crate::skill_records::exact_folder(state.paths(), &params.name)? != canon
    {
        return Err(format!("{:?} changed while it was read", params.name));
    }
    let record = crate::skill_records::record_for(state.paths(), &canon);
    let shown = extension_install::display_path(&canon);
    let text = std::fs::read_to_string(canon.join("SKILL.md")).unwrap_or_default();
    let (fields, body) = imperium_api::markdown::front_matter(&text);
    let mut notes = vec![
        format!(
            "The folder is {} and the skill's name in its front matter is {}.",
            params.name, name
        ),
        "This is a skill, not a plugin. Approving lets agents list it and load its instructions \
         in every later session. It runs nothing itself, but an agent follows what it says."
            .to_string(),
    ];
    if let Some(source) = record.as_ref().and_then(|record| record.source.as_deref()) {
        notes.push(format!("An agent installed it from {source}."));
    }
    match &record {
        None => notes.push(
            "Imperium has no record of this folder's content: it was installed before approvals existed"
                .to_string(),
        ),
        Some(record) if record.hash != sha256 => {
            notes.push("its content changed since Imperium recorded it".to_string());
        }
        Some(_) => {}
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&canon).into_iter().flatten().flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        if file != "SKILL.md" && file != plugins::SOURCE_FILE && file != ".git" {
            files.push(file);
        }
    }
    files.sort();
    if !files.is_empty() {
        files.truncate(30);
        notes.push(format!(
            "Other files in it (which the skill can tell an agent to read or run): {}",
            files.join(", ")
        ));
    }
    notes.push(format!(
        "Its instructions begin: {}",
        imperium_api::truncate_chars(body.trim(), 1200)
    ));
    let same_content = record.as_ref().is_some_and(|record| record.hash == sha256);
    Ok(PluginPreview {
        id: params.name.clone(),
        name: name.clone(),
        marketplace: String::new(),
        marketplace_source: String::new(),
        marketplace_commit: None,
        source_kind: "skill".into(),
        source: shown.clone(),
        commit: record.as_ref().and_then(|record| record.commit.clone()),
        description: fields.get("description").cloned().unwrap_or_default(),
        version: None,
        author: None,
        homepage: None,
        license: None,
        hooks: Vec::new(),
        servers: Vec::new(),
        launcher: false,
        skills: vec![name],
        commands: Vec::new(),
        agents: Vec::new(),
        commands_with_shell: Vec::new(),
        sha256,
        runs_digest,
        installed: Some(PluginInstalled {
            path: shown,
            same_content,
            recorded_sha256: record.map(|record| record.hash),
        }),
        notes,
    })
}

/// The folder an approval of `subject` named `name` is asked for.
pub(crate) fn resolve_subject(
    state: &DaemonState,
    subject: plugin_records::Subject,
    name: &str,
) -> Result<PathBuf, String> {
    match subject {
        plugin_records::Subject::Plugin => resolve_for_approval(state, name),
        plugin_records::Subject::Skill => crate::skill_records::exact_folder(state.paths(), name),
    }
}

/// Creates the install folder exclusively: anything already there (a folder that appeared since
/// the name check, or one that a name like `foo.` maps to on Windows) is an error, so nothing is
/// ever moved into, merged with or written over an existing folder.
fn claim(dest: &Path) -> Result<(), String> {
    std::fs::create_dir(dest).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "{} already exists: it is not installed over",
                dest.display()
            )
        } else {
            format!("{}: {error}", dest.display())
        }
    })
}

/// What `.imperium-source.json` records for a staged plugin.
fn install_source(staged: &Staged) -> InstallSource {
    InstallSource {
        kind: if staged.source_kind == "path" {
            "path"
        } else {
            "git"
        }
        .into(),
        location: if staged.source_kind == "path" {
            staged.located.source.clone()
        } else {
            staged.source.clone()
        },
        revision: staged.commit.clone(),
        requested: staged.requested.clone(),
        subdir: (staged.source_kind == "path" && !staged.source.is_empty())
            .then(|| staged.source.clone()),
        installed_at_unix_ms: unix_ms(),
        session_id: None,
    }
}

/// The gate's reason for withholding the installed folder `dest`, with where to approve it.
fn withheld_reason(state: &DaemonState, dest: &Path) -> Option<String> {
    let gate = plugin_records::gate(state.paths(), plugin_records::production_verifier());
    gate.withheld
        .iter()
        .find(|withheld| withheld.folder == dest)
        .map(|withheld| {
            format!(
                "{}. It loads after you approve it in the Imperium desktop app",
                withheld.reason
            )
        })
}

/// A staged plugin that now sits in `<config>/plugins` with its record.
struct Placed {
    name: String,
    dest: PathBuf,
    source: InstallSource,
    /// `name@marketplace`.
    id: String,
}

/// Moves a staged plugin into `<config>/plugins` and records it with `origin` and never an
/// approval. The caller holds the install lock. A failure leaves neither folder nor record.
fn place(
    state: &DaemonState,
    staged: &Staged,
    origin: Origin,
    session_id: Option<&str>,
) -> Result<Placed, String> {
    let name = check_name(&staged.plugin.name)?;
    let plugins_dir = state.paths().config_dir().join("plugins");
    std::fs::create_dir_all(&plugins_dir)
        .map_err(|error| format!("{}: {error}", plugins_dir.display()))?;
    // Folder names that differ only by letter case are one folder on Windows and macOS.
    for entry in std::fs::read_dir(&plugins_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let existing = entry.file_name().to_string_lossy().into_owned();
        if existing.eq_ignore_ascii_case(&name) {
            return Err(if existing == name {
                format!("a plugin folder named {name} is already installed")
            } else {
                format!(
                    "a plugin folder named {existing} is already installed and {name} would collide with it by letter case"
                )
            });
        }
    }
    let dest = plugins_dir.join(&name);
    claim(&dest)?;
    if let Err(error) = move_tree(&staged.out, &dest) {
        remove_tree(&dest);
        return Err(error);
    }
    let mut source = install_source(staged);
    source.session_id = session_id.map(str::to_string);
    let id = format!("{name}@{}", staged.located.marketplace);
    let recorded = extension_install::write_source(&dest, &source).and_then(|()| {
        plugin_records::add_record(
            state.paths(),
            &dest,
            Record {
                name: name.clone(),
                origin,
                id: Some(id.clone()),
                source: Some(staged.source.clone()),
                commit: staged.commit.clone(),
                hash: staged.hash.clone(),
                runs: plugins::runs(&staged.plugin).digest(),
                approval: None,
            },
        )
    });
    if let Err(error) = recorded {
        remove_tree(&dest);
        return Err(error);
    }
    Ok(Placed {
        name,
        dest,
        source,
        id,
    })
}

/// `plugins.install`.
pub(crate) async fn install(
    state: &DaemonState,
    params: PluginsInstallParams,
) -> Result<PluginsInstallResult, String> {
    let _serial = INSTALLS.lock().await;
    let staging = Staging::new(state.paths().data_dir())?;
    let staged = stage(
        state,
        &params.id,
        params.revision.as_deref(),
        staging.path(),
    )
    .await?;
    if !staged.hash.eq_ignore_ascii_case(params.sha256.trim()) {
        return Err(format!(
            "the plugin is not what was inspected (its hash is {}, not {}): inspect it again",
            staged.hash, params.sha256
        ));
    }
    let Placed {
        name,
        dest,
        source,
        id,
    } = place(state, &staged, Origin::Marketplace, None)?;
    crate::config_changes::plugin_installed(state, &id, &dest, &source, &staged.hash);
    let mut notes = staged.notes.clone();
    if params.approve.is_some() {
        notes.push(
            "approve was ignored: an install never approves; approval is only given in the desktop app"
                .into(),
        );
    }
    if params.environment.is_some() {
        notes.push("environment was ignored: plugins are not turned on per environment yet".into());
    }
    let reason = withheld_reason(state, &dest);
    Ok(PluginsInstallResult {
        name,
        path: dest.display().to_string(),
        withheld: reason.is_some(),
        reason,
        notes,
    })
}

/// `install_extension` with `kind = "plugin"` and `source = "name@marketplace"`: an agent's install
/// from a marketplace the user added. Nothing is shown to a person first, so the install is
/// recorded with `origin = agent` and no approval, and a plugin with hooks or servers is withheld
/// (and so is not run) until the user approves it in the desktop app, exactly as for an agent's
/// install from a git URL.
pub(crate) async fn install_for_agent(
    state: &DaemonState,
    id: &str,
    revision: Option<&str>,
    session_id: Option<&str>,
) -> Result<extension_install::Installed, String> {
    // A record can be written only after the first-start migration.
    state.migrate_plugins();
    plugin_records::writable(state.paths())?;
    let _serial = INSTALLS.lock().await;
    let staging = Staging::new(state.paths().data_dir())?;
    let staged = stage(state, id, revision, staging.path()).await?;
    let Placed {
        name, dest, source, ..
    } = place(state, &staged, Origin::Agent, session_id)?;
    let mut notes = staged.notes.clone();
    let withheld = withheld_reason(state, &dest);
    notes.push(match &withheld {
        Some(_) => "It is installed withheld and unapproved: it adds no skills, commands or agents to any session, and activate_plugin cannot start its servers, until the user approves it in the Imperium desktop app (Plugins, Installed). Tell the user it is waiting there."
            .into(),
        None => "It is withheld until the user approves it in the Imperium desktop app (Plugins, Installed); tell the user it is waiting there."
            .into(),
    });
    Ok(extension_install::Installed {
        kind: ExtensionKind::Plugin,
        name,
        path: dest,
        source,
        // Withheld by the gate, not by `extensions.disabled_plugins`: nothing to take off on undo.
        disabled: false,
        sha256: Some(staged.hash),
        notes,
    })
}

/// The folder under `<config>/plugins` that `text` names: a folder name, a recorded name, or a
/// recorded `name@marketplace`, all compared ignoring letter case. A folder name wins over a
/// recorded one; more than one folder of the same kind of match is an error.
pub(crate) fn resolve(state: &DaemonState, text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("name the plugin to uninstall".into());
    }
    let dir = state.paths().config_dir().join("plugins");
    let mut by_folder = Vec::new();
    let mut by_record = Vec::new();
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(text)
        {
            by_folder.push(path);
        } else if let Some(record) = plugin_records::record_for(state.paths(), &path)
            && (record.name.eq_ignore_ascii_case(text)
                || record
                    .id
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(text)))
        {
            by_record.push(path);
        }
    }
    // A folder's name can differ from its plugin's name: a folder match and a record match on
    // different folders is ambiguous, whichever way the name was meant.
    let mut found = by_folder;
    found.extend(by_record);
    found.sort();
    match found.len() {
        0 => Err(format!(
            "no plugin named {text} is installed in {}",
            dir.display()
        )),
        1 => Ok(found.remove(0)),
        _ => Err(format!(
            "{text} names more than one plugin folder ({}): use the folder name",
            found
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The secret store's name for the variable `variable` of the plugin `plugin`:
/// `plugin.<plugin>.<variable>`, lowercased. Names compare ignoring case, so `FOO` and `foo` are
/// one secret; one over 64 characters or with a character a secret name cannot hold is refused.
pub(crate) fn secret_name(
    plugin: &str,
    variable: &str,
) -> Result<imperium_config::SecretName, String> {
    // Only a plain variable name: a `.` could name another plugin's secret (`${b.x}` of plugin `a`
    // would be `plugin.a.b.x`, plugin `a.b`'s `x`).
    if !plugins::is_variable(variable) {
        return Err(format!(
            "{variable} cannot be a secret: a variable name is letters, digits and '_', not starting with a digit"
        ));
    }
    format!(
        "plugin.{}.{}",
        plugin.to_ascii_lowercase(),
        variable.to_ascii_lowercase()
    )
    .parse()
    .map_err(|_| {
        format!(
            "{variable} of {plugin} cannot be a secret: its store name, plugin.<plugin>.<variable> in lower case, may hold letters, digits, '.', '_' and '-' and be at most 64 characters"
        )
    })
}

/// Reads a downloaded plugin's `${VAR}` from the secret store (see [`secret_name`]).
pub(crate) fn plugin_secrets(
    store: std::sync::Arc<dyn imperium_config::SecretStore>,
) -> plugins::PluginSecrets {
    plugins::PluginSecrets::new(move |plugin, variable| {
        let name = secret_name(plugin, variable).ok()?;
        store.load(&name).ok().flatten()
    })
}

/// The folder and plugin a secret call names. Only a plugin installed from a marketplace or by an
/// agent reads saved secrets, and only a variable its servers name; of two variables that differ
/// only by letter case the first in sorted order may be set and the other is refused.
fn secret_target(
    state: &DaemonState,
    name: &str,
    variable: &str,
) -> Result<(Plugin, imperium_config::SecretName), String> {
    let dir = resolve(state, name)?;
    let record = plugin_records::record_for(state.paths(), &dir);
    if !record.is_some_and(|record| record.origin.downloaded()) {
        return Err(
            "only a plugin installed from a marketplace or by an agent, or one you approved yourself, reads saved secrets"
                .into(),
        );
    }
    let plugin =
        plugin_records::parse(&dir).ok_or("the folder holds no plugin that can be read")?;
    if !plugin
        .required_secrets
        .iter()
        .any(|known| known == variable)
    {
        return Err(format!(
            "{} has no server that needs {variable}: it needs {}",
            plugin.name,
            if plugin.required_secrets.is_empty() {
                "none".to_string()
            } else {
                plugin.required_secrets.join(", ")
            }
        ));
    }
    if let Some(other) = plugin
        .required_secrets
        .iter()
        .find(|other| other.as_str() < variable && other.eq_ignore_ascii_case(variable))
    {
        return Err(format!(
            "{variable} and {other} would be one saved secret (names compare ignoring letter case): {variable} is refused"
        ));
    }
    let secret = secret_name(&plugin.name, variable)?;
    Ok((plugin, secret))
}

/// `plugins.set_secret`. The value goes to the secret store and nowhere else, and not into a
/// result or an error.
pub(crate) fn set_secret(
    state: &DaemonState,
    params: PluginsSetSecretParams,
) -> Result<PluginsSecretResult, String> {
    let (plugin, secret) = secret_target(state, &params.name, &params.variable)?;
    if params.value.is_empty() || params.value.len() > 64 * 1024 {
        return Err("a secret value must be 1 byte to 64 KiB".into());
    }
    state
        .secrets()
        .store(&secret, &params.value)
        .map_err(|error| format!("could not save the secret: {error}"))?;
    Ok(PluginsSecretResult {
        name: plugin.name,
        variable: params.variable,
        secret: secret.to_string(),
        changed: true,
    })
}

/// `plugins.delete_secret`.
pub(crate) fn delete_secret(
    state: &DaemonState,
    params: PluginsDeleteSecretParams,
) -> Result<PluginsSecretResult, String> {
    let (plugin, secret) = secret_target(state, &params.name, &params.variable)?;
    let changed = state
        .secrets()
        .delete(&secret)
        .map_err(|error| format!("could not delete the secret: {error}"))?;
    Ok(PluginsSecretResult {
        name: plugin.name,
        variable: params.variable,
        secret: secret.to_string(),
        changed,
    })
}

/// The plugin's name: its record's, else its manifest's, else its folder's.
fn plugin_name(state: &DaemonState, dir: &Path, asked: &str) -> String {
    plugin_records::record_for(state.paths(), dir)
        .map(|record| record.name)
        .or_else(|| plugin_records::parse(dir).map(|plugin| plugin.name))
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| asked.trim().to_string())
}

/// `plugins.uninstall`: moves the folder to `<data>/config-trash`, removes its record, and
/// journals both so `config.undo` puts them back.
pub(crate) async fn uninstall(
    state: &DaemonState,
    params: PluginsUninstallParams,
) -> Result<PluginsUninstallResult, String> {
    let _serial = INSTALLS.lock().await;
    let dir = resolve(state, &params.name)?;
    let name = plugin_name(state, &dir, &params.name);
    // Read before the folder moves: the variables its secrets were saved under.
    let secrets: Vec<(String, String)> = if params.delete_secrets {
        plugin_records::parse(&dir)
            .map(|plugin| {
                plugin
                    .required_secrets
                    .iter()
                    .map(|variable| (plugin.name.clone(), variable.clone()))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let (change, mut notes) =
        crate::config_changes::uninstall_folder(state, None, ExtensionKind::Plugin, &name, &dir)?;
    Ok(PluginsUninstallResult {
        name,
        path: change.path.clone().unwrap_or_default(),
        kept_at: change.kept_at.clone().unwrap_or_default(),
        change_id: change.id,
        notes: {
            if params.delete_secrets {
                let deleted = secrets
                    .iter()
                    .filter(|(plugin, variable)| {
                        secret_name(plugin, variable)
                            .ok()
                            .is_some_and(|secret| state.secrets().delete(&secret).unwrap_or(false))
                    })
                    .count();
                notes.push(format!(
                    "deleted {deleted} saved secret(s); undo does not bring them back"
                ));
            } else {
                notes.push(format!(
                    "undo it with config.undo {}; the plugin's secrets were not touched",
                    change.id
                ));
            }
            notes
        },
    })
}

/// `plugins.list`: every plugin folder under `<config>/plugins`, with the gate's verdict.
pub(crate) fn list(
    state: &DaemonState,
    environment: Option<&str>,
) -> Result<PluginsListResult, String> {
    let scope = scope(state, environment)?;
    let gate = plugin_records::gate(state.paths(), plugin_records::production_verifier());
    let mut folders: Vec<(PathBuf, Option<String>)> = gate
        .allowed
        .iter()
        .map(|folder| (folder.clone(), None))
        .chain(
            gate.withheld
                .iter()
                .map(|withheld| (withheld.folder.clone(), Some(withheld.reason.clone()))),
        )
        .collect();
    folders.sort();
    let mut problems = Vec::new();
    let plugins = folders
        .into_iter()
        .map(|(folder, reason)| {
            let record = plugin_records::record_for(state.paths(), &folder);
            let parsed = plugin_records::parse(&folder);
            let name = record
                .as_ref()
                .map(|record| record.name.clone())
                .or_else(|| parsed.as_ref().map(|plugin| plugin.name.clone()))
                .unwrap_or_else(|| {
                    folder
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            let switch = plugin_switch(&name, &scope.user, &scope.repository, &scope.environment);
            // Only a downloaded plugin reads saved secrets.
            let required = match (&record, &parsed) {
                (Some(record), Some(plugin)) if record.origin.downloaded() => {
                    plugin.required_secrets.clone()
                }
                _ => Vec::new(),
            };
            InstalledPlugin {
                enabled: switch.on,
                decided_by: switch.decided_by.into(),
                id: record.as_ref().and_then(|record| record.id.clone()),
                origin: record.as_ref().map(|record| record.origin.as_str().into()),
                source: record.as_ref().and_then(|record| record.source.clone()),
                commit: record.as_ref().and_then(|record| record.commit.clone()),
                approval: record
                    .as_ref()
                    .and_then(|record| record.approval.as_ref())
                    .map(|approval| approval.kind.as_str().into()),
                withheld: reason.is_some(),
                reason,
                hooks: parsed.as_ref().map_or(0, |plugin| {
                    u32::try_from(plugin.hooks.len()).unwrap_or(u32::MAX)
                }),
                servers: parsed
                    .iter()
                    .flat_map(|plugin| plugin.servers.iter().map(|server| server.name.clone()))
                    .collect(),
                required_secrets: required.clone(),
                missing_secrets: required
                    .iter()
                    .filter(|variable| {
                        secret_name(&name, variable)
                            .ok()
                            .and_then(|secret| state.secrets().load(&secret).ok().flatten())
                            .is_none()
                    })
                    .cloned()
                    .collect(),
                folder: folder.display().to_string(),
                name,
            }
        })
        .collect();
    let dir = state.paths().config_dir().join("plugins");
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir()
            && !gate.allowed.contains(&path)
            && !gate.withheld.iter().any(|withheld| withheld.folder == path)
        {
            problems.push(format!(
                "{} holds no plugin (no .claude-plugin/plugin.json or marketplace-entry.json)",
                path.display()
            ));
        }
    }
    problems.sort();
    Ok(PluginsListResult { plugins, problems })
}

/// The lists that decide a plugin's switch for the user, or for one environment.
struct Scope {
    user: Vec<String>,
    repository: Vec<String>,
    environment: imperium_protocol::methods::EnvironmentPlugins,
}

fn environment_id(
    state: &DaemonState,
    environment: Option<&str>,
) -> Result<Option<String>, String> {
    match environment {
        Some(text) => Ok(Some(
            state
                .environments()
                .find(text)
                .map_err(|error| error.to_string())?
                .id,
        )),
        None => Ok(None),
    }
}

fn scope(state: &DaemonState, environment: Option<&str>) -> Result<Scope, String> {
    let environment_id = environment_id(state, environment)?;
    let narrowed = state.environment_settings(environment_id.as_deref());
    let (user, repository, environment) = state.plugin_lists(
        &narrowed.settings.extensions.disabled_plugins,
        &narrowed.repo_disabled_plugins,
        environment_id.as_deref(),
    );
    Ok(Scope {
        user,
        repository,
        environment,
    })
}

/// `plugins.set_enabled`.
pub(crate) fn set_enabled(
    state: &DaemonState,
    params: PluginsSetEnabledParams,
) -> Result<PluginsSetEnabledResult, String> {
    let dir = resolve(state, &params.name)?;
    let name = plugin_name(state, &dir, &params.name);
    if params.enabled {
        let gate = plugin_records::gate(state.paths(), plugin_records::production_verifier());
        if let Some(withheld) = gate.withheld.iter().find(|withheld| withheld.folder == dir) {
            return Err(format!(
                "{name} is withheld, so it cannot be turned on: {}",
                withheld.reason
            ));
        }
    }
    let target = environment_id(state, params.environment.as_deref())?;
    match &target {
        Some(id) => {
            state
                .environments()
                .set_plugin(id, &name, params.enabled)
                .map_err(|error| error.to_string())?;
        }
        None => extension_install::set_plugin_disabled(state, &name, !params.enabled)?,
    }
    let scope = scope(state, target.as_deref())?;
    let switch = plugin_switch(&name, &scope.user, &scope.repository, &scope.environment);
    let mut notes = Vec::new();
    if params.enabled && !switch.on {
        notes.push(format!(
            "it is still off: {} decides that",
            switch.decided_by
        ));
    }
    Ok(PluginsSetEnabledResult {
        name,
        enabled: switch.on,
        decided_by: switch.decided_by.into(),
        notes,
    })
}

/// `plugins.update`: replaces an installed marketplace plugin with what its source holds now.
///
/// The swap leaves no moment in which an old approval meets new content: the old record leaves
/// the store first, the old folder moves to `config-trash`, the new folder is created exclusively
/// and filled, and only then is the new record (no approval) written. Between the first two steps
/// the folder has no record, so the gate withholds its code; after them the path is empty; between
/// the third and the last the new content has no record, so its code is withheld too. Every
/// failure puts the old folder and record back.
pub(crate) async fn update(
    state: &DaemonState,
    params: PluginsUpdateParams,
) -> Result<PluginsUpdateResult, String> {
    let _serial = INSTALLS.lock().await;
    let text = params.name.trim();
    let (plain, wanted) = match text.rsplit_once('@') {
        Some((name, market)) if !name.is_empty() && !market.is_empty() => (name, Some(market)),
        _ => (text, None),
    };
    let dir = resolve(state, plain)?;
    let record = plugin_records::record_for(state.paths(), &dir)
        .ok_or("there is no record of where this plugin came from: install it again")?;
    let id = match (&record.origin, &record.id) {
        (Origin::Marketplace, Some(id)) => id.clone(),
        _ => {
            return Err(format!(
                "{} was not installed from a marketplace, so it cannot be updated",
                record.name
            ));
        }
    };
    let (_, market) = split_id(&id)?;
    if let Some(wanted) = wanted
        && !wanted.eq_ignore_ascii_case(market)
    {
        return Err(format!(
            "{} came from the marketplace {market}, not {wanted}: an update never moves a plugin between marketplaces",
            record.name
        ));
    }
    let staging = Staging::new(state.paths().data_dir())?;
    // The source is the record's (`id` names the marketplace entry), never the folder's.
    let staged = stage(state, &id, None, staging.path()).await?;
    if !staged.hash.eq_ignore_ascii_case(params.sha256.trim()) {
        return Err(format!(
            "the plugin is not what was inspected (its hash is {}, not {}): inspect it again",
            staged.hash, params.sha256
        ));
    }
    if staged.plugin.name != record.name {
        return Err(format!(
            "the plugin is now named {} (installed as {}): uninstall and install it again",
            staged.plugin.name, record.name
        ));
    }
    if record.source.as_deref() != Some(staged.source.as_str()) {
        return Err(format!(
            "the marketplace now lists {} as {}'s source (installed from {}): it is not updated; uninstall and install it again",
            staged.source,
            record.name,
            record.source.as_deref().unwrap_or("an unknown source")
        ));
    }
    let canon = dir
        .canonicalize()
        .map_err(|error| format!("{}: {error}", dir.display()))?;
    let current = plugin_records::look(&canon)?.hash;
    let path = dir.display().to_string();
    if current == staged.hash && record.hash == staged.hash {
        let reason = withheld_reason(state, &dir);
        return Ok(PluginsUpdateResult {
            name: record.name,
            path,
            up_to_date: true,
            replaced: false,
            kept_at: None,
            withheld: reason.is_some(),
            reason,
            notes: vec!["the installed content is the one inspected: nothing changed".into()],
        });
    }
    let key = canon.to_string_lossy().into_owned();
    extension_install::write_source(&staged.out, &install_source(&staged))?;
    let fresh = Record {
        name: record.name.clone(),
        origin: Origin::Marketplace,
        id: Some(id),
        source: Some(staged.source.clone()),
        commit: staged.commit.clone(),
        hash: staged.hash.clone(),
        runs: plugins::runs(&staged.plugin).digest(),
        approval: None,
    };
    let trash = extension_install::trash_dir(state);
    std::fs::create_dir_all(&trash)
        .map_err(|error| format!("could not create {}: {error}", trash.display()))?;
    let kept = trash.join(format!("{}-plugin-{}", unix_ms(), record.name));
    // 1. The old record leaves the store (an unreadable store stops the update here).
    let old = plugin_records::take_record(state.paths(), &key)?;
    let put_back = |folder_moved: bool| {
        if folder_moved {
            remove_tree(&dir);
            if let Err(error) = move_tree(&kept, &dir) {
                tracing::warn!(%error, "could not put the old plugin folder back");
            }
        }
        if let Some(old) = &old
            && let Err(error) =
                plugin_records::add_record_at(state.paths(), key.clone(), old.clone())
        {
            tracing::warn!(%error, "could not put the old plugin record back");
        }
    };
    // 2. The old folder moves aside.
    if let Err(error) = move_tree(&dir, &kept) {
        put_back(false);
        return Err(error);
    }
    // 3. The new folder is created exclusively and filled.
    if let Err(error) = claim(&dir).and_then(|()| move_tree(&staged.out, &dir)) {
        put_back(true);
        return Err(error);
    }
    // 4. The new record, with no approval.
    if let Err(error) = plugin_records::add_record_at(state.paths(), key.clone(), fresh) {
        put_back(true);
        return Err(error);
    }
    let reason = withheld_reason(state, &dir);
    let mut notes = staged.notes.clone();
    notes.push(format!("the previous folder is kept in {}", kept.display()));
    Ok(PluginsUpdateResult {
        name: record.name,
        path,
        up_to_date: false,
        replaced: true,
        kept_at: Some(kept.display().to_string()),
        withheld: reason.is_some(),
        reason,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::Arc;

    use imperium_config::{Config, Layer, Paths};
    use imperium_protocol::methods::{
        PluginsMarketplaceAddParams, PluginsMarketplaceRefreshParams,
    };
    use serde_json::json;

    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A folder marketplace `mk` listing a plugin with a hook and a server (that would leave
    /// `ran.marker` and `server.marker` if anything ran them), a skills-only one, a `strict: false`
    /// one, one whose manifest names it differently, and one that tests give a link.
    fn write_market(dir: &Path) {
        write(
            &dir.join(".claude-plugin/marketplace.json"),
            &json!({"name": "mk", "owner": {"name": "Owner"}, "plugins": [
                {"name": "hooked", "source": "./hooked", "description": "Hooks"},
                {"name": "plain", "source": "./plain"},
                {"name": "plain.", "source": "./plain"},
                {"name": "loose", "source": "./loose", "strict": false, "description": "Loose"},
                {"name": "mismatch", "source": "./mismatch"},
                {"name": "linky", "source": "./linky"},
                {"name": "keyed", "source": "./keyed"},
            ]})
            .to_string(),
        );
        write(
            &dir.join("hooked/.claude-plugin/plugin.json"),
            r#"{"name": "hooked", "version": "1.0.0"}"#,
        );
        write(
            &dir.join("hooked/hooks/hooks.json"),
            r#"{"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/hook.sh ran.marker"}]}]}}"#,
        );
        write(
            &dir.join("hooked/.mcp.json"),
            r#"{"mcpServers": {"srv": {"command": "touch", "args": ["server.marker"]}}}"#,
        );
        write(&dir.join("hooked/hook.sh"), "touch ran.marker\n");
        write(
            &dir.join("hooked/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review\n---\nbody\n",
        );
        write(
            &dir.join("plain/.claude-plugin/plugin.json"),
            r#"{"name": "plain"}"#,
        );
        write(
            &dir.join("plain/skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\nbody\n",
        );
        write(
            &dir.join("loose/skills/note/SKILL.md"),
            "---\nname: note\ndescription: Notes\n---\nbody\n",
        );
        write(
            &dir.join("mismatch/.claude-plugin/plugin.json"),
            r#"{"name": "other"}"#,
        );
        write(
            &dir.join("linky/.claude-plugin/plugin.json"),
            r#"{"name": "linky"}"#,
        );
        write(&dir.join("linky/skills/s/SKILL.md"), "---\nname: s\n---\n");
        write(
            &dir.join("keyed/.claude-plugin/plugin.json"),
            r#"{"name": "keyed"}"#,
        );
        // `FOO` and `foo` are one secret; the long name makes `plugin.keyed.<var>` 65 characters.
        write(
            &dir.join("keyed/.mcp.json"),
            &json!({"mcpServers": {
                "api": {"command": "echo", "env": {"TOKEN": "${API_KEY}"}},
                "case": {"command": "echo", "env": {"A": "${FOO}", "B": "${foo}"}},
                "long": {"command": "echo", "env": {"C": format!("${{{}}}", "A".repeat(52))}},
            }})
            .to_string(),
        );
    }

    /// A state in a fresh home that has run the migration, and a folder marketplace added to it.
    async fn setup() -> (tempfile::TempDir, tempfile::TempDir, Arc<DaemonState>) {
        let home = tempfile::tempdir().unwrap();
        let market = tempfile::tempdir().unwrap();
        write_market(market.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::with_secrets(
            Paths::under(home.path()),
            settings,
            None,
            Arc::new(imperium_config::MemorySecrets::new()),
        );
        state.migrate_plugins();
        marketplaces::add(
            &state,
            PluginsMarketplaceAddParams {
                source: market.path().display().to_string(),
                revision: None,
            },
        )
        .await
        .unwrap();
        (home, market, state)
    }

    fn inspect_params(id: &str) -> PluginsInspectParams {
        PluginsInspectParams {
            id: id.into(),
            revision: None,
        }
    }

    fn install_params(id: &str, sha256: &str) -> PluginsInstallParams {
        PluginsInstallParams {
            id: id.into(),
            sha256: sha256.into(),
            revision: None,
            environment: None,
            approve: None,
        }
    }

    fn dest(state: &DaemonState, name: &str) -> PathBuf {
        state.paths().config_dir().join("plugins").join(name)
    }

    fn all_files(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(all_files(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    fn assert_nothing_ran(dirs: &[&Path]) {
        for dir in dirs {
            for file in all_files(dir) {
                let name = file.file_name().unwrap().to_string_lossy().into_owned();
                assert!(
                    name != "ran.marker" && name != "server.marker",
                    "{} was written",
                    file.display()
                );
            }
        }
    }

    fn gate(state: &DaemonState) -> plugin_records::Gate {
        plugin_records::gate(state.paths(), plugin_records::production_verifier())
    }

    #[tokio::test]
    async fn a_plugin_with_code_is_inspected_then_installed_recorded_and_withheld() {
        let (home, market, state) = setup().await;
        let preview = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        assert_eq!(preview.id, "hooked@mk");
        assert_eq!(preview.source_kind, "path");
        assert_eq!(preview.hooks.len(), 1);
        // Shown with the folder it would be installed in, not the staging folder.
        let command = &preview.hooks[0].command;
        assert!(
            command.contains("plugins") && !command.contains(".staging"),
            "{command}"
        );
        assert_eq!(preview.servers.len(), 1);
        assert_eq!(preview.skills, ["review"]);
        assert!(preview.installed.is_none() && !preview.sha256.is_empty());
        assert!(!state.paths().data_dir().join(".staging").exists());
        assert!(!dest(&state, "hooked").exists());

        let done = install(&state, install_params("hooked@mk", &preview.sha256))
            .await
            .unwrap();
        assert!(done.withheld);
        assert!(done.reason.as_deref().unwrap().contains("desktop app"));
        let folder = dest(&state, "hooked");
        assert!(folder.join(plugins::ENTRY_FILE).is_file());
        // Recorded as a marketplace install without an approval, with the gate's own hash.
        let record = plugin_records::record_for(state.paths(), &folder).unwrap();
        assert_eq!(record.origin, Origin::Marketplace);
        assert_eq!(record.id.as_deref(), Some("hooked@mk"));
        assert!(record.approval.is_none());
        assert_eq!(record.hash, preview.sha256);
        let seen = plugin_records::look(&folder.canonicalize().unwrap()).unwrap();
        assert_eq!(seen.hash, preview.sha256);
        assert_eq!(gate(&state).withheld.len(), 1);
        let changes = crate::config_changes::list(&state);
        assert_eq!(changes[0].kind, "install");
        assert_eq!(changes[0].extension_kind.as_deref(), Some("plugin"));
        // Inspecting again compares with what is installed.
        let again = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        assert!(
            again
                .installed
                .is_some_and(|installed| installed.same_content)
        );
        assert_nothing_ran(&[home.path(), market.path()]);
    }

    #[tokio::test]
    async fn a_source_that_changed_since_the_inspection_is_refused() {
        let (_home, market, state) = setup().await;
        let preview = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        write(&market.path().join("hooked/extra.txt"), "new");
        let error = install(&state, install_params("hooked@mk", &preview.sha256))
            .await
            .unwrap_err();
        assert!(error.contains("inspect it again"), "{error}");
        assert!(!dest(&state, "hooked").exists());
    }

    #[tokio::test]
    async fn a_taken_name_is_refused_also_when_it_differs_only_by_letter_case() {
        let (_home, _market, state) = setup().await;
        let preview = inspect(&state, inspect_params("plain@mk")).await.unwrap();
        install(&state, install_params("plain@mk", &preview.sha256))
            .await
            .unwrap();
        let error = install(&state, install_params("plain@mk", &preview.sha256))
            .await
            .unwrap_err();
        assert!(error.contains("already installed"), "{error}");
        std::fs::create_dir_all(dest(&state, "Hooked")).unwrap();
        let hooked = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        let error = install(&state, install_params("hooked@mk", &hooked.sha256))
            .await
            .unwrap_err();
        assert!(
            error.contains("collide") || error.contains("already installed"),
            "{error}"
        );
    }

    /// Every file under `home` with its bytes, by path relative to it.
    fn snapshot(home: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        all_files(home)
            .into_iter()
            .map(|file| {
                let name = file
                    .strip_prefix(home)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                (name, std::fs::read(&file).unwrap())
            })
            .collect()
    }

    #[tokio::test]
    async fn a_name_ending_in_a_dot_is_refused_and_the_existing_plugin_is_left_alone() {
        let (home, _market, state) = setup().await;
        let preview = inspect(&state, inspect_params("plain@mk")).await.unwrap();
        install(&state, install_params("plain@mk", &preview.sha256))
            .await
            .unwrap();
        let before = snapshot(home.path());
        // `plain.` names the folder `plain` on Windows: the entry is refused when the marketplace
        // is read, so it is not listed.
        let error = install(&state, install_params("plain.@mk", &preview.sha256))
            .await
            .unwrap_err();
        assert!(error.contains("lists no plugin"), "{error}");
        assert!(inspect(&state, inspect_params("plain.@mk")).await.is_err());
        assert_eq!(snapshot(home.path()), before);
        let record = plugin_records::record_for(state.paths(), &dest(&state, "plain")).unwrap();
        assert_eq!(record.id.as_deref(), Some("plain@mk"));
    }

    #[test]
    fn an_install_folder_that_exists_is_never_merged_into() {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("foo");
        write(&folder.join("keep.txt"), "mine");
        let error = claim(&folder).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            std::fs::read_to_string(folder.join("keep.txt")).unwrap(),
            "mine"
        );
        assert_eq!(all_files(&folder).len(), 1);
        // A free name is claimed as an empty folder.
        let free = home.path().join("bar");
        claim(&free).unwrap();
        assert!(free.is_dir() && all_files(&free).is_empty());
    }

    #[tokio::test]
    async fn an_install_writes_only_its_folder_the_record_store_and_the_journal() {
        let (home, _market, state) = setup().await;
        write(
            &dest(&state, "other/.claude-plugin/plugin.json"),
            r#"{"name": "other"}"#,
        );
        write(
            &dest(&state, "other/skills/x/SKILL.md"),
            "---\nname: x\n---\n",
        );
        let before = snapshot(home.path());
        let preview = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        assert_eq!(
            snapshot(home.path()),
            before,
            "an inspection writes nothing"
        );
        install(&state, install_params("hooked@mk", &preview.sha256))
            .await
            .unwrap();
        let after = snapshot(home.path());
        let allowed = |name: &str| {
            name.starts_with("config/plugins/hooked/")
                || name == "data/plugin-installs.json"
                || name == "data/config-changes.json"
        };
        for (name, bytes) in &before {
            if !allowed(name) {
                assert_eq!(
                    after.get(name),
                    Some(bytes),
                    "{name} changed or was removed"
                );
            }
        }
        for name in after.keys().filter(|name| !before.contains_key(*name)) {
            assert!(allowed(name), "{name} was written");
        }
    }

    #[tokio::test]
    async fn a_non_strict_plugin_installs_and_stays_unchanged_after_a_refresh() {
        let (_home, market, state) = setup().await;
        let preview = inspect(&state, inspect_params("loose@mk")).await.unwrap();
        let done = install(&state, install_params("loose@mk", &preview.sha256))
            .await
            .unwrap();
        assert!(!done.withheld);
        let folder = dest(&state, "loose");
        assert!(gate(&state).allowed.contains(&folder));
        let (found, _) =
            plugins::discover(std::slice::from_ref(&folder), Duration::from_secs(5), &[]);
        assert_eq!(found[0].name, "loose");
        let entry = std::fs::read_to_string(folder.join(plugins::ENTRY_FILE)).unwrap();
        // The marketplace changes and is refreshed; the installed copy does not.
        let listing =
            std::fs::read_to_string(market.path().join(".claude-plugin/marketplace.json"))
                .unwrap()
                .replace("Loose", "Changed");
        write(
            &market.path().join(".claude-plugin/marketplace.json"),
            &listing,
        );
        marketplaces::refresh(&state, PluginsMarketplaceRefreshParams::default())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join(plugins::ENTRY_FILE)).unwrap(),
            entry
        );
        assert_eq!(
            plugin_records::measure(&folder).unwrap().hash,
            preview.sha256
        );
        assert!(gate(&state).allowed.contains(&folder));
    }

    #[tokio::test]
    async fn a_skills_only_plugin_installs_and_loads() {
        let (_home, _market, state) = setup().await;
        let preview = inspect(&state, inspect_params("plain@mk")).await.unwrap();
        assert!(!preview.launcher && preview.hooks.is_empty() && preview.servers.is_empty());
        let done = install(&state, install_params("plain@mk", &preview.sha256))
            .await
            .unwrap();
        assert!(!done.withheld && done.reason.is_none());
        let folder = dest(&state, "plain");
        assert!(gate(&state).allowed.contains(&folder));
        let (found, _) =
            plugins::discover(std::slice::from_ref(&folder), Duration::from_secs(5), &[]);
        let (skills, _) =
            imperium_extensions::skills::discover(&[(found[0].skills_dir(), found[0].origin())]);
        assert_eq!(skills[0].name, "pdf");
    }

    #[tokio::test]
    async fn a_stray_approve_is_ignored_and_noted() {
        let (_home, _market, state) = setup().await;
        let preview = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        let mut params = install_params("hooked@mk", &preview.sha256);
        params.approve = Some(json!(true));
        let done = install(&state, params).await.unwrap();
        assert!(done.withheld);
        assert!(
            done.notes
                .iter()
                .any(|note| note.contains("approve was ignored"))
        );
        let record = plugin_records::record_for(state.paths(), &dest(&state, "hooked")).unwrap();
        assert!(record.approval.is_none());
    }

    #[tokio::test]
    async fn a_manifest_name_unlike_the_entrys_a_link_and_a_revision_are_refused() {
        let (_home, market, state) = setup().await;
        let error = inspect(&state, inspect_params("mismatch@mk"))
            .await
            .unwrap_err();
        assert!(error.contains("manifest names it other"), "{error}");
        let outside = tempfile::tempdir().unwrap();
        assert!(plugin_records::make_link(
            outside.path(),
            &market.path().join("linky").join("skills").join("out")
        ));
        let error = inspect(&state, inspect_params("linky@mk"))
            .await
            .unwrap_err();
        assert!(error.contains("link"), "{error}");
        let mut params = inspect_params("plain@mk");
        params.revision = Some("main".into());
        let error = inspect(&state, params).await.unwrap_err();
        assert!(error.contains("revision"), "{error}");
        for id in ["nothing", "plain@nowhere", "ghost@mk", "@mk"] {
            assert!(inspect(&state, inspect_params(id)).await.is_err(), "{id}");
        }
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

    #[tokio::test]
    async fn a_git_source_is_cloned_without_its_git_folder_and_must_match_a_pinned_commit() {
        let repo = tempfile::tempdir().unwrap();
        write(
            &repo.path().join("skills/s/SKILL.md"),
            "---\nname: s\n---\n",
        );
        git(repo.path(), &["init", "-q", "-b", "main"]);
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "first"]);
        let text = repo.path().display().to_string().replace('\\', "/");
        let url = if text.starts_with('/') {
            format!("file://{text}")
        } else {
            format!("file:///{text}")
        };
        let staging = tempfile::tempdir().unwrap();
        let out = staging.path().join("out");
        let commit = fetch_git(&url, None, None, staging.path(), &out)
            .await
            .unwrap();
        assert_eq!(commit.len(), 40);
        assert!(out.join("skills/s/SKILL.md").is_file() && !out.join(".git").exists());
        let other = tempfile::tempdir().unwrap();
        let wrong = other.path().join("out");
        let error = fetch_git(&url, None, Some("0000000"), other.path(), &wrong)
            .await
            .unwrap_err();
        assert!(error.contains("pins"), "{error}");
        let again = tempfile::tempdir().unwrap();
        let right = again.path().join("out");
        let pinned = fetch_git(&url, Some(&commit), Some(&commit), again.path(), &right)
            .await
            .unwrap();
        assert_eq!(pinned, commit);
        // A pin of the same commit, abbreviated, passes; once the default branch has moved on, a
        // fetch of it is refused against the old pin.
        let abbreviated = tempfile::tempdir().unwrap();
        fetch_git(
            &url,
            Some(&commit),
            Some(&commit[..10]),
            abbreviated.path(),
            &abbreviated.path().join("out"),
        )
        .await
        .unwrap();
        write(&repo.path().join("second.txt"), "2");
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-q", "-m", "second"]);
        let newest = tempfile::tempdir().unwrap();
        let error = fetch_git(
            &url,
            None,
            Some(&commit),
            newest.path(),
            &newest.path().join("out"),
        )
        .await
        .unwrap_err();
        assert!(error.contains("pins"), "{error}");
    }

    /// Inspects and installs `id`, returning the install folder.
    async fn put(state: &DaemonState, id: &str) -> PathBuf {
        let preview = inspect(state, inspect_params(id)).await.unwrap();
        let done = install(state, install_params(id, &preview.sha256))
            .await
            .unwrap();
        PathBuf::from(done.path)
    }

    fn stored(state: &DaemonState) -> String {
        std::fs::read_to_string(state.paths().data_dir().join("plugin-installs.json"))
            .unwrap_or_default()
    }

    fn undo(state: &DaemonState, id: u64) -> Result<String, String> {
        crate::config_changes::undo(state, id).map(|done| done.message)
    }

    fn newest_change(state: &DaemonState) -> u64 {
        crate::config_changes::list(state)[0].id
    }

    fn uninstall_params(name: &str) -> PluginsUninstallParams {
        PluginsUninstallParams {
            name: name.into(),
            delete_secrets: false,
        }
    }

    #[tokio::test]
    async fn uninstall_moves_the_folder_and_record_and_undo_restores_both() {
        let (_home, _market, state) = setup().await;
        let folder = put(&state, "hooked@mk").await;
        let record = plugin_records::record_for(state.paths(), &folder).unwrap();
        let before = list(&state, None).unwrap();
        let shown = before.plugins.iter().find(|p| p.name == "hooked").unwrap();
        assert_eq!(shown.origin.as_deref(), Some("marketplace"));
        assert_eq!(shown.id.as_deref(), Some("hooked@mk"));
        assert!(shown.withheld && shown.reason.is_some() && shown.approval.is_none());

        // Names and ids compare ignoring letter case.
        let done = uninstall(&state, uninstall_params("HOOKED@MK"))
            .await
            .unwrap();
        assert_eq!(done.name, "hooked");
        assert!(!folder.exists());
        assert!(Path::new(&done.kept_at).is_dir());
        assert!(
            !stored(&state).contains("\"name\": \"hooked\""),
            "{}",
            stored(&state)
        );
        assert!(list(&state, None).unwrap().plugins.is_empty());

        // Undo puts back the folder and the same record, and the gate withholds it as before.
        undo(&state, done.change_id).unwrap();
        assert!(folder.join(".claude-plugin/plugin.json").is_file());
        assert_eq!(
            plugin_records::record_for(state.paths(), &folder),
            Some(record)
        );
        assert_eq!(list(&state, None).unwrap().plugins, before.plugins);
        let error = undo(&state, done.change_id).unwrap_err();
        assert!(error.contains("already undone"), "{error}");
    }

    #[tokio::test]
    async fn a_folder_name_and_a_recorded_name_on_different_folders_is_ambiguous() {
        let (_home, _market, state) = setup().await;
        let plugins_dir = state.paths().config_dir().join("plugins");
        // Folder `foo` holds the plugin `legacy`; folder `foo-repo` holds the plugin `foo`.
        write(
            &plugins_dir.join("foo/.claude-plugin/plugin.json"),
            r#"{"name": "legacy"}"#,
        );
        let repo = plugins_dir.join("foo-repo");
        write(
            &repo.join(".claude-plugin/plugin.json"),
            r#"{"name": "foo"}"#,
        );
        let seen = plugin_records::measure(&repo).unwrap();
        plugin_records::add_record(
            state.paths(),
            &repo,
            Record {
                name: "foo".into(),
                origin: Origin::User,
                id: None,
                source: None,
                commit: None,
                hash: seen.hash,
                runs: plugins::runs(&plugin_records::parse(&repo).unwrap()).digest(),
                approval: None,
            },
        )
        .unwrap();
        let error = uninstall(&state, uninstall_params("foo"))
            .await
            .unwrap_err();
        assert!(error.contains("more than one plugin folder"), "{error}");
        assert!(plugins_dir.join("foo").is_dir() && repo.is_dir());
        // The folder name alone still works.
        uninstall(&state, uninstall_params("foo-repo"))
            .await
            .unwrap();
        assert!(!repo.exists() && plugins_dir.join("foo").is_dir());
    }

    #[tokio::test]
    async fn the_result_says_when_the_record_could_not_be_removed() {
        let (_home, _market, state) = setup().await;
        let folder = put(&state, "plain@mk").await;
        std::fs::write(
            state.paths().data_dir().join("plugin-installs.json"),
            "not json",
        )
        .unwrap();
        let done = uninstall(&state, uninstall_params("plain")).await.unwrap();
        assert!(!folder.exists());
        assert!(
            done.notes
                .iter()
                .any(|note| note.contains("record could not be removed")),
            "{:?}",
            done.notes
        );
    }

    #[tokio::test]
    async fn undoing_an_uninstall_refuses_a_folder_that_differs_only_by_letter_case() {
        let (_home, _market, state) = setup().await;
        put(&state, "plain@mk").await;
        let done = uninstall(&state, uninstall_params("plain")).await.unwrap();
        let other = state.paths().config_dir().join("plugins/PLAIN");
        write(&other.join("readme.md"), "x");
        let error = undo(&state, done.change_id).unwrap_err();
        assert!(
            error.contains("letter case") || error.contains("exists again"),
            "{error}"
        );
        assert!(Path::new(&done.kept_at).is_dir());
    }

    #[tokio::test]
    async fn an_uninstall_needs_a_plugin_that_exists() {
        let (_home, _market, state) = setup().await;
        let error = uninstall(&state, uninstall_params("ghost"))
            .await
            .unwrap_err();
        assert!(error.contains("no plugin named ghost"), "{error}");
        let error = uninstall(&state, uninstall_params(" ")).await.unwrap_err();
        assert!(error.contains("name the plugin"), "{error}");
    }

    #[tokio::test]
    async fn undoing_an_install_removes_the_record_too() {
        let (_home, _market, state) = setup().await;
        let folder = put(&state, "plain@mk").await;
        assert!(stored(&state).contains("\"name\": \"plain\""));
        let message = undo(&state, newest_change(&state)).unwrap();
        assert!(message.contains("Removed"), "{message}");
        assert!(!folder.exists());
        assert!(
            !stored(&state).contains("\"name\": \"plain\""),
            "{}",
            stored(&state)
        );
    }

    #[tokio::test]
    async fn undo_leaves_a_folder_alone_that_changed_since_the_install() {
        let (_home, _market, state) = setup().await;
        let folder = put(&state, "plain@mk").await;
        let install_change = newest_change(&state);
        write(
            &folder.join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\nedited\n",
        );
        let error = undo(&state, install_change).unwrap_err();
        assert!(error.contains("changed or replaced"), "{error}");
        assert!(folder.is_dir());
        assert!(stored(&state).contains("\"name\": \"plain\""));
        let listed = crate::config_changes::list(&state);
        assert!(listed[0].undone_at_unix_ms.is_none());
    }

    #[tokio::test]
    async fn undoing_an_uninstall_does_not_restore_over_a_replaced_folder_or_a_changed_copy() {
        let (_home, _market, state) = setup().await;
        let folder = put(&state, "plain@mk").await;
        let done = uninstall(&state, uninstall_params("plain")).await.unwrap();

        // The name was installed again since: the old one is not restored over it.
        let again = put(&state, "plain@mk").await;
        assert_eq!(again, folder);
        let error = undo(&state, done.change_id).unwrap_err();
        assert!(error.contains("exists again"), "{error}");
        assert!(stored(&state).contains("\"name\": \"plain\""));
        let second = uninstall(&state, uninstall_params("plain")).await.unwrap();

        // The kept copy was edited: it is not restored, and no record comes back for it.
        write(
            &Path::new(&done.kept_at).join("skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\nedited\n",
        );
        let error = undo(&state, done.change_id).unwrap_err();
        assert!(error.contains("changed since it was removed"), "{error}");
        assert!(!folder.exists());
        assert!(
            !stored(&state).contains("\"name\": \"plain\""),
            "{}",
            stored(&state)
        );
        // An untouched copy restores.
        undo(&state, second.change_id).unwrap();
        assert!(folder.is_dir());
        assert!(stored(&state).contains("\"name\": \"plain\""));
    }

    #[tokio::test]
    async fn the_list_shows_unrecorded_dev_and_withheld_plugins() {
        let (_home, _market, state) = setup().await;
        let hook =
            r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "check"}]}]}}"#;
        let plugins_dir = state.paths().config_dir().join("plugins");
        let stray = plugins_dir.join("stray");
        write(
            &stray.join(".claude-plugin/plugin.json"),
            r#"{"name": "stray"}"#,
        );
        write(&stray.join("hooks/hooks.json"), hook);
        let devp = plugins_dir.join("devp");
        write(
            &devp.join(".claude-plugin/plugin.json"),
            r#"{"name": "devp"}"#,
        );
        write(&devp.join("hooks/hooks.json"), hook);
        let seen = plugin_records::measure(&devp).unwrap();
        let runs = plugins::runs(&plugin_records::parse(&devp).unwrap()).digest();
        plugin_records::add_record(
            state.paths(),
            &devp,
            Record {
                name: "devp".into(),
                origin: Origin::User,
                id: None,
                source: None,
                commit: None,
                hash: seen.hash.clone(),
                runs: runs.clone(),
                approval: Some(plugin_records::Approval {
                    hash: seen.hash,
                    runs,
                    at_unix_ms: 1,
                    kind: plugin_records::ApprovalKind::Dev,
                }),
            },
        )
        .unwrap();
        write(&plugins_dir.join("empty/readme.md"), "x");
        let listed = list(&state, None).unwrap();
        let find = |name: &str| listed.plugins.iter().find(|p| p.name == name).unwrap();
        let stray = find("stray");
        assert!(stray.withheld && stray.origin.is_none() && stray.approval.is_none());
        assert!(
            stray
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("not installed by Imperium")),
            "{:?}",
            stray.reason
        );
        assert_eq!(find("devp").approval.as_deref(), Some("dev"));
        assert_eq!(find("devp").origin.as_deref(), Some("user"));
        assert!(
            listed
                .problems
                .iter()
                .any(|problem| problem.contains("empty"))
        );
    }

    #[tokio::test]
    async fn mcp_list_carries_the_gate_verdict_id_and_origin() {
        let (_home, _market, state) = setup().await;
        put(&state, "hooked@mk").await;
        put(&state, "plain@mk").await;
        let listed = crate::connectors::list(&state);
        let find = |name: &str| listed.plugins.iter().find(|p| p.name == name).unwrap();
        let hooked = find("hooked");
        assert!(hooked.withheld && hooked.withheld_reason.is_some());
        assert_eq!(hooked.id.as_deref(), Some("hooked@mk"));
        assert_eq!(hooked.origin.as_deref(), Some("marketplace"));
        assert!(!find("plain").withheld);
    }

    #[tokio::test]
    async fn an_agent_install_of_a_plugin_is_recorded_without_an_approval() {
        let home = tempfile::tempdir().unwrap();
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        // The daemon has not run the migration: the install runs it first.
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let source = tempfile::tempdir().unwrap();
        write(
            &source.path().join(".claude-plugin/plugin.json"),
            r#"{"name": "agentic"}"#,
        );
        write(
            &source.path().join("hooks/hooks.json"),
            r#"{"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "check"}]}]}}"#,
        );
        let request = extension_install::InstallRequest {
            kind: ExtensionKind::Plugin,
            source: extension_install::Source::parse(
                &source.path().display().to_string(),
                Path::new("."),
            )
            .unwrap(),
            revision: None,
            subdir: None,
            session_id: Some("s1".into()),
        };
        let (change, _) = crate::config_changes::install(&state, request)
            .await
            .unwrap();
        let folder = state.paths().config_dir().join("plugins/agentic");
        let record = plugin_records::record_for(state.paths(), &folder).unwrap();
        assert_eq!(record.origin, Origin::Agent);
        assert!(record.approval.is_none());
        assert_eq!(change.sha256.as_deref(), Some(record.hash.as_str()));
        let listed = list(&state, None).unwrap();
        let row = listed.plugins.iter().find(|p| p.name == "agentic").unwrap();
        assert!(row.withheld);
        assert_eq!(row.origin.as_deref(), Some("agent"));
        // Undo removes the folder and the record.
        undo(&state, change.id).unwrap();
        assert!(!folder.exists());
        assert!(
            !stored(&state).contains("\"name\": \"agentic\""),
            "{}",
            stored(&state)
        );
    }

    /// What `plugins.update` is asked for after inspecting `id` now.
    async fn update_for(
        state: &DaemonState,
        id: &str,
        name: &str,
    ) -> Result<PluginsUpdateResult, String> {
        let preview = inspect(state, inspect_params(id)).await.unwrap();
        update(
            state,
            PluginsUpdateParams {
                name: name.into(),
                sha256: preview.sha256,
            },
        )
        .await
    }

    #[tokio::test]
    async fn an_update_with_the_same_content_is_up_to_date() {
        let (_home, _market, state) = setup().await;
        let folder = put(&state, "plain@mk").await;
        let done = update_for(&state, "plain@mk", "plain").await.unwrap();
        assert!(done.up_to_date && !done.replaced && done.kept_at.is_none());
        assert!(folder.is_dir());
    }

    #[tokio::test]
    async fn an_update_refuses_another_marketplace_and_content_that_was_not_inspected() {
        let (_home, _market, state) = setup().await;
        put(&state, "plain@mk").await;
        let preview = inspect(&state, inspect_params("plain@mk")).await.unwrap();
        let error = update(
            &state,
            PluginsUpdateParams {
                name: "plain@other".into(),
                sha256: preview.sha256.clone(),
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("never moves a plugin"), "{error}");
        let error = update(
            &state,
            PluginsUpdateParams {
                name: "plain".into(),
                sha256: "00".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("not what was inspected"), "{error}");
        // A plugin nobody installed from a marketplace is not updated.
        let manual = state.paths().config_dir().join("plugins/manual");
        write(
            &manual.join(".claude-plugin/plugin.json"),
            r#"{"name": "manual"}"#,
        );
        let error = update(
            &state,
            PluginsUpdateParams {
                name: "manual".into(),
                sha256: preview.sha256,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("no record") || error.contains("not installed from a marketplace"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_update_of_an_approved_plugin_is_withheld_until_it_is_approved_again() {
        let (_home, market, state) = setup().await;
        let folder = put(&state, "hooked@mk").await;
        let seen = plugin_records::measure(&folder).unwrap();
        let runs = plugins::runs(&plugin_records::parse(&folder).unwrap()).digest();
        plugin_records::record_approval(
            state.paths(),
            &plugin_records::TestVerifier,
            &folder,
            plugin_records::ApprovalKind::Counted,
            &seen.hash,
            &runs,
        )
        .unwrap();
        assert!(withheld_reason(&state, &folder).is_none());

        // The marketplace's copy changes in a skill only: the new content is withheld anyway,
        // the old approval does not carry over, and the old folder is kept.
        write(
            &market.path().join("hooked/skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review\n---\nedited\n",
        );
        let done = update_for(&state, "hooked@mk", "Hooked").await.unwrap();
        assert!(
            done.replaced && !done.up_to_date && done.withheld,
            "{done:?}"
        );
        let kept = PathBuf::from(done.kept_at.unwrap());
        assert!(kept.join("skills/review/SKILL.md").is_file());
        assert!(
            std::fs::read_to_string(kept.join("skills/review/SKILL.md"))
                .unwrap()
                .contains("body")
        );
        let record = plugin_records::record_for(state.paths(), &folder).unwrap();
        assert!(record.approval.is_none());
        assert_eq!(record.origin, Origin::Marketplace);
        assert_eq!(record.id.as_deref(), Some("hooked@mk"));
        assert!(
            std::fs::read_to_string(folder.join("skills/review/SKILL.md"))
                .unwrap()
                .contains("edited")
        );
        let listed = list(&state, None).unwrap();
        let row = listed.plugins.iter().find(|p| p.name == "hooked").unwrap();
        assert!(row.withheld && row.approval.is_none());
        assert_nothing_ran(&[state.paths().config_dir(), state.paths().data_dir()]);
    }

    #[tokio::test]
    async fn an_update_of_a_plugin_without_code_loads() {
        let (_home, market, state) = setup().await;
        let folder = put(&state, "plain@mk").await;
        write(
            &market.path().join("plain/skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\nnewer\n",
        );
        let done = update_for(&state, "plain@mk", "plain@MK").await.unwrap();
        assert!(done.replaced && !done.withheld);
        assert!(
            std::fs::read_to_string(folder.join("skills/pdf/SKILL.md"))
                .unwrap()
                .contains("newer")
        );
    }

    fn secret_params(name: &str, variable: &str, value: &str) -> PluginsSetSecretParams {
        PluginsSetSecretParams {
            name: name.into(),
            variable: variable.into(),
            value: value.into(),
        }
    }

    /// Every file under `dirs` that holds `needle`.
    fn files_holding(dirs: &[&Path], needle: &str) -> Vec<PathBuf> {
        dirs.iter()
            .flat_map(|dir| all_files(dir))
            .filter(|file| {
                std::fs::read(file)
                    .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
            })
            .collect()
    }

    #[tokio::test]
    async fn a_saved_secret_reaches_the_secret_store_and_nowhere_else() {
        let (home, _market, state) = setup().await;
        put(&state, "keyed@mk").await;
        let value = "tok-9f3a-very-secret-value";
        let row = |state: &DaemonState| {
            list(state, None)
                .unwrap()
                .plugins
                .into_iter()
                .find(|plugin| plugin.name == "keyed")
                .unwrap()
        };
        let before = row(&state);
        assert_eq!(before.required_secrets.len(), 4);
        assert_eq!(before.missing_secrets, before.required_secrets);

        // A variable no server names, a name too long, and a second spelling are refused, and an
        // error never holds the value.
        for variable in ["OTHER".to_string(), "A".repeat(52), "foo".to_string()] {
            let error = set_secret(&state, secret_params("keyed", &variable, value)).unwrap_err();
            assert!(!error.contains(value), "{error}");
        }
        let long = set_secret(&state, secret_params("keyed", &"A".repeat(52), value)).unwrap_err();
        assert!(long.contains("cannot be a secret"), "{long}");
        let second = set_secret(&state, secret_params("keyed", "foo", value)).unwrap_err();
        assert!(second.contains("one saved secret"), "{second}");

        let done = set_secret(&state, secret_params("KEYED", "API_KEY", value)).unwrap();
        assert_eq!(done.secret, "plugin.keyed.api_key");
        assert!(!format!("{done:?}").contains(value));
        set_secret(&state, secret_params("keyed", "FOO", value)).unwrap();
        // It is in the store, and in no other file: not the config, the plugin folder, the
        // record store or the journal.
        let name = secret_name("keyed", "API_KEY").unwrap();
        assert_eq!(state.secrets().load(&name).unwrap().as_deref(), Some(value));
        let holding = files_holding(&[home.path()], value);
        assert!(holding.is_empty(), "{holding:?}");
        let after = row(&state);
        assert!(!after.missing_secrets.contains(&"API_KEY".to_string()));
        assert!(!after.missing_secrets.contains(&"FOO".to_string()));
        assert!(!format!("{after:?}").contains(value));

        // Delete it again.
        let gone = delete_secret(
            &state,
            PluginsDeleteSecretParams {
                name: "keyed".into(),
                variable: "API_KEY".into(),
            },
        )
        .unwrap();
        assert!(gone.changed);
        assert!(state.secrets().load(&name).unwrap().is_none());
    }

    #[tokio::test]
    async fn uninstalling_can_delete_the_saved_secrets_and_only_a_downloaded_plugin_has_them() {
        let (_home, _market, state) = setup().await;
        put(&state, "keyed@mk").await;
        set_secret(&state, secret_params("keyed", "API_KEY", "v1-secret")).unwrap();
        let name = secret_name("keyed", "API_KEY").unwrap();
        let done = uninstall(
            &state,
            PluginsUninstallParams {
                name: "keyed".into(),
                delete_secrets: true,
            },
        )
        .await
        .unwrap();
        assert!(state.secrets().load(&name).unwrap().is_none());
        assert!(done.notes.iter().any(|note| note.contains("deleted 1")));

        // Without the option the secret stays.
        put(&state, "keyed@mk").await;
        set_secret(&state, secret_params("keyed", "API_KEY", "v2-secret")).unwrap();
        uninstall(&state, uninstall_params("keyed")).await.unwrap();
        assert!(state.secrets().load(&name).unwrap().is_some());

        // A folder nobody installed does not read saved secrets.
        let manual = state.paths().config_dir().join("plugins/manual");
        write(
            &manual.join(".claude-plugin/plugin.json"),
            r#"{"name": "manual"}"#,
        );
        write(
            &manual.join(".mcp.json"),
            r#"{"mcpServers": {"s": {"command": "x", "env": {"K": "${KEY}"}}}}"#,
        );
        let error = set_secret(&state, secret_params("manual", "KEY", "v")).unwrap_err();
        assert!(error.contains("marketplace or by an agent"), "{error}");
    }

    #[test]
    fn a_variable_with_a_dot_cannot_read_another_plugins_secret() {
        let store: std::sync::Arc<dyn imperium_config::SecretStore> =
            std::sync::Arc::new(imperium_config::MemorySecrets::new());
        // Plugin `a.b`'s secret `x` is `plugin.a.b.x`, which plugin `a`'s `${b.x}` would be.
        store
            .store(&secret_name("a.b", "x").unwrap(), "other-plugins-value")
            .unwrap();
        let lookup = plugin_secrets(store);
        let read = |plugin: &str, variable: &str| lookup.lookup_for_test(plugin, variable);
        assert_eq!(read("a.b", "x").as_deref(), Some("other-plugins-value"));
        assert_eq!(read("a", "b.x"), None);
        assert!(secret_name("a", "b.x").is_err());
        for bad in ["", "1abc", "a-b", "a b", "a.b"] {
            assert!(secret_name("a", bad).is_err(), "{bad}");
        }
        assert!(secret_name("a", "_ok1").is_ok());
    }
}
