//! `imperium workflow resolve` as it behaves before it is aligned with the daemon's pack loader
//! (docs/plans/cleanup.md, decision 1): packs come from `workflow.packs` or `--pack`, a pack
//! that cannot be read fails the command, and neither the `workflow.enabled` switch nor a
//! bundled default pack plays a part. The commit that aligns the command replaces the tests
//! marked "today" with its new behaviour; the others stay.

use std::path::Path;
use std::process::{Command, Output, Stdio};

struct Env {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    folders: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let env = Env {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
            folders: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(env.home.path().join("config")).unwrap();
        env
    }

    /// A readable pack folder called `name`.
    fn pack(&self, name: &str) -> std::path::PathBuf {
        let dir = self.folders.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Index\n").unwrap();
        dir
    }

    fn configure(&self, toml: &str) {
        std::fs::write(self.home.path().join("config/config.toml"), toml).unwrap();
    }

    fn resolve(&self, extra: &[&Path]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_imperium"));
        command
            .env("IMPERIUM_HOME", self.home.path())
            .args(["workflow", "resolve", "fix the bug", "--project"])
            .arg(self.project.path());
        for pack in extra {
            command.arg("--pack").arg(pack);
        }
        command
            .stdin(Stdio::null())
            .output()
            .expect("the imperium binary runs")
    }
}

fn listing(packs: &[&Path]) -> String {
    let quoted: Vec<String> = packs
        .iter()
        .map(|pack| format!("{:?}", pack.display().to_string()))
        .collect();
    format!("[workflow]\npacks = [{}]\n", quoted.join(", "))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_listed_pack_is_resolved_with_its_entry_index() {
    let env = Env::new();
    let pack = env.pack("mine");
    env.configure(&listing(&[&pack]));
    let output = env.resolve(&[]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    assert!(
        printed.starts_with("<context-manifest role=\"root\">"),
        "{printed}"
    );
    assert!(
        printed.contains("- mine:AGENTS.md (required: entry index"),
        "{printed}"
    );
}

#[test]
fn an_explicit_pack_replaces_the_configured_ones_and_the_config_is_not_read() {
    let env = Env::new();
    let broken = env.folders.path().join("broken");
    env.configure(&listing(&[&broken]));
    let wanted = env.pack("wanted");
    let output = env.resolve(&[&wanted]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    assert!(
        printed.contains("- wanted:AGENTS.md (required: entry index"),
        "{printed}"
    );
    assert!(!printed.contains("broken"), "{printed}");
}

#[test]
fn today_a_pack_that_cannot_be_read_fails_the_command() {
    let env = Env::new();
    let good = env.pack("good");
    let missing = env.folders.path().join("missing");
    env.configure(&listing(&[&missing, &good]));
    let output = env.resolve(&[]);
    assert!(!output.status.success());
    assert!(text(&output.stdout).is_empty(), "{}", text(&output.stdout));
    assert!(
        text(&output.stderr).contains("workflow pack"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn today_no_bundled_pack_is_used_or_named() {
    let env = Env::new();
    let output = env.resolve(&[]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    // No pack lines at all, and no line naming a bundled pack.
    assert!(!printed.contains("entry index"), "{printed}");
    assert!(!printed.to_lowercase().contains("bundled"), "{printed}");
}

#[test]
fn today_the_workflow_switch_does_not_stop_the_command() {
    let env = Env::new();
    let pack = env.pack("mine");
    let mut config = listing(&[&pack]);
    config.push_str("enabled = false\n");
    env.configure(&config);
    let output = env.resolve(&[]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).contains("- mine:AGENTS.md"),
        "{}",
        text(&output.stdout)
    );
}
