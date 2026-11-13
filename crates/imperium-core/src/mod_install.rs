//! `mods.inspect` and `mods.install`: installing and updating a UI mod at the user's request
//! (`imperium mods install|update`), with their approval of what it asks for.
//!
//! Inspecting fetches the source into a staging folder (a `git clone` of a URL, or a copy of a
//! folder) and reads the mod's manifest and hashes its files; it installs nothing and runs
//! nothing. The user is shown the permissions the mod requests, and for an update how they differ
//! from what they approved before. Installing fetches again and goes ahead only if the content
//! has the hash the user approved and the manifest asks for nothing beyond the permissions
//! they approved, so a source that changes between the two steps, or a moving branch, cannot
//! slip a wider mod in. The approval (hash, version, permissions) is kept in the mod's folder as
//! `.imperium-approval.json`, which the next update compares against.
//!
//! The daemon's own permissions are untouched: a mod's permissions only decide which data the
//! desktop app's frame may ask for (see `docs/06-extensibility.md`), and neither this module nor
//! the manifest can widen an agent's tool permissions or the safety floor.

use std::path::Path;

use imperium_protocol::methods::{
    InstallSource, ModInspectParams, ModInstallParams, ModInstallResult, ModInstalled, ModPreview,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::extension_install::{
    self, ExtensionKind, Fetched, InstallRequest, SOURCE_FILE, Source, Staging, check_name,
    check_subdir, move_tree, remove_tree, unix_ms, write_source,
};
use crate::state::DaemonState;

/// The user's approval of an installed mod, kept in its folder.
pub(crate) const APPROVAL_FILE: &str = ".imperium-approval.json";

#[derive(Debug, Serialize, Deserialize)]
struct Approval {
    sha256: String,
    version: String,
    permissions: Vec<String>,
    approved_at_unix_ms: u64,
}

fn read_approval(dir: &Path) -> Option<Approval> {
    let text = std::fs::read_to_string(dir.join(APPROVAL_FILE)).ok()?;
    serde_json::from_str(&text).ok()
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

/// What is installed under `id`: its approved permissions (those its manifest requests, when it
/// was installed without an approval record) and the hash recorded then.
fn installed(config_dir: &Path, id: &str) -> Option<ModInstalled> {
    let dir = ExtensionKind::Mod.dir(config_dir).join(id);
    if !dir.join("mod.json").is_file() {
        return None;
    }
    let approval = read_approval(&dir);
    let info = crate::mods::load(&dir, false).ok();
    let version = approval
        .as_ref()
        .map(|approval| approval.version.clone())
        .or_else(|| info.as_ref().map(|info| info.version.clone()))
        .unwrap_or_default();
    let permissions = match (&approval, &info) {
        (Some(approval), _) => approval.permissions.clone(),
        (None, Some(info)) => info.permissions.clone(),
        (None, None) => Vec::new(),
    };
    Some(ModInstalled {
        version,
        permissions,
        sha256: approval.map(|approval| approval.sha256),
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
    let installed = installed(&config_dir, &staged.info.id);
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
    let previous = installed(&config_dir, &id);
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
        _ => {}
    }
    let out = &staged.fetched.out;
    write_source(out, &staged.source)?;
    let approval = Approval {
        sha256: staged.sha256.clone(),
        version: staged.info.version.clone(),
        permissions: staged.info.permissions.clone(),
        approved_at_unix_ms: unix_ms(),
    };
    std::fs::write(
        out.join(APPROVAL_FILE),
        serde_json::to_string_pretty(&approval).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("could not record the approval: {error}"))?;
    std::fs::create_dir_all(&mods_dir)
        .map_err(|error| format!("could not create {}: {error}", mods_dir.display()))?;
    // An update keeps the old folder aside (like an uninstall) until the new one is in place.
    let kept = if previous.is_some() {
        let trash = extension_install::trash_dir(state);
        std::fs::create_dir_all(&trash)
            .map_err(|error| format!("could not create {}: {error}", trash.display()))?;
        let kept = trash.join(format!("mod-{id}-{}", unix_ms()));
        move_tree(&dest, &kept)?;
        Some(kept)
    } else {
        None
    };
    if let Err(error) = move_tree(out, &dest) {
        remove_tree(&dest);
        if let Some(kept) = &kept {
            let _ = move_tree(kept, &dest);
        }
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
/// its length, and its bytes, in path order. Leaves out `.git` and the bookkeeping files an
/// install writes, so a copy hashes the same as the source it came from.
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
        if name == ".git" || name == SOURCE_FILE || name == APPROVAL_FILE {
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
        }
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
    fn the_hash_follows_content_and_ignores_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        write_mod(dir.path(), "1.0.0", &["storage"], "export default {};");
        let first = tree_sha256(dir.path()).unwrap();
        std::fs::write(dir.path().join(SOURCE_FILE), "{}").unwrap();
        std::fs::write(dir.path().join(APPROVAL_FILE), "{}").unwrap();
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
        assert!(!state.mods_dir().join("demo").exists());
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

        let done = install(&state, install_params(&preview, source.path(), false))
            .await
            .unwrap();
        assert_eq!(done.version, "1.0.0");
        let folder = state.mods_dir().join("demo");
        assert!(folder.join("index.js").is_file());
        assert!(folder.join(SOURCE_FILE).is_file());
        let approval = read_approval(&folder).unwrap();
        assert_eq!(approval.sha256, preview.sha256);
        assert_eq!(approval.permissions, vec!["storage"]);
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
            read_approval(&folder).unwrap().permissions,
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
