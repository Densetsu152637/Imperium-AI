//! Git operations for auto-fix worktrees, through the `git` CLI so the user's credentials and
//! configuration apply. Nothing here force-pushes, rewrites published history, or touches the
//! user's own checkout beyond adding a worktree.

use std::path::Path;

async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
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
    if git(worktree, &["diff", "--cached", "--quiet"])
        .await
        .is_ok()
    {
        return Ok(false);
    }
    git(worktree, &["commit", "-m", message]).await?;
    Ok(true)
}

/// Pushes the worktree's commit to `branch` on origin. A plain push: if the branch moved
/// meanwhile, the push is rejected rather than overwriting it.
pub async fn push(worktree: &Path, branch: &str) -> Result<(), String> {
    let target = format!("HEAD:refs/heads/{branch}");
    git(worktree, &["push", "origin", &target])
        .await
        .map(|_| ())
}

/// The commit the worktree is at.
pub async fn head(worktree: &Path) -> Result<String, String> {
    git(worktree, &["rev-parse", "HEAD"]).await
}
