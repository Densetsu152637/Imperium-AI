//! The folders whose files the daemon runs or vouches for are ones the safety floor always asks
//! about writing into: installed mods, `<config>/plugins`, and the user's own plugin folders
//! (a plugin there can carry hooks that rewrite calls).

use std::path::Path;

use imperium_config::{Config, Layer, Paths};
use imperium_core::DaemonState;
use imperium_policy::permissions::{CallKind, Target, safety_floor};

fn floored(path: &Path) -> bool {
    safety_floor(
        CallKind::Edit,
        &Target::Path {
            absolute: path.to_string_lossy().replace('\\', "/"),
            relative: None,
        },
    )
}

#[test]
fn mods_and_plugin_folders_are_guarded() {
    let home = tempfile::tempdir().unwrap();
    let own = tempfile::tempdir().unwrap();
    let mut settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
    settings.extensions.plugin_dirs = vec![own.path().to_path_buf()];
    let paths = Paths::under(home.path());
    let _state = DaemonState::new(paths.clone(), settings, None);

    assert!(floored(&paths.config_dir().join("mods/m/index.js")));
    assert!(floored(
        &paths.config_dir().join("plugins/p/hooks/hooks.json")
    ));
    assert!(floored(&own.path().join("p/hooks/hooks.json")));
    // Other things in the config folder are not covered by this rule.
    assert!(!floored(&paths.config_dir().join("skills/s/SKILL.md")));
}
