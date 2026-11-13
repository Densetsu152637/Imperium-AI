//! `mods.inspect` and `mods.install`: installing and updating a UI mod at the user's request
//! (`imperium mods install|update`), with their approval of what it asks for.
//!
//! Inspecting fetches the source into a staging folder (a `git clone` of a URL, or a copy of a
//! folder) and reads the mod's manifest and hashes its files; it installs nothing and runs
//! nothing. The user is shown the permissions the mod requests, and for an update how they differ
//! from what they approved before. Installing fetches again and goes ahead only if the content
//! has the hash the user approved and the manifest asks for nothing beyond the permissions
//! they approved, so a source that changes between the two steps, or a moving branch, cannot
//! slip a wider mod in.
//!
//! The approval (content hash, version, approved permissions, the approved source, and whether
//! it was given at a terminal) is kept in the daemon's data folder, `<data>/mod-approvals.json`,
//! keyed by mod id, never in the mod's own folder, which the mod (or an agent) can write.
//! `mods.list` then reports a mod with only the permissions that are both in its manifest and
//! approved, withholds all of them (and turns the mod off) when its files no longer hash to the
//! approved content, and reports the approved source as `installed_from`; an update reads the
//! source from there, not from the folder's `.imperium-source.json`, and refuses to move to
//! another source without a fresh install.
//!
//! The daemon's own permissions are untouched: a mod's permissions only decide which data the
//! desktop app's frame may ask for (see `docs/06-extensibility.md`), and neither this module nor
//! the manifest can widen an agent's tool permissions or the safety floor.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use imperium_protocol::methods::{
    InstallSource, ModInspectParams, ModInstallParams, ModInstallResult, ModInstalled,
    ModListResult, ModPreview,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::extension_install::{
    self, ExtensionKind, Fetched, InstallRequest, SOURCE_FILE, Source, Staging, check_name,
    check_subdir, move_tree, remove_tree, unix_ms, write_source,
};
use crate::state::DaemonState;

/// The user's approval of an installed mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Approval {
    pub sha256: String,
    pub version: String,
    pub permissions: Vec<String>,
    /// The source the user approved; updates come from here.
    pub source: InstallSource,
    /// Approved by answering a prompt at a terminal. Otherwise the mod stays off until the user
    /// turns it on in the desktop app.
    pub interactive: bool,
    pub approved_at_unix_ms: u64,
}

type Store = BTreeMap<String, Approval>;

/// Serialises read-modify-write of the approvals file.
static STORE_LOCK: Mutex<()> = Mutex::new(());

fn store_path(state: &DaemonState) -> std::path::PathBuf {
    state.paths().data_dir().join("mod-approvals.json")
}

/// The approvals: empty when there is no file yet, `None` when there is one that cannot be
/// read (corrupt, or unreadable), which is not the same as nothing being approved.
fn load_store(state: &DaemonState) -> Option<Store> {
    match std::fs::read_to_string(store_path(state)) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(store) => Some(store),
            Err(error) => {
                tracing::warn!(%error, "the mod approvals file is corrupt");
                None
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Store::new()),
        Err(error) => {
            tracing::warn!(%error, "the mod approvals file cannot be read");
            None
        }
    }
}

fn approval(state: &DaemonState, id: &str) -> Option<Approval> {
    load_store(state)?.remove(id)
}

/// Records (or, with `None`, forgets) the approval of `id`.
fn set_approval(state: &DaemonState, id: &str, approval: Option<Approval>) -> Result<(), String> {
    let _lock = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load_store(state).ok_or_else(|| {
        format!(
            "{} cannot be read, so no approval can be recorded; fix or remove it",
            store_path(state).display()
        )
    })?;
    match approval {
        Some(approval) => store.insert(id.to_string(), approval),
        None => store.remove(id),
    };
    let path = store_path(state);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&store).map_err(|error| error.to_string())?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)
        .map_err(|error| format!("could not record the approval: {error}"))?;
    std::fs::rename(&temp, &path).map_err(|error| format!("could not record the approval: {error}"))
}

/// Applies the approvals to what `mods.list` found in the user's folder. A mod with an approval
/// record shows only the permissions that are in its manifest and approved; if its folder no
/// longer hashes to the approved content, it shows none, is marked `modified`, and is off until
/// the user turns it on; one approved without a terminal prompt is off until turned on. Its
/// source is the approved one. A mod without a record (copied in by hand, or installed by an
/// agent) is left as it is. If the approvals file exists but cannot be read, nothing can be
/// vouched for: every installed mod is treated as modified (no permissions, off).
pub(crate) fn apply_approvals(state: &DaemonState, listing: &mut ModListResult) {
    let Some(store) = load_store(state) else {
        for info in listing.mods.iter_mut().filter(|info| !info.bundled) {
            info.permissions.clear();
            info.modified = true;
            info.opt_in = true;
        }
        return;
    };
    if store.is_empty() {
        return;
    }
    let mods_dir = state.mods_dir();
    for info in listing.mods.iter_mut().filter(|info| !info.bundled) {
        let Some(record) = store.get(&info.id) else {
            continue;
        };
        info.installed_from = Some(record.source.clone());
        info.permissions
            .retain(|permission| record.permissions.contains(permission));
        let intact = tree_sha256(&mods_dir.join(&info.id))
            .is_ok_and(|hash| hash.eq_ignore_ascii_case(&record.sha256));
        if !intact {
            info.permissions.clear();
            info.modified = true;
        }
        info.opt_in = !record.interactive || !intact;
    }
}

/// What a source holds, fetched into `staging`.
struct Staged {
    info: imperium_protocol::methods::ModInfo,
    sha256: String,
    fetched: Fetched,
    source: InstallSource,
}

async fn stage(
    config_dir: &Path,
    staging: &Path,
    source: &str,
    revision: Option<&str>,
    path: Option<&str>,
) -> Result<Staged, String> {
    let revision = revision.map(str::trim).filter(|r| !r.is_empty());
    let subdir = path
        .map(str::trim)
        .filter(|path| !path.is_empty() && *path != ".")
        .map(check_subdir)
        .transpose()?;
    let source = Source::parse(source, Path::new("."))?;
    if revision.is_some() && matches!(source, Source::Path(_)) {
        return Err("revision applies to a git source only".into());
    }
    let request = InstallRequest {
        kind: ExtensionKind::Mod,
        source,
        revision: revision.map(str::to_string),
        subdir: subdir.clone(),
        session_id: None,
    };
    let fetched =
        extension_install::fetch(config_dir, staging, &request, subdir.as_deref()).await?;
    if !fetched.out.join("mod.json").is_file() {
        return Err("no mod.json at the top of the source (use --path to pick its folder)".into());
    }
    let info = crate::mods::load(&fetched.out, false)?;
    check_name(&info.id)?;
    let sha256 = tree_sha256(&fetched.out)?;
    let source = InstallSource {
        kind: match request.source {
            Source::Git(_) => "git",
            Source::Path(_) => "path",
        }
        .into(),
        location: fetched.location.clone(),
        revision: fetched.revision.clone(),
        requested: request.revision.clone(),
        subdir,
        installed_at_unix_ms: unix_ms(),
        session_id: None,
    };
    Ok(Staged {
        info,
        sha256,
        fetched,
        source,
    })
}

/// What is installed under `id`, if `<config>/mods/<id>` holds a mod of that id: the permissions
/// and hash the user approved (the manifest's permissions, and no hash, for a mod without an
/// approval record).
fn installed(state: &DaemonState, id: &str) -> Option<ModInstalled> {
    let dir = state.mods_dir().join(id);
    let info = crate::mods::load(&dir, false)
        .ok()
        .filter(|info| info.id == id)?;
    Some(match approval(state, id) {
        Some(record) => ModInstalled {
            version: record.version,
            permissions: record.permissions,
            sha256: Some(record.sha256),
        },
        None => ModInstalled {
            version: info.version,
            permissions: info.permissions,
            sha256: None,
        },
    })
}

/// `mods.inspect`.
pub(crate) async fn inspect(
    state: &DaemonState,
    params: ModInspectParams,
) -> Result<ModPreview, String> {
    let config_dir = state.paths().config_dir().to_path_buf();
    let staging = Staging::new(&config_dir)?;
    let staged = stage(
        &config_dir,
        staging.path(),
        &params.source,
        params.revision.as_deref(),
        params.path.as_deref(),
    )
    .await?;
    let installed = installed(state, &staged.info.id);
    let (added, removed) = permission_diff(
        installed.as_ref().map(|i| i.permissions.as_slice()),
        &staged.info.permissions,
    );
    Ok(ModPreview {
        id: staged.info.id,
        name: staged.info.name,
        version: staged.info.version,
        permissions: staged.info.permissions,
        panels: staged.info.panels,
        sha256: staged.sha256,
        location: staged.source.location,
        revision: staged.source.revision,
        installed,
        added_permissions: added,
        removed_permissions: removed,
    })
}

/// Permissions in `requested` that `approved` lacks, and those in `approved` no longer requested.
/// With nothing approved (a new mod) every request is added.
pub(crate) fn permission_diff(
    approved: Option<&[String]>,
    requested: &[String],
) -> (Vec<String>, Vec<String>) {
    let approved = approved.unwrap_or_default();
    let added = requested
        .iter()
        .filter(|permission| !approved.contains(permission))
        .cloned()
        .collect();
    let removed = approved
        .iter()
        .filter(|permission| !requested.contains(permission))
        .cloned()
        .collect();
    (added, removed)
}

/// `mods.install`.
pub(crate) async fn install(
    state: &DaemonState,
    params: ModInstallParams,
) -> Result<ModInstallResult, String> {
    let config_dir = state.paths().config_dir().to_path_buf();
    let staging = Staging::new(&config_dir)?;
    let staged = stage(
        &config_dir,
        staging.path(),
        &params.source,
        params.revision.as_deref(),
        params.path.as_deref(),
    )
    .await?;
    if !params.sha256.eq_ignore_ascii_case(&staged.sha256) {
        return Err(format!(
            "the source no longer has the content that was approved (it hashes to {}, not {}); \
             inspect it and approve again",
            staged.sha256, params.sha256
        ));
    }
    if let Some(extra) = staged
        .info
        .permissions
        .iter()
        .find(|permission| !params.permissions.contains(permission))
    {
        return Err(format!(
            "the mod requests {extra}, which was not approved; nothing was installed"
        ));
    }
    let id = staged.info.id.clone();
    let mods_dir = ExtensionKind::Mod.dir(&config_dir);
    let dest = mods_dir.join(&id);
    let previous = installed(state, &id);
    // Something at the destination that is not this mod is never written over or merged into.
    if dest.exists() && previous.is_none() {
        return Err(format!(
            "{} exists and is not the mod {id}; move it away first",
            dest.display()
        ));
    }
    let mut interactive = params.interactive;
    match (&previous, params.replace) {
        (Some(_), false) => {
            return Err(format!(
                "a mod {id} is already installed at {}; update it with `imperium mods update {id}`",
                dest.display()
            ));
        }
        (None, true) => {
            return Err(format!(
                "no mod {id} is installed, so there is nothing to update"
            ));
        }
        (Some(_), true) => {
            let record = approval(state, &id).ok_or_else(|| {
                format!(
                    "{id} was not installed with `imperium mods install`, so it has no approved \
                     source to update from; uninstall it and install it again"
                )
            })?;
            let approved = &record.source;
            let now = &staged.source;
            if approved.location != now.location
                || approved.subdir != now.subdir
                || approved.requested != now.requested
            {
                return Err(format!(
                    "the update comes from a different source than the approved one ({}); \
                     install the mod fresh to change where it comes from",
                    approved.location
                ));
            }
            // A prompt at a terminal can be inherited only by an update that asks for nothing
            // new; one that widens is off unless it was itself approved at a prompt.
            let (added, _) = permission_diff(
                Some(record.permissions.as_slice()),
                &staged.info.permissions,
            );
            if added.is_empty() {
                interactive |= record.interactive;
            }
        }
        (None, false) => {}
    }
    let out = &staged.fetched.out;
    write_source(out, &staged.source)?;
    std::fs::create_dir_all(&mods_dir)
        .map_err(|error| format!("could not create {}: {error}", mods_dir.display()))?;
    // The approval is recorded before the folder appears, so no list ever shows the new files
    // without it.
    let before = approval(state, &id);
    set_approval(
        state,
        &id,
        Some(Approval {
            sha256: staged.sha256.clone(),
            version: staged.info.version.clone(),
            permissions: staged.info.permissions.clone(),
            source: staged.source.clone(),
            interactive,
            approved_at_unix_ms: unix_ms(),
        }),
    )?;
    // An update keeps the old folder aside (like an uninstall) until the new one is in place.
    let kept = if previous.is_some() {
        let trash = extension_install::trash_dir(state);
        let moved = std::fs::create_dir_all(&trash)
            .map_err(|error| format!("could not create {}: {error}", trash.display()))
            .and_then(|()| {
                let kept = trash.join(format!("mod-{id}-{}", unix_ms()));
                move_tree(&dest, &kept).map(|()| kept)
            });
        match moved {
            Ok(kept) => Some(kept),
            Err(error) => {
                let _ = set_approval(state, &id, before);
                return Err(error);
            }
        }
    } else {
        None
    };
    if let Err(error) = move_tree(out, &dest) {
        remove_tree(&dest);
        if let Some(kept) = &kept {
            let _ = move_tree(kept, &dest);
        }
        let _ = set_approval(state, &id, before);
        return Err(error);
    }
    Ok(ModInstallResult {
        id,
        version: staged.info.version,
        path: dest.display().to_string(),
        permissions: staged.info.permissions,
        sha256: staged.sha256,
        replaced: previous.map(|previous| previous.version),
    })
}

/// SHA-256, as hex, of a folder's files: each one's path relative to the folder (with `/`),
/// its length, and its bytes, in path order. Leaves out `.git` and the source file an install
/// writes, so a copy hashes the same as the source it came from.
pub(crate) fn tree_sha256(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect(dir, "", &mut files)?;
    files.sort();
    let mut digest = Sha256::new();
    for relative in files {
        let bytes =
            std::fs::read(dir.join(&relative)).map_err(|error| format!("{relative}: {error}"))?;
        digest.update(relative.as_bytes());
        digest.update([0u8]);
        digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        digest.update(&bytes);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn collect(root: &Path, prefix: &str, files: &mut Vec<String>) -> Result<(), String> {
    let folder = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(prefix)
    };
    for entry in std::fs::read_dir(&folder)
        .map_err(|error| format!("{}: {error}", folder.display()))?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == SOURCE_FILE {
            continue;
        }
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(root, &relative, files)?;
        } else if kind.is_file() {
            files.push(relative);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use imperium_config::{Config, Layer, Paths};
    use imperium_protocol::methods::{Method, ModInstall};

    use super::*;

    fn state(home: &Path) -> Arc<DaemonState> {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        DaemonState::new(Paths::under(home), settings, None)
    }

    fn write_mod(dir: &Path, version: &str, permissions: &[&str], body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("mod.json"),
            serde_json::json!({
                "id": "demo", "name": "Demo", "version": version,
                "permissions": permissions,
                "contributes": {"panels": [{"id": "p", "title": "Demo"}]},
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("index.js"), body).unwrap();
    }

    fn inspect_params(source: &Path) -> ModInspectParams {
        ModInspectParams {
            source: source.display().to_string(),
            revision: None,
            path: None,
        }
    }

    fn install_params(preview: &ModPreview, source: &Path, replace: bool) -> ModInstallParams {
        ModInstallParams {
            source: source.display().to_string(),
            revision: None,
            path: None,
            sha256: preview.sha256.clone(),
            permissions: preview.permissions.clone(),
            replace,
            interactive: true,
        }
    }

    fn listed(state: &DaemonState) -> ModListResult {
        let mut listing = crate::mods::list(&state.mods_dir(), &[]);
        apply_approvals(state, &mut listing);
        listing
    }

    #[test]
    fn the_diff_lists_added_and_removed_permissions() {
        let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (added, removed) = permission_diff(
            Some(&strings(&["storage", "pets:read"])[..]),
            &strings(&["storage", "sessions:read"]),
        );
        assert_eq!(added, strings(&["sessions:read"]));
        assert_eq!(removed, strings(&["pets:read"]));
        let (added, removed) = permission_diff(None, &strings(&["storage"]));
        assert_eq!(added, strings(&["storage"]));
        assert!(removed.is_empty());
    }

    #[test]
    fn the_hash_follows_content_and_ignores_the_source_file() {
        let dir = tempfile::tempdir().unwrap();
        write_mod(dir.path(), "1.0.0", &["storage"], "export default {};");
        let first = tree_sha256(dir.path()).unwrap();
        std::fs::write(dir.path().join(SOURCE_FILE), "{}").unwrap();
        assert_eq!(tree_sha256(dir.path()).unwrap(), first);
        std::fs::write(dir.path().join("index.js"), "export default 1;").unwrap();
        assert_ne!(tree_sha256(dir.path()).unwrap(), first);
    }

    #[tokio::test]
    async fn inspect_reports_permissions_and_installs_nothing() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(
            source.path(),
            "1.0.0",
            &["storage", "pets:read"],
            "export default {};",
        );
        let state = state(home.path());
        let preview = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        assert_eq!(preview.id, "demo");
        assert_eq!(preview.permissions, vec!["storage", "pets:read"]);
        assert_eq!(preview.added_permissions, vec!["storage", "pets:read"]);
        assert!(preview.installed.is_none());
        assert_eq!(preview.sha256.len(), 64);
        assert!(!state.mods_dir().exists());
        assert!(load_store(&state).unwrap().is_empty());
    }

    #[tokio::test]
    async fn install_needs_the_approved_hash_and_permissions() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(source.path(), "1.0.0", &["storage"], "export default {};");
        let state = state(home.path());
        let preview = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();

        // Content that changed after the user was shown it.
        let mut stale = install_params(&preview, source.path(), false);
        stale.sha256 = "0".repeat(64);
        let error = install(&state, stale).await.unwrap_err();
        assert!(error.contains("no longer has the content"), "{error}");

        // A permission the user did not approve.
        let mut narrow = install_params(&preview, source.path(), false);
        narrow.permissions = Vec::new();
        let error = install(&state, narrow).await.unwrap_err();
        assert!(error.contains("storage"), "{error}");
        assert!(!state.mods_dir().join("demo").exists());
        assert!(load_store(&state).unwrap().is_empty());

        let done = install(&state, install_params(&preview, source.path(), false))
            .await
            .unwrap();
        assert_eq!(done.version, "1.0.0");
        let folder = state.mods_dir().join("demo");
        assert!(folder.join("index.js").is_file());
        assert!(folder.join(SOURCE_FILE).is_file());
        // The approval is in the daemon's data folder, not in the mod's.
        assert!(!folder.join(".imperium-approval.json").exists());
        let record = approval(&state, "demo").unwrap();
        assert_eq!(record.sha256, preview.sha256);
        assert_eq!(record.permissions, vec!["storage"]);
        assert!(record.interactive);
        // The folder holds the content that was hashed.
        assert_eq!(tree_sha256(&folder).unwrap(), preview.sha256);
        // Not over an existing mod, except as an update.
        let error = install(&state, install_params(&preview, source.path(), false))
            .await
            .unwrap_err();
        assert!(error.contains("mods update"), "{error}");
        // No staging folder is left behind.
        assert!(!state.paths().config_dir().join(".staging").exists());
    }

    #[tokio::test]
    async fn something_else_at_the_destination_is_never_merged_into() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(source.path(), "1.0.0", &["storage"], "export default {};");
        let state = state(home.path());
        let preview = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        let in_the_way = state.mods_dir().join("demo");
        std::fs::create_dir_all(&in_the_way).unwrap();
        std::fs::write(in_the_way.join("keep.txt"), "mine").unwrap();
        let error = install(&state, install_params(&preview, source.path(), false))
            .await
            .unwrap_err();
        assert!(error.contains("is not the mod demo"), "{error}");
        assert!(in_the_way.join("keep.txt").is_file());
        assert!(!in_the_way.join("index.js").exists());
    }

    #[tokio::test]
    async fn an_update_shows_the_permission_diff_and_keeps_the_old_folder_aside() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(
            source.path(),
            "1.0.0",
            &["storage", "pets:read"],
            "export default 1;",
        );
        let state = state(home.path());
        let first = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        install(&state, install_params(&first, source.path(), false))
            .await
            .unwrap();

        write_mod(
            source.path(),
            "1.1.0",
            &["storage", "sessions:read"],
            "export default 2;",
        );
        let preview = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        let installed = preview.installed.as_ref().unwrap();
        assert_eq!(installed.version, "1.0.0");
        assert_eq!(installed.permissions, vec!["storage", "pets:read"]);
        assert_eq!(installed.sha256.as_deref(), Some(first.sha256.as_str()));
        assert_eq!(preview.added_permissions, vec!["sessions:read"]);
        assert_eq!(preview.removed_permissions, vec!["pets:read"]);

        // Approving only what it had before refuses the wider update.
        let mut narrow = install_params(&preview, source.path(), true);
        narrow.permissions = vec!["storage".into(), "pets:read".into()];
        assert!(install(&state, narrow).await.is_err());
        assert!(
            std::fs::read_to_string(state.mods_dir().join("demo/index.js"))
                .unwrap()
                .contains('1')
        );

        let done = install(&state, install_params(&preview, source.path(), true))
            .await
            .unwrap();
        assert_eq!(done.replaced.as_deref(), Some("1.0.0"));
        let folder = state.mods_dir().join("demo");
        assert_eq!(
            std::fs::read_to_string(folder.join("index.js")).unwrap(),
            "export default 2;"
        );
        assert_eq!(
            approval(&state, "demo").unwrap().permissions,
            vec!["storage", "sessions:read"]
        );
        let kept: Vec<_> = std::fs::read_dir(extension_install::trash_dir(&state))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(kept.len(), 1);
        // Updating something that is not installed is refused.
        std::fs::remove_dir_all(&folder).unwrap();
        let error = install(&state, install_params(&preview, source.path(), true))
            .await
            .unwrap_err();
        assert!(error.contains("nothing to update"), "{error}");
    }

    #[tokio::test]
    async fn an_update_cannot_move_to_another_source() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        write_mod(source.path(), "1.0.0", &["storage"], "export default 1;");
        write_mod(
            other.path(),
            "9.0.0",
            &["storage"],
            "export default 'other';",
        );
        let state = state(home.path());
        let first = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        install(&state, install_params(&first, source.path(), false))
            .await
            .unwrap();
        // Even with a forged source file in the folder, the approved source is the one in the
        // daemon's store, and another source needs a fresh install.
        let folder = state.mods_dir().join("demo");
        std::fs::write(
            folder.join(SOURCE_FILE),
            serde_json::to_string(&InstallSource {
                kind: "path".into(),
                location: other.path().display().to_string(),
                revision: None,
                requested: None,
                subdir: None,
                installed_at_unix_ms: 0,
                session_id: None,
            })
            .unwrap(),
        )
        .unwrap();
        let listing = listed(&state);
        let shown = listing.mods[0].installed_from.as_ref().unwrap();
        assert_eq!(shown.location, first.location);
        let preview = inspect(&state, inspect_params(other.path())).await.unwrap();
        let error = install(&state, install_params(&preview, other.path(), true))
            .await
            .unwrap_err();
        assert!(error.contains("different source"), "{error}");
        assert!(
            std::fs::read_to_string(folder.join("index.js"))
                .unwrap()
                .contains('1')
        );
    }

    #[tokio::test]
    async fn a_forged_approval_in_the_folder_does_nothing_and_edits_lose_permissions() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(source.path(), "1.0.0", &["storage"], "export default 1;");
        let state = state(home.path());
        let preview = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        install(&state, install_params(&preview, source.path(), false))
            .await
            .unwrap();
        let folder = state.mods_dir().join("demo");
        let listing = listed(&state);
        assert_eq!(listing.mods[0].permissions, vec!["storage"]);
        assert!(!listing.mods[0].modified);
        assert!(!listing.mods[0].opt_in);

        // A mod writes an approval of its own, and asks for more in its manifest.
        std::fs::write(
            folder.join(".imperium-approval.json"),
            serde_json::json!({
                "sha256": "x", "version": "1", "permissions": ["storage", "pets:read", "sessions:read"],
                "approved_at_unix_ms": 0
            })
            .to_string(),
        )
        .unwrap();
        write_mod(
            &folder,
            "1.0.0",
            &["storage", "pets:read", "sessions:read"],
            "export default 1;",
        );
        let listing = listed(&state);
        let info = &listing.mods[0];
        // It gained nothing: the edit is flagged, its permissions withheld, and it is off.
        assert!(info.modified);
        assert!(info.permissions.is_empty());
        assert!(info.opt_in);
        // And an update is still measured against what the user approved.
        write_mod(
            source.path(),
            "1.0.0",
            &["storage", "pets:read"],
            "export default 1;",
        );
        let again = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        assert_eq!(again.installed.unwrap().permissions, vec!["storage"]);
        assert_eq!(again.added_permissions, vec!["pets:read"]);
    }

    #[tokio::test]
    async fn a_mod_approved_without_a_terminal_is_off_until_turned_on() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(source.path(), "1.0.0", &["storage"], "export default 1;");
        let state = state(home.path());
        let preview = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        let mut params = install_params(&preview, source.path(), false);
        params.interactive = false;
        install(&state, params).await.unwrap();
        let listing = listed(&state);
        assert!(listing.mods[0].opt_in);
        assert!(!listing.mods[0].modified);
        assert_eq!(listing.mods[0].permissions, vec!["storage"]);
    }

    #[tokio::test]
    async fn only_a_prompted_update_may_widen_a_prompted_install() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(source.path(), "1.0.0", &["storage"], "export default 1;");
        let state = state(home.path());
        let first = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        install(&state, install_params(&first, source.path(), false))
            .await
            .unwrap();
        assert!(!listed(&state).mods[0].opt_in);

        // A widening update approved with --yes (no prompt) turns the mod off.
        write_mod(
            source.path(),
            "1.1.0",
            &["storage", "pets:read"],
            "export default 2;",
        );
        let wider = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        let mut flag = install_params(&wider, source.path(), true);
        flag.interactive = false;
        install(&state, flag).await.unwrap();
        let listing = listed(&state);
        assert!(listing.mods[0].opt_in);
        assert_eq!(listing.mods[0].permissions, vec!["storage", "pets:read"]);

        // A prompted widening update is on; a later update that asks for nothing new keeps it so.
        write_mod(
            source.path(),
            "1.2.0",
            &["storage", "pets:read", "sessions:read"],
            "export default 3;",
        );
        let wider = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        install(&state, install_params(&wider, source.path(), true))
            .await
            .unwrap();
        assert!(!listed(&state).mods[0].opt_in);
        write_mod(
            source.path(),
            "1.2.1",
            &["storage", "pets:read", "sessions:read"],
            "export default 4;",
        );
        let same = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        let mut flag = install_params(&same, source.path(), true);
        flag.interactive = false;
        install(&state, flag).await.unwrap();
        assert!(!listed(&state).mods[0].opt_in);
    }

    #[tokio::test]
    async fn an_unreadable_approvals_file_vouches_for_nothing() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        write_mod(source.path(), "1.0.0", &["storage"], "export default 1;");
        let state = state(home.path());
        let preview = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap();
        install(&state, install_params(&preview, source.path(), false))
            .await
            .unwrap();
        assert_eq!(listed(&state).mods[0].permissions, vec!["storage"]);

        // Corrupt, as opposed to absent: the mod keeps no permissions and is off.
        std::fs::write(store_path(&state), "{ not json").unwrap();
        let listing = listed(&state);
        assert!(listing.mods[0].modified);
        assert!(listing.mods[0].opt_in);
        assert!(listing.mods[0].permissions.is_empty());
        // Nothing is recorded over it either.
        let error = install(&state, install_params(&preview, source.path(), true))
            .await
            .unwrap_err();
        assert!(
            error.contains("cannot be read") || error.contains("no approved"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(store_path(&state)).unwrap(),
            "{ not json"
        );

        // Absent is the documented case: a mod without a record behaves as before.
        std::fs::remove_file(store_path(&state)).unwrap();
        let listing = listed(&state);
        assert!(!listing.mods[0].modified);
        assert_eq!(listing.mods[0].permissions, vec!["storage"]);
    }

    #[tokio::test]
    async fn a_source_without_a_valid_mod_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let state = state(home.path());
        let error = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap_err();
        assert!(error.contains("no mod.json"), "{error}");
        write_mod(source.path(), "1.0.0", &["root-access"], "x");
        let error = inspect(&state, inspect_params(source.path()))
            .await
            .unwrap_err();
        assert!(error.contains("unknown permission"), "{error}");
        // Methods are named for the CLI to call.
        assert_eq!(ModInstall::NAME, "mods.install");
    }
}
