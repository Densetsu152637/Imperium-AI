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
    InstallSource, PluginHookRun, PluginInstalled, PluginPreview, PluginServerRun,
    PluginsInspectParams, PluginsInstallParams, PluginsInstallResult,
};

use crate::extension_install::{self, Staging, check_name, move_tree, remove_tree};
use crate::marketplaces::{self, Located};
use crate::plugin_records::{self, Origin, Record};
use crate::state::DaemonState;

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

fn preview(state: &DaemonState, staged: &Staged, installed_at: &Path) -> PluginPreview {
    let plugin = &staged.plugin;
    let runs = plugins::runs(plugin);
    // The summary expands `${CLAUDE_PLUGIN_ROOT}` to the staging folder; show where it will be.
    let staging_root = plugin.root.display().to_string();
    let there = installed_at.display().to_string();
    let show = |text: &str| text.replace(&staging_root, &there);
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
        sha256: staged.hash.clone(),
        runs_digest: runs.digest(),
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

/// Creates the install folder exclusively: anything already there (a folder that appeared since
/// the name check, or one that a name like `foo.` maps to on Windows) is an error, so nothing is
/// ever moved into, merged with or written over an existing folder.
fn claim(dest: &Path) -> Result<(), String> {
    std::fs::create_dir(dest).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{} already exists: it is not installed over", dest.display())
        } else {
            format!("{}: {error}", dest.display())
        }
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
    let source = InstallSource {
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
    };
    let id = format!("{name}@{}", staged.located.marketplace);
    let recorded = extension_install::write_source(&dest, &source).and_then(|()| {
        plugin_records::add_record(
            state.paths(),
            &dest,
            Record {
                name: name.clone(),
                origin: Origin::Marketplace,
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
    crate::config_changes::plugin_installed(state, &id, &dest, &source);
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
    let gate = plugin_records::gate(state.paths(), plugin_records::production_verifier());
    let reason = gate
        .withheld
        .iter()
        .find(|withheld| withheld.folder == dest)
        .map(|withheld| {
            format!(
                "{}. It loads after you approve it in the Imperium desktop app",
                withheld.reason
            )
        });
    Ok(PluginsInstallResult {
        name,
        path: dest.display().to_string(),
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
    }

    /// A state in a fresh home that has run the migration, and a folder marketplace added to it.
    async fn setup() -> (tempfile::TempDir, tempfile::TempDir, Arc<DaemonState>) {
        let home = tempfile::tempdir().unwrap();
        let market = tempfile::tempdir().unwrap();
        write_market(market.path());
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
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
                    .replace('\', "/");
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
            "---
name: x
---
",
        );
        let before = snapshot(home.path());
        let preview = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        assert_eq!(snapshot(home.path()), before, "an inspection writes nothing");
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
                assert_eq!(after.get(name), Some(bytes), "{name} changed or was removed");
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
}
