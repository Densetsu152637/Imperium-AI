//! `imperium pack` against a local Git repository standing in for the pack's source: nothing is
//! pinned until the user accepts, loosening needs a separate yes, and sources come only from
//! the user's own config folder.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .status()
        .expect("git is installed");
    assert!(status.success(), "git {args:?}");
}

/// Writes `files` into the repository and commits them.
fn commit(repo: &Path, files: &[(&str, &str)], message: &str) {
    for (path, text) in files {
        let target = repo.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, text).unwrap();
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}", text.trim_start_matches('/'))
}

struct Env {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        commit(
            repo.path(),
            &[
                (
                    "AGENTS.md",
                    "# Agents\n\n- Bounded work: [Leaf](workflows/AGENTS.leaf.md)\n",
                ),
                ("workflows/AGENTS.leaf.md", "# Leaf\n\nDo the work.\n"),
                ("guides/git.md", "# Git\n\nAsk before every push.\n"),
            ],
            "v1",
        );
        Env {
            home: tempfile::tempdir().unwrap(),
            repo,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_imperium"))
            .env("IMPERIUM_HOME", self.home.path())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("the imperium binary runs")
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "imperium {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Runs a command that must fail and returns what it printed to standard error.
    fn fails(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(!output.status.success(), "imperium {args:?} succeeded");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn add_source(&self) {
        let url = file_url(self.repo.path());
        self.ok(&["pack", "add-source", "acw", &url]);
    }

    fn pins(&self) -> String {
        std::fs::read_to_string(self.home.path().join("data/packs/pins.toml")).unwrap_or_default()
    }
}

/// The hash `check` printed for the new version.
fn new_hash(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("new      sha256:"))
        .expect("the output names the new hash")
        .trim()
        .to_string()
}

#[test]
fn an_update_is_shown_as_a_diff_and_pinned_only_on_accept() {
    let env = Env::new();
    env.add_source();

    // The first fetch stages the pack; nothing is pinned or loaded.
    let first = env.ok(&["pack", "check", "acw"]);
    assert!(first.contains("First version"), "{first}");
    assert!(env.pins().is_empty(), "nothing is pinned by a check");
    let hash = new_hash(&first);

    // Accepting needs the hash the user read; no terminal means no prompt.
    assert!(env.fails(&["pack", "accept", "acw"]).contains("terminal"));
    let wrong = env.fails(&["pack", "accept", "acw", "--hash", "0123456789abcdef"]);
    assert!(wrong.contains("not the pending version"), "{wrong}");
    assert!(env.pins().is_empty());

    env.ok(&["pack", "accept", "acw", "--hash", &hash[..16]]);
    assert!(env.pins().contains(&hash));
    let listed = env.ok(&["pack", "list"]);
    assert!(
        listed.contains(&format!("sha256:{}", &hash[..12])),
        "{listed}"
    );
    assert!(!listed.contains("pending"), "{listed}");

    // A pinned pack is served to sessions like a listed folder.
    let project = tempfile::tempdir().unwrap();
    let project_path = project.path().to_string_lossy().into_owned();
    let manifest = env.ok(&[
        "workflow",
        "resolve",
        "--role",
        "leaf",
        "--project",
        &project_path,
        "do the work",
    ]);
    assert!(
        manifest.contains("acw:workflows/AGENTS.leaf.md"),
        "{manifest}"
    );

    // Nothing new upstream: nothing pending.
    assert!(env.ok(&["pack", "check", "acw"]).contains("up to date"));

    // A harmless change and a new personality are shown in plain words and applied on accept.
    commit(
        env.repo.path(),
        &[
            (
                "guides/git.md",
                "# Git\n\nAsk before every push.\nKeep commits small.\n",
            ),
            ("personalities/calm.md", "# Calm\n\nSpeak gently.\n"),
        ],
        "v2",
    );
    let second = env.ok(&["pack", "check", "acw"]);
    assert!(
        second.contains("2 files differ: 1 changed, 1 added, 0 removed."),
        "{second}"
    );
    assert!(second.contains("Rules and workflows"), "{second}");
    assert!(second.contains("Prompts and personalities"), "{second}");
    assert!(!second.contains("!!"), "{second}");
    let full = env.ok(&["pack", "diff", "acw", "--full"]);
    assert!(full.contains("+ Keep commits small."), "{full}");
    assert!(env.pins().contains(&hash), "still the first version");
    let second_hash = new_hash(&second);
    env.ok(&["pack", "accept", "acw", "--hash", &second_hash[..12]]);
    assert!(env.pins().contains(&second_hash) && !env.pins().contains(&hash));
}

#[test]
fn an_update_that_loosens_things_needs_a_separate_explicit_yes() {
    let env = Env::new();
    env.add_source();
    let hash = new_hash(&env.ok(&["pack", "check", "acw"]));
    env.ok(&["pack", "accept", "acw", "--hash", &hash]);

    commit(
        env.repo.path(),
        &[(
            "guides/git.md",
            "# Git\n\nPush straight to main without asking.\n",
        )],
        "v2",
    );
    let output = env.ok(&["pack", "check", "acw"]);
    assert!(output.contains("!! These changes may loosen"), "{output}");
    assert!(output.contains("without asking"), "{output}");
    assert!(output.contains("Ask before every push"), "{output}");
    let loose = new_hash(&output);

    let refused = env.fails(&["pack", "accept", "acw", "--hash", &loose]);
    assert!(refused.contains("--allow-loosening"), "{refused}");
    assert!(env.pins().contains(&hash), "the old version stays pinned");

    env.ok(&[
        "pack",
        "accept",
        "acw",
        "--hash",
        &loose,
        "--allow-loosening",
    ]);
    assert!(env.pins().contains(&loose));
}

#[test]
fn a_rejected_update_changes_nothing() {
    let env = Env::new();
    env.add_source();
    let hash = new_hash(&env.ok(&["pack", "check", "acw"]));
    env.ok(&["pack", "accept", "acw", "--hash", &hash]);
    commit(env.repo.path(), &[("guides/new.md", "# New\n")], "v2");
    env.ok(&["pack", "check", "acw"]);
    assert!(env.ok(&["pack", "list"]).contains("pending"));
    env.ok(&["pack", "reject", "acw"]);
    assert!(!env.ok(&["pack", "list"]).contains("pending"));
    assert!(env.pins().contains(&hash));
    assert!(
        env.fails(&["pack", "diff", "acw"])
            .contains("no pending update")
    );
}

#[test]
fn pinned_files_that_change_on_disk_are_no_longer_loaded() {
    let env = Env::new();
    env.add_source();
    let hash = new_hash(&env.ok(&["pack", "check", "acw"]));
    env.ok(&["pack", "accept", "acw", "--hash", &hash]);
    let file = env
        .home
        .path()
        .join("data/packs/acw")
        .join(&hash)
        .join("guides/git.md");
    std::fs::write(file, "# Git\n\nPush whenever.\n").unwrap();
    assert!(env.ok(&["pack", "list"]).contains("no longer match"));
    // The next check fetches it again and shows it as an update from nothing pinned.
    let listed = env.ok(&["pack", "check", "acw"]);
    assert!(listed.contains("update is waiting"), "{listed}");
}

#[test]
fn sources_come_only_from_the_users_config_folder() {
    let env = Env::new();
    let url = file_url(env.repo.path());
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".imperium")).unwrap();
    std::fs::write(
        project.path().join(".imperium/pack-sources.toml"),
        format!("[sources.sneaky]\nurl = \"{url}\"\n"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_imperium"))
        .env("IMPERIUM_HOME", env.home.path())
        .env(
            "IMPERIUM_WORKFLOW__PACK_SOURCES",
            format!("[\"sneaky={url}\"]"),
        )
        .args(["pack", "check"])
        .current_dir(project.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("No pack sources"), "{text}");
    assert!(env.pins().is_empty());
}

#[test]
fn unsafe_source_addresses_are_refused() {
    let env = Env::new();
    for url in [
        "http://example.com/pack.git",
        "ext::sh -c touch /tmp/pwned",
        "https://user:token@example.com/pack.git",
    ] {
        let refused = env.fails(&["pack", "add-source", "bad", url]);
        assert!(
            refused.contains("cannot be a pack source"),
            "{url}: {refused}"
        );
    }
    assert!(env.ok(&["pack", "list"]).contains("No pack sources"));
}
