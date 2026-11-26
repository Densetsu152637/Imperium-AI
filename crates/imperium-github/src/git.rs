//! Git operations for auto-fix worktrees, through the `git` CLI so the user's credentials and
//! configuration apply. Nothing here force-pushes, rewrites published history, or touches the
//! user's own checkout beyond adding a worktree.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Git's own variables from the daemon's surroundings that would point a command at another
/// repository, index or program; they are dropped.
const DROPPED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// An empty folder made for this process (a fresh private temporary directory), where
/// `core.hooksPath` points: no hook of the repository runs.
fn no_hooks() -> Result<&'static Path, String> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("imperium-no-hooks-")
            .tempdir()
            .ok()
            .map(tempfile::TempDir::keep)
    })
    .as_deref()
    .ok_or_else(|| "could not make an empty hooks folder, so git was not run".to_string())
}

/// Runs git in `dir` with nothing from the repository able to run a program: its hooks are
/// pointed at an empty folder (a session may have written a hook file, and a repository's own
/// `core.hooksPath` such as `.husky` names files in the worktree), fsmonitor is off, submodules
/// are never entered, and git's variables from the surroundings are dropped. Config keys that
/// name programs (filters, signing programs, transports) are refused by the caller before a
/// commit or push.
async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let no_hooks = no_hooks()?;
    let mut command = tokio::process::Command::new("git");
    for name in DROPPED_ENV {
        command.env_remove(name);
    }
    let output = command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "4")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", no_hooks)
        .env("GIT_CONFIG_KEY_1", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_1", "false")
        .env("GIT_CONFIG_KEY_2", "submodule.recurse")
        .env("GIT_CONFIG_VALUE_2", "false")
        .env("GIT_CONFIG_KEY_3", "push.recurseSubmodules")
        .env("GIT_CONFIG_VALUE_3", "no")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Makes `worktree` a checkout of `origin/<branch>` as of now, on a local branch named
/// `local_branch`. An existing auto-fix worktree is reset to the remote branch (it is
/// Imperium's own, so discarding a previous unpushed attempt is intended).
pub async fn prepare_worktree(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    local_branch: &str,
) -> Result<(), String> {
    git(repo, &["fetch", "origin", branch]).await?;
    let remote = format!("origin/{branch}");
    if worktree.join(".git").exists() {
        git(worktree, &["reset", "--hard", &remote]).await?;
        git(worktree, &["clean", "-fd"]).await?;
        return Ok(());
    }
    if let Some(parent) = worktree.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let path = worktree.display().to_string();
    git(
        repo,
        &["worktree", "add", "-B", local_branch, &path, &remote],
    )
    .await?;
    Ok(())
}

/// Commits every change in the worktree; false when there was nothing to commit.
pub async fn commit_all(worktree: &Path, message: &str) -> Result<bool, String> {
    git(worktree, &["add", "-A"]).await?;
    if git(
        worktree,
        &[
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
        ],
    )
    .await
    .is_ok()
    {
        return Ok(false);
    }
    git(worktree, &["commit", "--no-verify", "-m", message]).await?;
    Ok(true)
}

/// Every address a push to `origin` goes to, as git resolves them (`git remote get-url --push
/// --all origin`): `pushurl` (a second one is pushed to as well) and the `url.*.insteadOf` and
/// `pushInsteadOf` rewrites included, so these are where the push really goes.
pub async fn push_urls(dir: &Path) -> Result<Vec<String>, String> {
    let out = git(dir, &["remote", "get-url", "--push", "--all", "origin"]).await?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// Pushes the worktree's commit to `branch` on origin. A plain push: if the branch moved
/// meanwhile, the push is rejected rather than overwriting it.
pub async fn push(worktree: &Path, branch: &str) -> Result<(), String> {
    let target = format!("HEAD:refs/heads/{branch}");
    git(
        worktree,
        &[
            "push",
            "--no-verify",
            "--recurse-submodules=no",
            "origin",
            &target,
        ],
    )
    .await
    .map(|_| ())
}

/// Pushes exactly the commit `sha` to `branch` on origin (a plain push, like [`push`]), so what
/// was approved is what goes, whatever `HEAD` is by then.
pub async fn push_commit(worktree: &Path, sha: &str, branch: &str) -> Result<(), String> {
    let target = format!("{sha}:refs/heads/{branch}");
    git(
        worktree,
        &[
            "push",
            "--no-verify",
            "--recurse-submodules=no",
            "origin",
            &target,
        ],
    )
    .await
    .map(|_| ())
}

/// What a push of the worktree's `HEAD` would add to `origin/<branch>`: each changed path with
/// its `git diff --name-status` letter (a rename counts as a delete and an add).
pub async fn changes_since_origin(
    worktree: &Path,
    branch: &str,
) -> Result<Vec<(String, String)>, String> {
    changes_up_to(worktree, branch, "HEAD").await
}

/// What a push of commit `rev` would add to `origin/<branch>`, as [`changes_since_origin`].
pub async fn changes_up_to(
    worktree: &Path,
    branch: &str,
    rev: &str,
) -> Result<Vec<(String, String)>, String> {
    let base = format!("origin/{branch}");
    let out = git(
        worktree,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "--no-renames",
            "-z",
            &base,
            rev,
            "--",
        ],
    )
    .await?;
    let mut fields = out.split('\0').filter(|field| !field.is_empty());
    let mut changes = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        changes.push((status.to_string(), path.to_string()));
    }
    Ok(changes)
}

/// How many commits a push of the worktree's `HEAD` would add to `origin/<branch>`.
pub async fn commits_ahead(worktree: &Path, branch: &str) -> Result<u64, String> {
    commits_ahead_of(worktree, branch, "HEAD").await
}

/// How many commits a push of commit `rev` would add to `origin/<branch>`.
pub async fn commits_ahead_of(worktree: &Path, branch: &str, rev: &str) -> Result<u64, String> {
    let range = format!("origin/{branch}..{rev}");
    git(worktree, &["rev-list", "--count", &range, "--"])
        .await?
        .parse()
        .map_err(|error| format!("unexpected commit count: {error}"))
}

/// The commit the worktree is at.
pub async fn head(worktree: &Path) -> Result<String, String> {
    git(worktree, &["rev-parse", "HEAD"]).await
}

/// Fetches `origin/<branch>`, so a later comparison is with the remote branch as it is now.
pub async fn fetch(dir: &Path, branch: &str) -> Result<(), String> {
    git(dir, &["fetch", "origin", branch]).await.map(|_| ())
}

/// The branch checked out in `dir` (`HEAD` when it is detached).
pub async fn current_branch(dir: &Path) -> Result<String, String> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await
}

/// Whether `origin/<branch>` is `HEAD` or one of its ancestors, so a push would add commits to
/// the remote branch and never replace any. False when it is not (behind, diverged) or cannot
/// be told.
pub async fn descends_from_origin(dir: &Path, branch: &str) -> bool {
    let base = format!("origin/{branch}");
    git(dir, &["merge-base", "--is-ancestor", &base, "HEAD"])
        .await
        .is_ok()
}

/// Whether `dir` has no uncommitted change and no untracked file.
pub async fn is_clean(dir: &Path) -> Result<bool, String> {
    git(dir, &["status", "--porcelain"])
        .await
        .map(|out| out.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo_with_origin(url: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"]);
        run(dir.path(), &["remote", "add", "origin", url]);
        dir
    }

    #[tokio::test]
    async fn the_push_address_is_the_origin_url_by_default() {
        let dir = repo_with_origin("https://github.com/owner/repo.git");
        assert_eq!(
            push_urls(dir.path()).await.unwrap(),
            ["https://github.com/owner/repo.git"]
        );
    }

    #[tokio::test]
    async fn a_pushurl_redirect_is_the_push_address() {
        let dir = repo_with_origin("https://github.com/owner/repo.git");
        run(
            dir.path(),
            &[
                "config",
                "remote.origin.pushurl",
                "https://example.com/other/place.git",
            ],
        );
        assert_eq!(
            push_urls(dir.path()).await.unwrap(),
            ["https://example.com/other/place.git"]
        );
    }

    #[tokio::test]
    async fn every_pushurl_is_a_push_address() {
        let dir = repo_with_origin("https://github.com/owner/repo.git");
        for url in [
            "https://github.com/owner/repo.git",
            "https://example.com/other/place.git",
        ] {
            run(
                dir.path(),
                &["config", "--add", "remote.origin.pushurl", url],
            );
        }
        assert_eq!(
            push_urls(dir.path()).await.unwrap(),
            [
                "https://github.com/owner/repo.git",
                "https://example.com/other/place.git"
            ]
        );
    }

    #[tokio::test]
    async fn a_push_insteadof_redirect_is_the_push_address() {
        let dir = repo_with_origin("https://github.com/owner/repo.git");
        run(
            dir.path(),
            &[
                "config",
                "url.https://example.com/other/.pushInsteadOf",
                "https://github.com/owner/",
            ],
        );
        assert_eq!(
            push_urls(dir.path()).await.unwrap(),
            ["https://example.com/other/repo.git"]
        );
    }
}
