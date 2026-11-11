//! A project's git worktrees, open to a session along with the project folder.
//!
//! For each local folder inside a git repository, the repository's worktrees (the ones
//! `git worktree list` shows: the main checkout and every linked worktree) are discovered from
//! the repository's own records, never guessed from folder names. A folder below the top of its
//! repository opens the same subfolder of each worktree.
//!
//! Discovery reads git's files directly rather than running git, so nothing in a repository's
//! configuration runs, and it trusts nothing a session could forge from inside its folders:
//!
//! - The repository (its common git folder) is pinned when the workspace is made. A session
//!   that rewrites its folder's `.git` file afterwards cannot point discovery elsewhere.
//! - A linked worktree counts only when its own `.git` file points back at its entry in the
//!   pinned repository's `worktrees/` folder, and that entry's `gitdir` points at the file. A
//!   session can add entries to a repository inside its folders, but cannot write the `.git`
//!   file in a folder it could not already write, so it cannot open anything new that way.
//! - A linked worktree's own `.git` file is checked the same way before its repository is
//!   pinned, so a forged one opens nothing.
//!
//! The list is read again when a path falls outside every folder and grant, so worktrees added
//! while a session runs (by the agent with `git worktree add`, or by the user) are covered
//! without restarting it, and removed ones drop out.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// The worktrees of the repositories a workspace's folders are in.
#[derive(Debug)]
pub(crate) struct ProjectWorktrees {
    projects: Vec<Project>,
    /// The worktree folders found by the last look, canonical.
    found: Mutex<Vec<PathBuf>>,
}

impl PartialEq for ProjectWorktrees {
    fn eq(&self, other: &Self) -> bool {
        self.projects == other.projects
    }
}

impl Eq for ProjectWorktrees {}

/// One folder's repository.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Project {
    /// The repository's common git folder (`<main checkout>/.git`), canonical.
    common: PathBuf,
    /// Where the folder is inside its worktree (empty at the top).
    inside: PathBuf,
}

impl ProjectWorktrees {
    /// The repositories `folders` are in (folders outside git are skipped). `None` when none
    /// is in a repository.
    pub(crate) fn discover(folders: &[&Path]) -> Option<Self> {
        let mut projects: Vec<Project> = Vec::new();
        for folder in folders {
            if let Some(project) = project_of(folder)
                && !projects.contains(&project)
            {
                projects.push(project);
            }
        }
        if projects.is_empty() {
            return None;
        }
        let found = ProjectWorktrees {
            projects,
            found: Mutex::new(Vec::new()),
        };
        found.refresh();
        Some(found)
    }

    /// The worktree folders found by the last look.
    pub(crate) fn known(&self) -> Vec<PathBuf> {
        self.found
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Looks again; returns whether the list changed.
    pub(crate) fn refresh(&self) -> bool {
        let mut now: Vec<PathBuf> = Vec::new();
        for project in &self.projects {
            for tree in worktrees(&project.common) {
                let folder = if project.inside.as_os_str().is_empty() {
                    tree
                } else {
                    // A link at the subfolder (committed on that branch) must not lead out
                    // of the worktree: `tree` is canonical, so a target outside it is refused.
                    match tree.join(&project.inside).canonicalize() {
                        Ok(folder) if folder.is_dir() && folder.starts_with(&tree) => folder,
                        _ => continue,
                    }
                };
                if !now.contains(&folder) {
                    now.push(folder);
                }
            }
        }
        let mut found = self
            .found
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = *found != now;
        *found = now;
        changed
    }
}

/// The repository `folder` is in, read from the nearest `.git` above it.
fn project_of(folder: &Path) -> Option<Project> {
    let folder = folder.canonicalize().ok()?;
    let top = folder
        .ancestors()
        .find(|dir| dir.join(".git").exists())?
        .to_path_buf();
    let dot_git = top.join(".git");
    let common = if dot_git.is_dir() {
        dot_git.canonicalize().ok()?
    } else {
        // A linked worktree: its `.git` file names its entry in the repository's
        // `worktrees/`, which must name it back.
        let entry = gitdir_of(&dot_git)?;
        if !entry_points_at(&entry, &dot_git) {
            return None;
        }
        common_of(&entry)?
    };
    let inside = folder.strip_prefix(&top).ok()?.to_path_buf();
    if inside
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    Some(Project { common, inside })
}

/// The worktrees of the repository whose common git folder is `common`: the main checkout,
/// then each linked worktree that checks out, canonical.
fn worktrees(common: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    // The main checkout holds the common folder as its `.git` (a bare repository has none).
    if common.file_name().is_some_and(|name| name == ".git")
        && let Some(main) = common.parent()
        && main
            .join(".git")
            .canonicalize()
            .is_ok_and(|dot_git| dot_git == common)
        && let Ok(main) = main.canonicalize()
    {
        found.push(main);
    }
    let Ok(entries) = std::fs::read_dir(common.join("worktrees")) else {
        return found;
    };
    let mut entries: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for entry in entries {
        let Some(dot_git) = read_path(&entry.join("gitdir"), &entry) else {
            continue;
        };
        // The worktree's own `.git` must be a file naming this entry.
        if !dot_git.is_file() {
            continue;
        }
        let Some(named) = gitdir_of(&dot_git) else {
            continue;
        };
        if named.canonicalize().ok() != entry.canonicalize().ok() {
            continue;
        }
        if let Some(tree) = dot_git.parent().and_then(|tree| tree.canonicalize().ok())
            && tree.is_dir()
            && !found.contains(&tree)
        {
            found.push(tree);
        }
    }
    found
}

/// The git folder a `.git` file names (`gitdir: <path>`, relative to the file's folder).
fn gitdir_of(dot_git: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let line = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    let path = absolute(line, dot_git.parent()?);
    path.canonicalize().ok()
}

/// Whether the worktree entry `entry` (`<common>/worktrees/<name>`) names `dot_git` as its
/// worktree's `.git` file.
fn entry_points_at(entry: &Path, dot_git: &Path) -> bool {
    let Some(named) = read_path(&entry.join("gitdir"), entry) else {
        return false;
    };
    named.canonicalize().ok() == dot_git.canonicalize().ok()
}

/// The common git folder a worktree entry belongs to (its `commondir` file).
fn common_of(entry: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(entry.join("commondir")).ok()?;
    let common = absolute(text.lines().next()?.trim(), entry)
        .canonicalize()
        .ok()?;
    // The entry must sit in that repository's `worktrees/`.
    (entry.parent()?.canonicalize().ok()? == common.join("worktrees").canonicalize().ok()?)
        .then_some(common)
}

/// The path in the first line of `file`, relative to `base` when it is relative.
fn read_path(file: &Path, base: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| absolute(line, base))
}

fn absolute(written: &str, base: &Path) -> PathBuf {
    let path = Path::new(written);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn repo_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("src/lib.rs"), "fn main() {}\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let tree = base.path().join("elsewhere").join("feature");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                tree.to_str().unwrap(),
            ],
        );
        (base, repo, tree)
    }

    #[test]
    fn finds_the_worktrees_git_lists() {
        let (_base, repo, tree) = repo_with_worktree();
        let found = ProjectWorktrees::discover(&[repo.as_path()]).unwrap();
        let known = found.known();
        assert!(known.contains(&repo.canonicalize().unwrap()));
        assert!(known.contains(&tree.canonicalize().unwrap()));
        // From inside the linked worktree, the same two.
        let from_tree = ProjectWorktrees::discover(&[tree.as_path()]).unwrap();
        assert_eq!(from_tree.known().len(), 2);
        // A subfolder maps to the same subfolder of each worktree.
        let sub = ProjectWorktrees::discover(&[repo.join("src").as_path()]).unwrap();
        assert!(
            sub.known()
                .contains(&tree.join("src").canonicalize().unwrap())
        );
    }

    /// Makes `link` a directory link to `target`; `false` where links cannot be made.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, link).or_else(|_| {
            // Creating symbolic links needs a privilege; a junction needs none.
            let status = Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()?
                .status;
            if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other("mklink failed"))
            }
        });
        made.is_ok()
    }

    #[test]
    fn a_subfolder_that_is_a_link_out_of_its_worktree_opens_nothing() {
        let (base, repo, tree) = repo_with_worktree();
        // On the linked worktree's branch, `src` is a link to a folder outside the worktree.
        let outside = base.path().join("home");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(tree.join("src")).unwrap();
        if !link_dir(&outside, &tree.join("src")) {
            return;
        }
        let found = ProjectWorktrees::discover(&[repo.join("src").as_path()]).unwrap();
        let known = found.known();
        assert!(
            known.contains(&repo.join("src").canonicalize().unwrap()),
            "the session's own folder stays"
        );
        assert!(
            !known.contains(&outside.canonicalize().unwrap()),
            "the link's target is not opened: {known:?}"
        );
        assert_eq!(known.len(), 1);
    }

    #[test]
    fn a_forged_entry_opens_nothing() {
        let (base, repo, _tree) = repo_with_worktree();
        let found = ProjectWorktrees::discover(&[repo.as_path()]).unwrap();
        // An entry pointing at a folder whose `.git` does not point back is ignored.
        let secret = base.path().join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        let entry = repo.join(".git").join("worktrees").join("forged");
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join("gitdir"),
            format!("{}\n", secret.join(".git").display()),
        )
        .unwrap();
        found.refresh();
        assert!(!found.known().contains(&secret.canonicalize().unwrap()));
    }

    #[test]
    fn a_rewritten_dot_git_file_is_refused() {
        let (base, _repo, tree) = repo_with_worktree();
        // Another repository the worktree's `.git` file is pointed at.
        let other = base.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "-q"]);
        std::fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", other.join(".git").display()),
        )
        .unwrap();
        assert!(ProjectWorktrees::discover(&[tree.as_path()]).is_none());
    }
}
