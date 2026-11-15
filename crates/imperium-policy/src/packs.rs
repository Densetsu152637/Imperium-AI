//! Which workflow packs a session uses, and in what order: the folders the user listed (trusted),
//! the bundled default pack when none are listed or pinned (not trusted), then the packs the user
//! pinned from an update source (not trusted, and never under another pack's name).
//!
//! Finding the bundled pack is the caller's job: it differs between the daemon (its resource
//! folder) and the command line, so the path comes in as an input.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use imperium_workflow::personality::Sources;
use imperium_workflow::update::{PackStore, Pinned};
use imperium_workflow::{ContextIndex, Pack};

/// What decides the packs of a session.
#[derive(Debug, Clone)]
pub struct PackInputs<'a> {
    /// `workflow.enabled`: off, a session has no packs and no instruction files.
    pub enabled: bool,
    /// `workflow.packs`: the folders the user listed.
    pub listed: &'a [PathBuf],
    /// `workflow.default_pack`: use the bundled pack when nothing is listed or pinned.
    pub default_pack: bool,
    /// The folder `imperium pack` keeps pinned packs under (the daemon's data folder).
    pub data_dir: &'a Path,
    /// The user's config folder, where local personalities live.
    pub config_dir: &'a Path,
    /// The bundled default pack, if the caller found one.
    pub bundled: Option<PathBuf>,
}

/// The packs of a session.
#[derive(Debug, Clone)]
pub struct ResolvedPacks {
    /// The packs and the project's files; `None` when the workflow is off.
    pub index: Option<Arc<ContextIndex>>,
    /// Where personalities are looked up. Not affected by the workflow switch.
    pub personalities: Sources,
    /// Packs that were skipped, and why.
    pub notices: Vec<String>,
}

/// Layers the packs for a session in `project_dir`. Packs that cannot be read are skipped with a
/// notice rather than failing the session.
pub fn resolve(inputs: &PackInputs<'_>, project_dir: &Path) -> ResolvedPacks {
    let pinned = PackStore::new(inputs.data_dir).pinned();
    let personalities = sources(inputs, &pinned);
    if !inputs.enabled {
        return ResolvedPacks {
            index: None,
            personalities,
            notices: Vec::new(),
        };
    }
    let mut notices = Vec::new();
    // Versions the user accepted from a pack update source, whose files still match the hash
    // they were pinned at (`imperium pack`). They are not trusted like listed folders: their `@`
    // imports stay inside the pack.
    notices.extend(pinned.notices);
    let builtin = (inputs.listed.is_empty() && pinned.packs.is_empty() && inputs.default_pack)
        .then(|| inputs.bundled.clone())
        .flatten();
    let mut packs: Vec<Pack> = inputs
        .listed
        .iter()
        .chain(builtin.as_ref())
        .filter_map(|root| match Pack::load(root) {
            // Folders the user configured are trusted; the bundled snapshot is not.
            Ok(pack) if inputs.listed.contains(root) => Some(pack.trusted()),
            Ok(pack) => Some(pack),
            Err(error) => {
                notices.push(format!("workflow pack skipped: {error}"));
                None
            }
        })
        .collect();
    for found in pinned.packs {
        // A reference is `<pack name>:<path>`, so a pinned pack may not take the name of another
        // loaded pack or of the project's own files.
        if found.name == "project" || packs.iter().any(|pack| pack.name == found.name) {
            notices.push(format!(
                "pinned pack {} skipped: another pack already has that name",
                found.name
            ));
            continue;
        }
        match Pack::load(&found.dir) {
            Ok(mut pack) => {
                pack.name = found.name;
                packs.push(pack);
            }
            Err(error) => notices.push(format!("pinned pack {} skipped: {error}", found.name)),
        }
    }
    ResolvedPacks {
        index: Some(Arc::new(ContextIndex::new(packs, project_dir))),
        personalities,
        notices,
    }
}

/// Where personalities are looked up: `<config>/personalities/`, the listed folders, the pinned
/// packs, then the bundled pack (with the default compiled in as the last resort). The workflow
/// switch does not matter: personalities are found either way.
pub fn personality_sources(inputs: &PackInputs<'_>) -> Sources {
    sources(inputs, &PackStore::new(inputs.data_dir).pinned())
}

fn sources(inputs: &PackInputs<'_>, pinned: &Pinned) -> Sources {
    Sources {
        config_dir: Some(inputs.config_dir.to_path_buf()),
        packs: inputs.listed.to_vec(),
        confined: pinned
            .packs
            .iter()
            .map(|found| (found.name.clone(), found.dir.clone()))
            .collect(),
        built_in: inputs.bundled.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(base: &Path, name: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Index\n").unwrap();
        dir
    }

    fn layers(inputs: &PackInputs<'_>) -> (Option<Vec<(String, bool)>>, Vec<String>) {
        let project = tempfile::tempdir().unwrap();
        let resolved = resolve(inputs, project.path());
        let packs = resolved.index.map(|index| {
            index
                .packs()
                .iter()
                .map(|pack| (pack.name.clone(), pack.trusted))
                .collect()
        });
        (packs, resolved.notices)
    }

    #[test]
    fn the_bundled_path_is_used_only_without_listed_or_pinned_packs() {
        let base = tempfile::tempdir().unwrap();
        let bundled = pack(base.path(), "bundled");
        let listed = [pack(base.path(), "mine")];
        let mut inputs = PackInputs {
            enabled: true,
            listed: &[],
            default_pack: true,
            data_dir: &base.path().join("data"),
            config_dir: &base.path().join("config"),
            bundled: Some(bundled),
        };
        assert_eq!(layers(&inputs).0, Some(vec![("bundled".into(), false)]));
        inputs.default_pack = false;
        assert_eq!(layers(&inputs).0, Some(vec![]));
        inputs.default_pack = true;
        inputs.listed = &listed;
        assert_eq!(layers(&inputs).0, Some(vec![("mine".into(), true)]));
        inputs.listed = &[];
        inputs.bundled = None;
        assert_eq!(layers(&inputs).0, Some(vec![]));
    }

    #[test]
    fn a_workflow_that_is_off_has_no_packs_but_still_has_personality_sources() {
        let base = tempfile::tempdir().unwrap();
        let listed = [pack(base.path(), "mine")];
        let inputs = PackInputs {
            enabled: false,
            listed: &listed,
            default_pack: true,
            data_dir: &base.path().join("data"),
            config_dir: &base.path().join("config"),
            bundled: None,
        };
        let resolved = resolve(&inputs, base.path());
        assert!(resolved.index.is_none() && resolved.notices.is_empty());
        assert_eq!(resolved.personalities.packs, listed.to_vec());
        assert_eq!(personality_sources(&inputs).packs, listed.to_vec());
    }

    #[test]
    fn an_unreadable_listed_pack_is_skipped_with_a_notice() {
        let base = tempfile::tempdir().unwrap();
        let listed = [base.path().join("missing"), pack(base.path(), "good")];
        let inputs = PackInputs {
            enabled: true,
            listed: &listed,
            default_pack: true,
            data_dir: &base.path().join("data"),
            config_dir: &base.path().join("config"),
            bundled: None,
        };
        let (packs, notices) = layers(&inputs);
        assert_eq!(packs, Some(vec![("good".into(), true)]));
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].starts_with("workflow pack skipped: "),
            "{notices:?}"
        );
    }

    /// Pins a pack called `name` under `data_dir` the way `imperium pack accept` does.
    fn pin(data_dir: &Path, name: &str) {
        let store = PackStore::new(data_dir);
        let snapshot = imperium_workflow::update::Snapshot::from_files([(
            "AGENTS.md",
            "# Pinned
",
        )]);
        store
            .stage(name, "https://example.com/pack.git", "abc123", &snapshot)
            .unwrap();
        store.accept(name, &snapshot.hash(), true).unwrap();
    }

    #[test]
    fn pinned_packs_follow_the_listed_ones_and_replace_the_bundled_pack() {
        let base = tempfile::tempdir().unwrap();
        let data = base.path().join("data");
        pin(&data, "acw");
        pin(&data, "other");
        let listed = [pack(base.path(), "acw")];
        let mut inputs = PackInputs {
            enabled: true,
            listed: &listed,
            default_pack: true,
            data_dir: &data,
            config_dir: &base.path().join("config"),
            bundled: Some(pack(base.path(), "bundled")),
        };
        // A pinned pack cannot take a listed pack's name; the others follow, not trusted.
        let (packs, notices) = layers(&inputs);
        assert_eq!(
            packs,
            Some(vec![("acw".into(), true), ("other".into(), false)])
        );
        assert_eq!(
            notices,
            vec!["pinned pack acw skipped: another pack already has that name".to_string()]
        );
        // With nothing listed, the pinned packs stand in for the bundled one.
        inputs.listed = &[];
        let (packs, notices) = layers(&inputs);
        assert_eq!(
            packs,
            Some(vec![("acw".into(), false), ("other".into(), false)])
        );
        assert!(notices.is_empty(), "{notices:?}");
    }
}
