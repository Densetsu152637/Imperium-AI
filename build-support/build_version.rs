//! The build's version string, shared by the build scripts of `imperium-cli` and `imperium-core`
//! (`include!`d from each `build.rs`; it is not a crate, so no dependency is added).
//!
//! It sets `IMPERIUM_BUILD_VERSION_STRING`, which the crate reads with `env!`:
//!
//! - a release build (CI sets `IMPERIUM_BUILD_VERSION` to the tag without its `v`, and
//!   `IMPERIUM_BUILD_COMMIT` to the commit): `0.2.281-experimental (223a0d02)`;
//! - any other build: `<crate version>-dev (<short commit, or "unknown">)`, the commit read from
//!   `git rev-parse` when the checkout has one.

use std::process::Command;

/// The characters of the commit shown.
const SHORT_COMMIT: usize = 8;

fn emit_build_version() {
    println!("cargo:rerun-if-env-changed=IMPERIUM_BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=IMPERIUM_BUILD_COMMIT");
    let release = non_empty_env("IMPERIUM_BUILD_VERSION")
        .map(|version| version.trim_start_matches('v').to_string())
        .filter(|version| !version.is_empty());
    let commit = non_empty_env("IMPERIUM_BUILD_COMMIT")
        .or_else(git_commit)
        .map(|commit| commit.chars().take(SHORT_COMMIT).collect::<String>())
        .unwrap_or_else(|| "unknown".into());
    let version = release.unwrap_or_else(|| {
        format!(
            "{}-dev",
            std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
        )
    });
    println!("cargo:rustc-env=IMPERIUM_BUILD_VERSION_STRING={version} ({commit})");
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The checkout's head, when `git` and a repository are there; a build outside one (a source
/// archive, a vendored copy) has none, and that is not an error.
fn git_commit() -> Option<String> {
    watch_git_head();
    git(&["rev-parse", "--short=8", "HEAD"])
}

/// Rebuild when the checked-out commit moves, so a development build does not keep a stale one.
fn watch_git_head() {
    for path in [
        git(&["rev-parse", "--git-path", "HEAD"]),
        git(&["symbolic-ref", "-q", "HEAD"])
            .and_then(|reference| git(&["rev-parse", "--git-path", &reference])),
    ]
    .into_iter()
    .flatten()
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
