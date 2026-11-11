//! Sessions started on git worktrees: each environment folder inside a git repository is
//! replaced by a new worktree of that repository on a new branch, so the session's changes stay
//! apart from the checkout the user works in. Folders outside a repository are used as they are.
//!
//! Worktrees go in `<sessions.worktree_dir>/<repo name>/<branch>` when that setting is set, and
//! otherwise inside the repository in `.worktrees/<branch>`, which is added to the repository's
//! `.git/info/exclude` so it is never committed. Removing one keeps its branch.

use std::path::{Path, PathBuf};

use imperium_protocol::methods::SessionWorktree;

/// Runs git in `dir` with nothing from the repository's config able to run (hooks and
/// fsmonitor off, no network; [`crate::safe_git`]), returning its trimmed output or its error
/// message.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    crate::safe_git::run(crate::safe_git::command(dir).args(args))
        .map(|out| crate::safe_git::text(&out))
}

/// Refuses a repository whose own config names checkout filter drivers: `worktree add`
/// checks files out, which would run them in the daemon.
fn check_filters(repo: &Path) -> Result<(), String> {
    let found = git(
        repo,
        &[
            "config",
            "--local",
            "--includes",
            "--name-only",
            "--get-regexp",
            r"^filter\..+\.(smudge|process)$",
        ],
    )
    .unwrap_or_default();
    if found.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the repository's own config names checkout filters ({}), which Imperium does not \
             run; add the worktree from a terminal",
            found.lines().collect::<Vec<_>>().join(", ")
        ))
    }
}

/// The top folder of the repository `folder` is in, if it is in one.
fn repository(folder: &Path) -> Option<PathBuf> {
    let top = git(folder, &["rev-parse", "--show-toplevel"]).ok()?;
    // git prints forward slashes on Windows; compare canonical paths (written plainly, since
    // git does not take Windows' `\\?\` paths).
    let top = PathBuf::from(top).canonicalize().ok()?;
    Some(PathBuf::from(plain(&top)))
}

/// Whether `folder` looks like it is inside a git repository: it or a parent holds `.git`. A
/// cheap check for listings; starting worktrees asks git itself.
pub fn in_repository(folder: &Path) -> bool {
    folder.ancestors().any(|dir| dir.join(".git").exists())
}

/// The branch name as a single folder name.
fn folder_name(branch: &str) -> String {
    branch
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect()
}

/// A path as people write it: without Windows' `\\?\` prefix.
fn plain(path: &Path) -> String {
    let shown = path.display().to_string();
    match shown.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => shown,
    }
}

/// Adds `.worktrees/` to the repository's `info/exclude` unless it is there.
fn exclude_worktrees(repo: &Path) -> Result<(), String> {
    let common = git(repo, &["rev-parse", "--git-common-dir"])?;
    let common = if Path::new(&common).is_absolute() {
        PathBuf::from(common)
    } else {
        repo.join(common)
    };
    let info = common.join("info");
    std::fs::create_dir_all(&info).map_err(|error| error.to_string())?;
    let exclude = info.join("exclude");
    let text = std::fs::read_to_string(&exclude).unwrap_or_default();
    if text.lines().any(|line| line.trim() == ".worktrees/") {
        return Ok(());
    }
    let mut next = text;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(".worktrees/\n");
    std::fs::write(&exclude, next).map_err(|error| error.to_string())
}

/// Starts `folders` on worktrees on `branch`: each folder in a git repository gets a new
/// worktree (one per repository, shared by folders in the same one) and is replaced by the same
/// place inside it. Returns the folders to use and, for each replaced folder, its worktree. On
/// failure, worktrees already made are removed again.
pub fn create(
    folders: &[String],
    branch: &str,
    worktree_dir: Option<&Path>,
) -> Result<(Vec<String>, Vec<SessionWorktree>), String> {
    // Each repository's worktree root, once made.
    let mut roots: Vec<(PathBuf, String)> = Vec::new();
    let mut records = Vec::new();
    let mut used = Vec::new();
    for folder in folders {
        let Some(repo) = repository(Path::new(folder)) else {
            used.push(folder.clone());
            continue;
        };
        let inside = Path::new(folder)
            .canonicalize()
            .ok()
            .and_then(|canonical| {
                PathBuf::from(plain(&canonical))
                    .strip_prefix(&repo)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .unwrap_or_default();
        let root = match roots.iter().find(|(known, _)| *known == repo) {
            Some((_, root)) => root.clone(),
            None => match make(&repo, branch, worktree_dir) {
                Ok(root) => {
                    roots.push((repo.clone(), root.clone()));
                    root
                }
                Err(error) => {
                    for (repo, root) in &roots {
                        let _ = git(repo, &["worktree", "remove", "--force", root]);
                    }
                    return Err(format!("{folder}: {error}"));
                }
            },
        };
        let path = if inside.as_os_str().is_empty() {
            root
        } else {
            plain(&Path::new(&root).join(&inside))
        };
        used.push(path.clone());
        records.push(SessionWorktree {
            folder: folder.clone(),
            path,
            branch: branch.to_string(),
            repo: plain(&repo),
        });
    }
    Ok((used, records))
}
/// Makes the worktree for `repo`, returning its folder.
fn make(repo: &Path, branch: &str, worktree_dir: Option<&Path>) -> Result<String, String> {
    let name = repo
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let path = match worktree_dir {
        Some(dir) => dir.join(&name).join(folder_name(branch)),
        None => {
            exclude_worktrees(repo)?;
            repo.join(".worktrees").join(folder_name(branch))
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let shown = plain(&path);
    check_filters(repo)?;
    git(repo, &["worktree", "add", "-b", branch, "--", &shown])?;
    Ok(shown)
}

/// The folders a resumed session uses: each folder recorded with a worktree is replaced by it.
pub fn apply(folders: &[String], worktrees: &[SessionWorktree]) -> Vec<String> {
    folders
        .iter()
        .map(|folder| {
            worktrees
                .iter()
                .find(|tree| tree.folder == *folder)
                .map_or_else(|| folder.clone(), |tree| tree.path.clone())
        })
        .collect()
}

/// Removes the worktrees `records` name (one per repository, whatever is in them), keeping
/// their branches. Returns the folders removed and the failures.
fn registered_root(repo: &Path, path: &Path) -> Result<String, String> {
    let listed = git(repo, &["worktree", "list", "--porcelain"])?;
    let wanted = path
        .canonicalize()
        .map_err(|error| format!("the worktree is gone: {error}"))?;
    listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        // The first is the main checkout, which is never removed.
        .skip(1)
        .filter_map(|tree| {
            let canonical = Path::new(tree).canonicalize().ok()?;
            wanted
                .starts_with(&canonical)
                .then(|| (canonical.components().count(), plain(&canonical)))
        })
        .max()
        .map(|(_, root)| root)
        .ok_or_else(|| "not a linked worktree of its repository".to_string())
}

pub fn remove(records: &[SessionWorktree]) -> (Vec<String>, Vec<String>) {
    let mut removed: Vec<String> = Vec::new();
    let mut failed = Vec::new();
    for record in records {
        // The worktree is looked up in the repository's own list (never by asking git inside
        // the session's folder, whose `.git` the session could have rewritten): the linked
        // worktree that holds the recorded path.
        let root = match registered_root(Path::new(&record.repo), Path::new(&record.path)) {
            Ok(root) => root,
            Err(error) => {
                failed.push(format!("{}: {error}", record.path));
                continue;
            }
        };
        let same = |known: &String| {
            Path::new(known).canonicalize().ok() == Path::new(&root).canonicalize().ok()
        };
        if removed.iter().any(same) {
            continue;
        }
        match git(
            Path::new(&record.repo),
            &["worktree", "remove", "--force", "--", &root],
        ) {
            Ok(_) => removed.push(root),
            Err(error) => failed.push(format!("{root}: {error}")),
        }
    }
    (removed, failed)
}
