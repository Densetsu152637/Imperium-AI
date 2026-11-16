use std::path::PathBuf;

use crate::{Paths, Settings};

/// The folders whose files Imperium runs or vouches for, which the safety floor guards so a write
/// there is always asked about: the installed mods (an approval vouches for their files), the
/// plugin folders (the user's own can carry hooks that rewrite calls), the pinned policy packs in
/// `<data>/packs`, `<config>/plugins` and `extensions.plugin_dirs`. The daemon and the headless
/// `imperium run` both protect these.
pub fn extension_dirs(paths: &Paths, settings: &Settings) -> Vec<PathBuf> {
    let mut dirs = vec![
        paths.config_dir().join("mods"),
        paths.config_dir().join("plugins"),
        paths.data_dir().join("packs"),
    ];
    dirs.extend(settings.extensions.plugin_dirs.iter().cloned());
    dirs
}
