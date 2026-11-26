//! What the daemon keeps file tools away from: the extension folders and `config.toml` are
//! refused (not asked about), the approval stores are asked about, and nothing else in a folder
//! shared by config and data (macOS) is touched, so a write into a workspace there still runs.

use imperium_config::{Config, Layer, Paths};
use imperium_core::DaemonState;
use imperium_policy::permissions::{CallKind, Target, refused_write, safety_floor};

fn slash(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn floored(path: &std::path::Path) -> bool {
    safety_floor(
        CallKind::Edit,
        &Target::Path {
            absolute: slash(path),
            relative: None,
        },
    )
}

fn guarded(paths: &Paths) {
    let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
    let _state = DaemonState::new(paths.clone(), settings, None);
    for refused in [
        "skills/s/SKILL.md",
        "agents/a.md",
        "commands/c.md",
        "mods/m/mod.json",
        "plugins/p/hooks/hooks.json",
        "config.toml",
        // A full settings layer, the connector catalogue's overrides and the personalities.
        "models.toml",
        "connectors.json",
        "personalities/p.md",
    ] {
        let path = paths.config_dir().join(refused);
        assert!(refused_write(&slash(&path)), "{refused}");
    }
    // The approval records are unsigned: a write would be an approval.
    for store in [
        "skill-installs.json",
        "plugin-installs.json",
        "mod-approvals.json",
    ] {
        let path = paths.data_dir().join(store);
        assert!(refused_write(&slash(&path)), "{store}");
    }
    // The approval stores ask, in the data folder.
    assert!(floored(&paths.data_dir().join("skill-installs.json")));
    assert!(floored(&paths.data_dir().join("plugin-installs.json")));
    // Workspaces and worktrees in the data folder are neither refused nor asked about.
    for open in ["workspaces/w/src/main.rs", "worktrees/t/src/main.rs"] {
        let path = paths.data_dir().join(open);
        assert!(!refused_write(&slash(&path)), "{open}");
        assert!(!floored(&path), "{open}");
    }
}

#[test]
fn separate_config_and_data_folders_refuse_the_extension_paths_only() {
    let home = tempfile::tempdir().unwrap();
    guarded(&Paths::under(home.path()));
}

#[test]
fn a_shared_config_and_data_folder_leaves_workspaces_and_worktrees_alone() {
    // The macOS layout: one folder is the config folder and the data folder.
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::in_one_folder(home.path());
    assert_eq!(paths.config_dir(), paths.data_dir());
    guarded(&paths);
}

#[test]
fn the_users_own_plugin_folders_are_refused_too() {
    // Plugins there skip the load-time gate, so no file tool writes into them.
    let home = tempfile::tempdir().unwrap();
    let own = tempfile::tempdir().unwrap();
    let mut settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
    settings.extensions.plugin_dirs = vec![own.path().to_path_buf()];
    let _state = DaemonState::new(Paths::under(home.path()), settings, None);
    assert!(refused_write(&slash(
        &own.path().join("p/hooks/hooks.json")
    )));
    assert!(!refused_write(&slash(&home.path().join("elsewhere/x.md"))));
}
