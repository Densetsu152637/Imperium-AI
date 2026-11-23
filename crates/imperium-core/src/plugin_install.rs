//! `plugins.inspect`: fetching one plugin of a marketplace and showing what it would run.
//!
//! Inspecting copies the plugin (out of the marketplace's own clone, or from its own git
//! repository) into a staging folder under `<data>/.staging`, writes
//! `.claude-plugin/marketplace-entry.json` there, parses it and hashes it. It runs no hook, server,
//! build step, install script or git hook: files are read and copied, nothing else.
//!
//! The content hash is the load-time gate's (`plugin_records::measure`): the files' paths, lengths
//! and bytes, without a top-level `.git` and `.imperium-source.json`, so the folder hashes the same
//! once installed. The copy refuses a link anywhere in the source (git writes a link as a plain
//! file under `core.symlinks=false`, so a git checkout has none on disk).

use std::path::{Path, PathBuf};
use std::time::Duration;

use imperium_extensions::marketplace::Source as PluginSource;
use imperium_extensions::plugins::{self, Plugin, inside};
use imperium_protocol::methods::{
    PluginHookRun, PluginInstalled, PluginPreview, PluginServerRun, PluginsInspectParams,
};

use crate::extension_install::{self, Staging};
use crate::marketplaces::{self, Located};
use crate::plugin_records;
use crate::state::DaemonState;

const MAX_BYTES: u64 = 256 * 1024 * 1024;

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
    let (source_kind, source, commit) = match &entry_source {
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
            ("path", rel.clone(), located.commit.clone())
        }
        PluginSource::Github { .. } | PluginSource::Url { .. } => {
            drop(guard);
            let url = entry_source
                .clone_url()
                .ok_or("the plugin has no git source")?;
            let pinned = revision.is_none().then(|| entry_source.sha()).flatten();
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
            (kind, url, Some(commit))
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

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::Arc;

    use imperium_config::{Config, Layer, Paths};
    use imperium_protocol::methods::PluginsMarketplaceAddParams;
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

    #[tokio::test]
    async fn a_plugin_with_code_is_inspected_and_nothing_runs_or_is_installed() {
        let (home, market, state) = setup().await;
        let preview = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        assert_eq!(preview.id, "hooked@mk");
        assert_eq!(preview.marketplace, "mk");
        assert_eq!(preview.source_kind, "path");
        assert_eq!(preview.version.as_deref(), Some("1.0.0"));
        assert_eq!(preview.hooks.len(), 1);
        // Shown with the folder it would be installed in, not the staging folder.
        let command = &preview.hooks[0].command;
        assert!(
            command.contains("plugins") && !command.contains(".staging"),
            "{command}"
        );
        assert_eq!(preview.servers.len(), 1);
        assert_eq!(preview.servers[0].command.as_deref(), Some("touch"));
        assert_eq!(preview.skills, ["review"]);
        assert!(preview.installed.is_none());
        assert_eq!(preview.sha256.len(), 64);
        // The same content hashes the same, wherever it was staged.
        let again = inspect(&state, inspect_params("hooked@mk")).await.unwrap();
        assert_eq!(again.sha256, preview.sha256);
        assert_eq!(again.runs_digest, preview.runs_digest);
        assert!(!state.paths().data_dir().join(".staging").exists());
        assert!(!dest(&state, "hooked").exists());
        assert_nothing_ran(&[home.path(), market.path()]);
    }

    #[tokio::test]
    async fn a_non_strict_plugin_is_read_from_its_entry() {
        let (_home, _market, state) = setup().await;
        let preview = inspect(&state, inspect_params("loose@mk")).await.unwrap();
        assert_eq!(preview.name, "loose");
        assert_eq!(preview.description, "Loose");
        assert_eq!(preview.skills, ["note"]);
        assert!(preview.hooks.is_empty() && preview.servers.is_empty() && !preview.launcher);
    }

    #[tokio::test]
    async fn an_installed_folder_of_the_same_name_is_compared() {
        let (_home, _market, state) = setup().await;
        let before = inspect(&state, inspect_params("plain@mk")).await.unwrap();
        // The staged content, laid where it would be installed.
        let folder = dest(&state, "plain");
        write(
            &folder.join(".claude-plugin/plugin.json"),
            r#"{"name": "plain"}"#,
        );
        let after = inspect(&state, inspect_params("plain@mk")).await.unwrap();
        let installed = after.installed.unwrap();
        assert!(!installed.same_content && installed.recorded_sha256.is_none());
        assert_eq!(before.sha256, after.sha256);
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
    }
}
