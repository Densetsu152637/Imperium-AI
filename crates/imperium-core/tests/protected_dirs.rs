//! The folders whose files the daemon runs or vouches for are ones the safety floor always asks
//! about writing into: installed mods, `<config>/plugins`, and the user's own plugin folders
//! (a plugin there can carry hooks that rewrite calls), and the approval stores.

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
    // The user's skills, agents, commands and settings are refused outright, not asked about
    // (`tests/config_guard.rs`), so the floor does not list them.
    for refused in [
        "skills/s/SKILL.md",
        "agents/a.md",
        "commands/c.md",
        "config.toml",
    ] {
        let path = paths.config_dir().join(refused);
        assert!(
            imperium_policy::permissions::refused_write(&path.to_string_lossy().replace('\\', "/")),
            "{refused}"
        );
    }
    // And the daemon's approval stores in the data folder.
    assert!(floored(&paths.data_dir().join("skill-installs.json")));
    assert!(floored(&paths.data_dir().join("plugin-installs.json")));
    // A folder beside the config folder is not.
    assert!(!floored(&home.path().join("elsewhere/x.md")));
}
