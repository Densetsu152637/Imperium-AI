//! The daemon's part of code mode's confinement (docs/plans/code-mode.md, C1 "Windows").
//!
//! On Windows the worker runs at Low integrity, so everything Imperium keeps must carry a Medium
//! `NO_READ_UP` label a Low process cannot read past. The label is inherited, so files and
//! environments made later are covered too. Until the folders are labelled, or when a file cannot
//! take the label, code mode is unavailable and the reason names the file
//! (`imperium_script::windows::labels::status`). Elsewhere the worker confines itself and there
//! is nothing to do here.
//!
//! Labelling rewrites the security descriptor of everything in the daemon's folders the first
//! time, so it happens only for code mode: [`enable`] when code mode is on at start or is turned
//! on, and [`follow_move`] after the working folders move, only once code mode was enabled in this
//! daemon. A daemon that never uses code mode never labels anything. Nothing calls [`enable`]
//! until stage 2 adds `harness.code_mode` (plan stage 2).

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use imperium_config::Paths;

/// The folders a Low-integrity worker must not read: the data folder, the config folder, and the
/// environments' working folders when `workspaces.root` puts them outside both.
#[must_use]
pub fn label_targets(paths: &Paths, workspaces_root: &Path) -> Vec<PathBuf> {
    let mut targets: Vec<PathBuf> = Vec::new();
    for folder in [paths.data_dir(), paths.config_dir(), workspaces_root] {
        if !targets.iter().any(|target| folder.starts_with(target)) {
            targets.retain(|target| !target.starts_with(folder));
            targets.push(folder.to_path_buf());
        }
    }
    targets
}

/// Whether code mode was enabled in this daemon, so its labels must follow the working folders.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Code mode is on at start, or is being turned on: labels the folders of [`label_targets`] in the
/// background (Windows), and from now on [`follow_move`] labels the working folders' new place.
pub fn enable(paths: &Paths, workspaces_root: &Path) {
    ENABLED.store(true, Ordering::SeqCst);
    label_folders(paths, workspaces_root);
}

/// Whether [`enable`] was called in this daemon.
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// The working folders moved to `workspaces_root`: their labels follow when code mode is enabled;
/// otherwise nothing is labelled.
pub fn follow_move(paths: &Paths, workspaces_root: &Path) {
    if enabled() {
        label_folders(paths, workspaces_root);
    }
}

/// Labels the folders of [`label_targets`] in the background (Windows); does nothing elsewhere.
fn label_folders(paths: &Paths, workspaces_root: &Path) {
    #[cfg(windows)]
    {
        let targets = label_targets(paths, workspaces_root);
        let spawned = std::thread::Builder::new()
            .name("code-mode-labels".to_owned())
            .spawn(
                move || match imperium_script::windows::labels::apply(&targets) {
                    Ok(done) => {
                        tracing::debug!(?done, "code mode: the daemon's folders are labelled")
                    }
                    Err(reason) => tracing::warn!(%reason, "code mode is unavailable"),
                },
            );
        if let Err(error) = spawned {
            tracing::warn!(%error, "code mode: could not start labelling the daemon's folders");
        }
    }
    #[cfg(not(windows))]
    let _ = (paths, workspaces_root);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_and_config_folders_are_labelled_and_working_folders_only_when_elsewhere() {
        let paths = Paths::under(Path::new("/home/user/.imperium"));
        let inside = paths.data_dir().join("workspaces");
        assert_eq!(
            label_targets(&paths, &inside),
            vec![
                paths.data_dir().to_path_buf(),
                paths.config_dir().to_path_buf()
            ]
        );
        let elsewhere = Path::new("/work/imperium");
        assert_eq!(
            label_targets(&paths, elsewhere),
            vec![
                paths.data_dir().to_path_buf(),
                paths.config_dir().to_path_buf(),
                elsewhere.to_path_buf()
            ]
        );
        // A root that holds the others replaces them.
        let home = Path::new("/home/user/.imperium");
        assert_eq!(label_targets(&paths, home), vec![home.to_path_buf()]);
    }

    #[test]
    fn nothing_is_labelled_until_code_mode_is_enabled() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path());
        // A start and a move of the working folders with code mode never enabled.
        follow_move(&paths, &root.path().join("elsewhere"));
        assert!(!enabled());
        #[cfg(windows)]
        assert!(imperium_script::windows::labels::status().is_err());
    }
}
