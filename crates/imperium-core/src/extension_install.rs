//! Installing and removing skills, plugins, and UI mods in the user's config folder, as agents
//! do with `install_extension` and `uninstall_extension` (roadmap item 22).
//!
//! An install fetches the source (a `git clone` of a URL, or a copy of a local folder) into a
//! staging folder beside the target, checks that it is the kind it claims to be, writes where it
//! came from to `.imperium-source.json` inside it, and moves it into place. Nothing in it runs:
//! no install script, no build, no hook. Git clones no submodules, skips Git LFS downloads, and
//! refuses the `ext::` transport; copies skip symbolic links and `.git`. Skills and plugins are
//! then listed for agents and load on first use; a plugin with hooks or MCP servers is added to
//! `extensions.disabled_plugins` so neither its hooks nor its servers (which `activate_plugin`
//! would start unasked) run until the user turns it on in Connectors, and a mod installed by an
//! agent shows only after the user turns it on in the desktop app. Whatever the user turns on,
//! `${VAR}` in an agent-installed plugin's `.mcp.json` never reads the daemon's environment
//! (see `imperium_extensions::plugins`).
//!
//! An uninstall moves the folder aside to `<data>/config-trash/`, so undoing it puts it back.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use imperium_protocol::methods::InstallSource;

use crate::state::DaemonState;

/// The provenance file written into each folder an agent installs.
pub(crate) const SOURCE_FILE: &str = imperium_extensions::plugins::SOURCE_FILE;
/// Longest a `git clone` may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(300);
/// Largest extension copied into the config folder.
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// What is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtensionKind {
    Skill,
    Plugin,
    Mod,
}

impl ExtensionKind {
    pub(crate) fn parse(text: &str) -> Result<ExtensionKind, String> {
        match text.trim() {
            "skill" => Ok(ExtensionKind::Skill),
            "plugin" => Ok(ExtensionKind::Plugin),
            "mod" => Ok(ExtensionKind::Mod),
            other => Err(format!("kind must be skill, plugin, or mod, not {other:?}")),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ExtensionKind::Skill => "skill",
            ExtensionKind::Plugin => "plugin",
            ExtensionKind::Mod => "mod",
        }
    }

    /// The folder of the config folder it is installed in.
    fn folder(self) -> &'static str {
        match self {
            ExtensionKind::Skill => "skills",
            ExtensionKind::Plugin => "plugins",
            ExtensionKind::Mod => "mods",
        }
    }

    /// Where extensions of this kind are installed.
    pub(crate) fn dir(self, config_dir: &Path) -> PathBuf {
        config_dir.join(self.folder())
    }
}

/// Where an install comes from, as the agent gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    Git(String),
    Path(PathBuf),
}

impl Source {
    /// Reads `source`: a git URL, or a folder (relative ones against `base`). Anything else is
    /// refused, since Imperium has no extension registry yet.
    pub(crate) fn parse(source: &str, base: &Path) -> Result<Source, String> {
        let source = source.trim();
        if source.is_empty() {
            return Err("source is empty".into());
        }
        if source.starts_with('-') {
            return Err("source must not start with -".into());
        }
        let lower = source.to_ascii_lowercase();
        if lower.starts_with("ext::") || lower.contains("::") {
            return Err("that git transport is not allowed".into());
        }
        if ["https://", "http://", "ssh://", "git://", "file://"]
            .iter()
            .any(|scheme| lower.starts_with(scheme))
            || is_scp_like(source)
        {
            return Ok(Source::Git(source.to_string()));
        }
        if lower.contains("://") {
            return Err(format!("unsupported URL scheme in {source}"));
        }
        let path = match source.strip_prefix("~") {
            Some(rest) => home_dir()
                .ok_or("no home folder to expand ~ against")?
                .join(rest.trim_start_matches(['/', '\\'])),
            None => base.join(source),
        };
        if path.is_dir() {
            return Ok(Source::Path(path));
        }
        Err(format!(
            "{source} is neither a git URL nor a folder on this machine (Imperium has no \
             extension registry yet; give a git URL or a local folder)"
        ))
    }
}

/// `user@host:path` (git's scp-like syntax), but not a Windows drive (`C:\...`).
fn is_scp_like(source: &str) -> bool {
    let Some((host, path)) = source.split_once(':') else {
        return false;
    };
    host.contains('@') && !host.contains(['/', '\\']) && !path.is_empty()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// What an install asked for.
#[derive(Debug, Clone)]
pub(crate) struct InstallRequest {
    pub kind: ExtensionKind,
    pub source: Source,
    /// A branch, tag, or commit (git only).
    pub revision: Option<String>,
    /// A folder inside the source.
    pub subdir: Option<String>,
    pub session_id: Option<String>,
}

/// What was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Installed {
    pub kind: ExtensionKind,
    pub name: String,
    pub path: PathBuf,
    pub source: InstallSource,
    /// A plugin with hooks or MCP servers, added to `extensions.disabled_plugins`.
    pub disabled: bool,
    /// What the agent and the user are told about it.
    pub notes: Vec<String>,
}

pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Installs `request` into the config folder.
pub(crate) async fn install(
    state: &DaemonState,
    request: InstallRequest,
) -> Result<Installed, String> {
    let config_dir = state.paths().config_dir().to_path_buf();
    let subdir = request
        .subdir
        .as_deref()
        .map(str::trim)
        .filter(|subdir| !subdir.is_empty() && *subdir != ".")
        .map(check_subdir)
        .transpose()?;
    if request.revision.is_some() && matches!(request.source, Source::Path(_)) {
        return Err("revision applies to a git source only".into());
    }
    let staging_root = config_dir.join(".staging");
    let staging = staging_root.join(format!("install-{}-{}", std::process::id(), unix_ms()));
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("could not create {}: {error}", staging.display()))?;
    let result = install_in(state, &config_dir, &staging, &request, subdir.as_deref()).await;
    remove_tree(&staging);
    // Leaves no empty staging folder behind.
    let _ = std::fs::remove_dir(&staging_root);
    result
}

async fn install_in(
    state: &DaemonState,
    config_dir: &Path,
    staging: &Path,
    request: &InstallRequest,
    subdir: Option<&str>,
) -> Result<Installed, String> {
    let fetched = staging.join("fetched");
    let (location, revision) = match &request.source {
        Source::Git(url) => {
            let commit = git_fetch(url, request.revision.as_deref(), &fetched).await?;
            (url.clone(), Some(commit))
        }
        Source::Path(path) => {
            let path = path
                .canonicalize()
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if path.starts_with(
                config_dir
                    .canonicalize()
                    .unwrap_or(config_dir.to_path_buf()),
            ) {
                return Err("the folder is already inside Imperium's config folder".into());
            }
            (display_path(&path), None)
        }
    };
    let top = match &request.source {
        Source::Git(_) => fetched.clone(),
        Source::Path(path) => path.clone(),
    };
    let root = match subdir {
        Some(subdir) => top.join(subdir),
        None => top.clone(),
    };
    if !root.is_dir() {
        return Err(format!(
            "{} has no folder {}",
            location,
            subdir.unwrap_or(".")
        ));
    }
    let out = staging.join("out");
    let mut budget = MAX_BYTES;
    copy_tree(&root, &out, &mut budget)?;
    let fallback = default_name(&request.source, subdir);
    let (name, mut notes, Runs { hooks, servers }) = inspect(request.kind, &out, &fallback)?;
    let dest = request.kind.dir(config_dir).join(&name);
    if dest.exists() {
        return Err(format!(
            "a {} named {name} is already installed at {}; uninstall it first",
            request.kind.as_str(),
            dest.display()
        ));
    }
    let source = InstallSource {
        kind: match request.source {
            Source::Git(_) => "git",
            Source::Path(_) => "path",
        }
        .into(),
        location,
        revision,
        requested: request.revision.clone(),
        subdir: subdir.map(str::to_string),
        installed_at_unix_ms: unix_ms(),
        session_id: request.session_id.clone(),
    };
    write_source(&out, &source)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    move_tree(&out, &dest)?;
    let mut disabled = false;
    if request.kind == ExtensionKind::Plugin && (hooks > 0 || servers > 0) {
        let has = match (hooks, servers) {
            (0, servers) => format!("{servers} MCP server(s), which would run its code"),
            (hooks, 0) => format!("{hooks} hook(s), which would run on every matching event"),
            (hooks, servers) => {
                format!("{hooks} hook(s) and {servers} MCP server(s), which would run its code")
            }
        };
        match set_plugin_disabled(state, &name, true) {
            Ok(()) => {
                disabled = true;
                notes.push(format!(
                    "It has {has}, so it is installed turned off and activate_plugin cannot \
                     start it; only the user can turn it on, in Connectors."
                ));
            }
            Err(error) => {
                // Without the setting its hooks or servers could run unasked: take it out again.
                remove_tree(&dest);
                return Err(format!(
                    "the plugin has hooks or servers and could not be turned off ({error}), so \
                     it was not installed"
                ));
            }
        }
    }
    match request.kind {
        ExtensionKind::Skill => notes.push(format!(
            "Agents see it in the skill index of new sessions and load it with load_skill {name}."
        )),
        ExtensionKind::Plugin if !disabled => notes
            .push("New sessions list its skills and commands; it has no hooks or servers.".into()),
        ExtensionKind::Mod => {
            notes.push("Its panels show only after the user turns it on in Settings → Mods.".into())
        }
        ExtensionKind::Plugin => {}
    }
    Ok(Installed {
        kind: request.kind,
        name,
        path: dest,
        source,
        disabled,
        notes,
    })
}

/// A relative folder inside the source, without `..` or a root.
fn check_subdir(subdir: &str) -> Result<String, String> {
    let path = Path::new(subdir);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("path {subdir} must be a folder inside the source"));
    }
    Ok(subdir.replace('\\', "/").trim_end_matches('/').to_string())
}

/// What a plugin would run once enabled: the hooks and MCP servers it declares.
#[derive(Debug, Default, Clone, Copy)]
struct Runs {
    hooks: usize,
    servers: usize,
}

/// Checks that `dir` (a copy of `original`) is a `kind`, and returns its name, notes for the
/// user, and what a plugin would run.
fn inspect(
    kind: ExtensionKind,
    dir: &Path,
    fallback: &str,
) -> Result<(String, Vec<String>, Runs), String> {
    match kind {
        ExtensionKind::Skill => {
            let text = std::fs::read_to_string(dir.join("SKILL.md")).map_err(
                |_| "no SKILL.md at the top of the source (use path to pick its folder)",
            )?;
            let (fields, _) = imperium_api::markdown::front_matter(&text);
            if fields
                .get("description")
                .is_none_or(|d| d.trim().is_empty())
            {
                return Err("SKILL.md has no description in its front matter".into());
            }
            let name = fields
                .get("name")
                .cloned()
                .unwrap_or_else(|| fallback.to_string());
            Ok((check_name(&name)?, Vec::new(), Runs::default()))
        }
        ExtensionKind::Plugin => {
            if !dir.join(".claude-plugin").join("plugin.json").is_file() {
                return Err(
                    "no .claude-plugin/plugin.json at the top of the source (use path to pick \
                     its folder)"
                        .into(),
                );
            }
            let (plugins, problems) = imperium_extensions::plugins::discover(
                &[dir.to_path_buf()],
                Duration::from_secs(60),
            );
            let plugin = plugins
                .into_iter()
                .next()
                .ok_or_else(|| format!("the plugin does not load: {}", problems.join("; ")))?;
            let mut notes = Vec::new();
            if !problems.is_empty() {
                notes.push(format!("Problems: {}", problems.join("; ")));
            }
            Ok((
                check_name(&plugin.name)?,
                notes,
                Runs {
                    hooks: plugin.hooks.len(),
                    servers: plugin.servers.len(),
                },
            ))
        }
        ExtensionKind::Mod => {
            if !dir.join("mod.json").is_file() {
                return Err(
                    "no mod.json at the top of the source (use path to pick its folder)".into(),
                );
            }
            let info = crate::mods::load(dir, false)?;
            Ok((check_name(&info.id)?, Vec::new(), Runs::default()))
        }
    }
}

/// The name of a skill without one in its front matter: the last folder of `subdir`, else of
/// the source (a URL's last segment without `.git`).
fn default_name(source: &Source, subdir: Option<&str>) -> String {
    let last = |text: &str| {
        text.trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\', ':'])
            .next()
            .unwrap_or_default()
            .trim_end_matches(".git")
            .to_string()
    };
    match (subdir, source) {
        (Some(subdir), _) => last(subdir),
        (None, Source::Git(url)) => last(url),
        (None, Source::Path(path)) => last(&path.display().to_string()),
    }
}

/// A name that is safe as a folder name: letters, digits, `.`, `_`, `-`, not starting with a dot.
fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(name.to_string())
    } else {
        Err(format!(
            "the name {name:?} cannot be installed: use 1-64 letters, digits, '.', '_' or '-'"
        ))
    }
}

fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    // `canonicalize` gives `\\?\C:\...` on Windows.
    text.strip_prefix(r"\\?\")
        .map_or(text.clone(), str::to_string)
}

/// Clones `url` into `dest` and returns the commit checked out. A shallow clone of the default
/// branch, or of `revision` when it names a branch or tag; a full clone for a commit.
async fn git_fetch(url: &str, revision: Option<&str>, dest: &Path) -> Result<String, String> {
    if let Some(revision) = revision
        && (revision.starts_with('-') || revision.trim().is_empty())
    {
        return Err(format!("revision {revision:?} is not valid"));
    }
    let dest_text = dest.display().to_string();
    let mut args: Vec<&str> = vec!["clone", "--no-recurse-submodules", "--quiet"];
    let commit = revision.filter(|r| r.len() >= 7 && r.chars().all(|c| c.is_ascii_hexdigit()));
    match (revision, commit) {
        (Some(_), Some(_)) => args.push("--no-checkout"),
        (Some(branch), None) => args.extend(["--depth", "1", "--branch", branch]),
        (None, _) => args.extend(["--depth", "1"]),
    }
    args.extend(["--", url, &dest_text]);
    git(None, &args).await?;
    if let Some(commit) = commit {
        git(Some(dest), &["checkout", "--quiet", "--detach", commit]).await?;
    }
    let head = git(Some(dest), &["rev-parse", "HEAD"]).await?;
    Ok(head.trim().to_string())
}

/// Runs git without prompts, LFS downloads, or the `ext::` transport; returns its stdout.
async fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "core.symlinks=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = tokio::time::timeout(GIT_TIMEOUT, command.output())
        .await
        .map_err(|_| "git took longer than 5 minutes".to_string())?
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args[0], stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Copies `from` into `to`, leaving out `.git` and symbolic links, within `budget` bytes.
fn copy_tree(from: &Path, to: &Path, budget: &mut u64) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    let entries =
        std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == ".git" || name == SOURCE_FILE {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let target = to.join(&name);
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, budget)?;
        } else if kind.is_file() {
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            *budget = budget
                .checked_sub(size)
                .ok_or("the extension is larger than 256 MiB")?;
            std::fs::copy(entry.path(), &target)
                .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Moves a folder, copying it when a rename cannot (another drive).
pub(crate) fn move_tree(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_all(from, to)?;
    remove_tree(from);
    Ok(())
}

/// Copies everything (`.git` and the source file included), for moving a folder.
fn copy_all(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    let entries =
        std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?;
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            copy_all(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)
                .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Removes a folder, clearing read-only flags (git's object files are read-only on Windows).
pub(crate) fn remove_tree(dir: &Path) {
    if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
        return;
    }
    clear_readonly(dir);
    if let Err(error) = std::fs::remove_dir_all(dir) {
        tracing::warn!(path = %dir.display(), %error, "could not remove a folder");
    }
}

fn clear_readonly(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            clear_readonly(&path);
        } else if let Ok(meta) = entry.metadata() {
            let mut permissions = meta.permissions();
            if permissions.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(&path, permissions);
            }
        }
    }
}

fn write_source(dir: &Path, source: &InstallSource) -> Result<(), String> {
    let text = serde_json::to_string_pretty(source).map_err(|error| error.to_string())?;
    std::fs::write(dir.join(SOURCE_FILE), text)
        .map_err(|error| format!("could not record the source: {error}"))
}

/// Where the extension in `dir` came from, when an agent installed it.
pub(crate) fn read_source(dir: &Path) -> Option<InstallSource> {
    let text = std::fs::read_to_string(dir.join(SOURCE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Adds `name` to (or removes it from) `extensions.disabled_plugins` in the user config and in
/// the running daemon's settings, so new sessions follow at once.
pub(crate) fn set_plugin_disabled(
    state: &DaemonState,
    name: &str,
    disabled: bool,
) -> Result<(), String> {
    let saved = crate::connectors::saved_settings(state.paths())
        .unwrap_or_else(|| (*state.settings()).clone());
    let mut list = saved.extensions.disabled_plugins.clone();
    let present = list.iter().any(|entry| entry == name);
    if present == disabled {
        apply_disabled(state, name, disabled);
        return Ok(());
    }
    if disabled {
        list.push(name.to_string());
    } else {
        list.retain(|entry| entry != name);
    }
    imperium_config::set_user_setting(
        state.paths(),
        "extensions.disabled_plugins",
        &serde_json::json!(list),
        std::env::vars(),
    )
    .map_err(|error| error.to_string())?;
    apply_disabled(state, name, disabled);
    Ok(())
}

fn apply_disabled(state: &DaemonState, name: &str, disabled: bool) {
    let mut next = (*state.settings()).clone();
    let list = &mut next.extensions.disabled_plugins;
    let present = list.iter().any(|entry| entry == name);
    if disabled && !present {
        list.push(name.to_string());
    } else if !disabled && present {
        list.retain(|entry| entry != name);
    } else {
        return;
    }
    state.replace_settings(next);
}

/// The folder of the installed `kind` called `name` in the config folder: the folder of that
/// name, else the one whose manifest gives that name.
pub(crate) fn find_installed(
    config_dir: &Path,
    kind: ExtensionKind,
    name: &str,
) -> Result<PathBuf, String> {
    let name = name.trim();
    let dir = kind.dir(config_dir);
    if check_name(name).is_ok() {
        let direct = dir.join(name);
        if marker(kind, &direct) {
            return Ok(direct);
        }
    }
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if !marker(kind, &path) {
            continue;
        }
        let found = match kind {
            ExtensionKind::Skill => std::fs::read_to_string(path.join("SKILL.md"))
                .ok()
                .and_then(|text| {
                    imperium_api::markdown::front_matter(&text)
                        .0
                        .get("name")
                        .cloned()
                }),
            ExtensionKind::Plugin => imperium_extensions::plugins::discover(
                std::slice::from_ref(&path),
                Duration::from_secs(60),
            )
            .0
            .into_iter()
            .next()
            .map(|plugin| plugin.name),
            ExtensionKind::Mod => crate::mods::load(&path, false).ok().map(|info| info.id),
        };
        if found.as_deref() == Some(name) {
            return Ok(path);
        }
    }
    Err(format!(
        "no {} named {name} is installed in {}",
        kind.as_str(),
        dir.display()
    ))
}

fn marker(kind: ExtensionKind, dir: &Path) -> bool {
    match kind {
        ExtensionKind::Skill => dir.join("SKILL.md").is_file(),
        ExtensionKind::Plugin => dir.join(".claude-plugin").join("plugin.json").is_file(),
        ExtensionKind::Mod => dir.join("mod.json").is_file(),
    }
}

/// Where uninstalled extensions are kept so an undo can restore them.
pub(crate) fn trash_dir(state: &DaemonState) -> PathBuf {
    state.paths().data_dir().join("config-trash")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_git_urls_or_folders() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir(base.path().join("local")).unwrap();
        for url in [
            "https://github.com/a/b.git",
            "ssh://git@host/a/b",
            "git@github.com:a/b.git",
            "file:///tmp/repo",
        ] {
            assert_eq!(
                Source::parse(url, base.path()).unwrap(),
                Source::Git(url.into())
            );
        }
        assert_eq!(
            Source::parse("local", base.path()).unwrap(),
            Source::Path(base.path().join("local"))
        );
        for bad in ["ext::sh -c x", "-u", "some-registry-name", "ftp://x/y", ""] {
            assert!(Source::parse(bad, base.path()).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_and_subfolders_are_checked() {
        assert!(check_name("pdf-tools_1.0").is_ok());
        for bad in ["", ".hidden", "a/b", "a\\b", "..", "x y"] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        assert_eq!(check_subdir("skills/pdf/").unwrap(), "skills/pdf");
        assert!(check_subdir("../x").is_err());
        assert!(check_subdir("/etc").is_err());
    }
}
