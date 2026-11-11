//! Sessions started on git worktrees: a repository folder is replaced by a new worktree on a new
//! branch (inside the repository, excluded from git, or under `sessions.worktree_dir`), a folder
//! outside git is used as it is, the session keeps its worktree after a restart, and deleting it
//! can remove the worktree while keeping the branch.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use serde_json::{Value, json};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("README.md"), "hello\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    repo
}

fn daemon(home: &Path, worktree_dir: Option<&Path>) -> Arc<DaemonState> {
    let mut config = String::from(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n",
    );
    if let Some(dir) = worktree_dir {
        let dir = dir.display().to_string().replace('\\', "/");
        config.push_str(&format!("[sessions]\nworktree_dir = \"{dir}\"\n"));
    }
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    DaemonState::new(imperium_config::Paths::under(home), settings, None)
}

fn call(connection: &mut Connection, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let Some(Response { outcome, .. }) = connection.handle_text(&frame.to_string()).response else {
        panic!("no reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {error:?}"),
    }
}

fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    call(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    connection
}

fn same(a: &str, b: &Path) -> bool {
    Path::new(a).canonicalize().unwrap() == b.canonicalize().unwrap()
}

#[tokio::test]
async fn a_session_works_on_a_new_worktree_inside_the_repository_and_keeps_it() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let plain = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), None);
    let mut connection = connect(&state);
    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Both", "folders": [repo.path(), plain.path()]}),
    );
    let created = call(
        &mut connection,
        "session.create",
        json!({"environment_id": env["id"], "worktree": true}),
    );
    let id = created["session_id"].as_str().unwrap().to_string();
    let branch = format!("imperium/{}", &id[..8]);
    let trees = created["worktrees"].as_array().unwrap();
    assert_eq!(trees.len(), 1, "only the repository gets a worktree");
    assert_eq!(trees[0]["branch"], branch.as_str());
    let tree = repo
        .path()
        .join(".worktrees")
        .join(branch.replace('/', "-"));
    assert!(tree.join("README.md").is_file());
    assert!(same(created["folders"][0].as_str().unwrap(), &tree));
    assert!(
        same(created["folders"][1].as_str().unwrap(), plain.path()),
        "a folder outside git is used as it is"
    );
    let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|line| line == ".worktrees/"));
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]),
        "",
        "the worktree folder is not seen by git"
    );
    assert_eq!(git(&tree, &["branch", "--show-current"]), branch);

    // After a restart the session is back on its worktree.
    drop(connection);
    drop(state);
    let state = daemon(home.path(), None);
    assert_eq!(state.resume_sessions(), 1);
    let session = state.session(&id).unwrap();
    let summary = state.summary(&session);
    assert!(same(&summary.folders[0], &tree));
    assert_eq!(summary.worktrees.len(), 1);

    // Deleting it can remove the worktree; the branch stays.
    let mut connection = connect(&state);
    let deleted = call(
        &mut connection,
        "session.delete",
        json!({"session_id": id, "remove_worktrees": true}),
    );
    assert_eq!(deleted["removed_worktrees"].as_array().unwrap().len(), 1);
    assert!(!tree.exists());
    assert!(state.session(&id).is_none());
    assert!(git(repo.path(), &["branch", "--list", &branch]).contains(&branch));
    drop(state);
    assert_eq!(daemon(home.path(), None).resume_sessions(), 0);
}

#[tokio::test]
async fn worktrees_go_under_the_configured_folder_when_set() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let elsewhere = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), Some(elsewhere.path()));
    let mut connection = connect(&state);
    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Repo", "folders": [repo.path()]}),
    );
    let created = call(
        &mut connection,
        "session.create",
        json!({"environment_id": env["id"], "worktree": true, "worktree_branch": "feature/x"}),
    );
    let name = repo.path().canonicalize().unwrap();
    let name = name.file_name().unwrap();
    let tree = elsewhere.path().join(name).join("feature-x");
    assert!(same(created["folders"][0].as_str().unwrap(), &tree));
    assert_eq!(git(&tree, &["branch", "--show-current"]), "feature/x");
    assert!(
        !repo.path().join(".worktrees").exists(),
        "nothing is made inside the repository"
    );
}

/// A call's error message (it must fail).
fn call_error(connection: &mut Connection, method: &str, params: Value) -> String {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let Some(Response { outcome, .. }) = connection.handle_text(&frame.to_string()).response else {
        panic!("no reply to {method}");
    };
    match outcome {
        Outcome::Error(error) => error.message,
        Outcome::Result(value) => panic!("{method} should fail, got {value}"),
    }
}

#[tokio::test]
async fn making_a_worktree_runs_nothing_from_the_repositorys_config() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let marker = home.path().join("hook-ran");
    let hooks = repo.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("post-checkout"),
        format!(
            "#!/bin/sh\ntouch '{}'\n",
            marker.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            hooks.join("post-checkout"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let state = daemon(home.path(), None);
    let mut connection = connect(&state);
    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Repo", "folders": [repo.path()]}),
    );
    call(
        &mut connection,
        "session.create",
        json!({"environment_id": env["id"], "worktree": true}),
    );
    assert!(!marker.exists(), "the post-checkout hook did not run");

    // A checkout filter in the repository's own config is refused, not run.
    git(
        repo.path(),
        &["config", "filter.evil.smudge", "touch marker"],
    );
    let refused = call_error(
        &mut connection,
        "session.create",
        json!({"environment_id": env["id"], "worktree": true, "worktree_branch": "other"}),
    );
    assert!(refused.contains("filter.evil.smudge"), "{refused}");
}

#[tokio::test]
async fn deleting_a_session_removes_only_its_recorded_worktree() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let state = daemon(home.path(), None);
    let mut connection = connect(&state);
    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Repo", "folders": [repo.path()]}),
    );
    let created = call(
        &mut connection,
        "session.create",
        json!({"environment_id": env["id"], "worktree": true}),
    );
    let id = created["session_id"].as_str().unwrap().to_string();
    let mine = std::path::PathBuf::from(created["folders"][0].as_str().unwrap());
    // Someone else's worktree of the same repository.
    let others = tempfile::tempdir().unwrap();
    let theirs = others.path().join("theirs");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "theirs",
            &theirs.display().to_string(),
        ],
    );
    std::fs::write(theirs.join("work.txt"), "uncommitted\n").unwrap();
    // The session points its own `.git` at a git folder that names their worktree.
    let fake = mine.join("fake");
    git(
        &mine,
        &["init", "-q", "--bare", &fake.display().to_string()],
    );
    git(&fake, &["config", "core.bare", "false"]);
    git(
        &fake,
        &["config", "core.worktree", &theirs.display().to_string()],
    );
    std::fs::write(mine.join(".git"), format!("gitdir: {}\n", fake.display())).unwrap();

    call(
        &mut connection,
        "session.delete",
        json!({"session_id": id, "remove_worktrees": true}),
    );
    assert_eq!(
        std::fs::read_to_string(theirs.join("work.txt")).unwrap(),
        "uncommitted\n",
        "their worktree is untouched"
    );
}
