//! `imperium-core --version` names the build, and the handshake reports the same string.

use std::process::Command;

#[test]
fn version_shows_the_release_and_the_commit() {
    let output = Command::new(env!("CARGO_BIN_EXE_imperium-core"))
        .arg("--version")
        .output()
        .expect("the imperium-core binary runs");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    assert_eq!(
        line,
        format!("imperium-core {}", imperium_core::BUILD_VERSION)
    );
    let (_, commit) = line
        .strip_suffix(')')
        .and_then(|rest| rest.split_once(" ("))
        .unwrap_or_else(|| panic!("{line:?} has no parenthesised commit"));
    assert!(
        commit == "unknown" || (commit.len() >= 7 && commit.chars().all(|c| c.is_ascii_hexdigit())),
        "{line:?}"
    );
}
