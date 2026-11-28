//! `imperium --version` names the build: the release tag and the short commit, so an eval run
//! can record which build it ran on.

use std::process::Command;

/// Whether `text` is a commit (`223a0d02`) or the `unknown` a build outside a repository shows.
fn is_commit_or_unknown(text: &str) -> bool {
    text == "unknown" || (text.len() >= 7 && text.chars().all(|c| c.is_ascii_hexdigit()))
}

#[test]
fn version_shows_the_release_and_the_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_imperium"))
        .arg("--version")
        .output()
        .expect("the imperium binary runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    let rest = line
        .strip_prefix("imperium ")
        .unwrap_or_else(|| panic!("{line:?} does not start with the program name"));
    let (version, commit) = rest
        .strip_suffix(')')
        .and_then(|rest| rest.split_once(" ("))
        .unwrap_or_else(|| panic!("{line:?} has no parenthesised commit"));
    assert!(is_commit_or_unknown(commit), "{line:?}");
    // A release build carries the tag CI gave it (without its `v`); any other is a `-dev` build.
    match option_env!("IMPERIUM_BUILD_VERSION").map(str::trim) {
        Some(tag) if !tag.is_empty() => assert_eq!(version, tag.trim_start_matches('v')),
        _ => assert!(version.ends_with("-dev"), "{line:?}"),
    }
}
