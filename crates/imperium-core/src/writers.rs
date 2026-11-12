//! Worktrees for parallel writing subagents (roadmap item 26). A writer subagent started with
//! `access: "write"` gets a new git worktree of every repository its delegator's folders are
//! in, on its own branch made from that repository's current `HEAD`; its file tools and
//! commands work there, so writers running together never edit the same files. When it stops
//! working, what it changed is committed on its branch and its report says how the branch
//! integrates with the delegator's checkout: the files, and whether merging it now is clean or
//! conflicts (found with `git merge-tree`, which touches neither checkout). The merge itself is
//! left to the delegator (or the user), with conflicts theirs to resolve; the daemon never
//! merges, resets, or checks out anything in the delegator's folders.
//!
//! Worktrees are made and removed with the session worktree rules ([`crate::worktrees`]): in
//! `<sessions.worktree_dir>/<repo>/<branch>` or the repository's `.worktrees/<branch>`, kept for
//! the life of the session (so a finished writer can be resumed), recorded beside the session's
//! journal, and removed with `session.delete` and `remove_worktrees`, which keeps the branches.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use imperium_protocol::methods::SessionWorktree;
use imperium_tools::{WriterStart, WriterTrees};
use serde::{Deserialize, Serialize};

use crate::review::{Step, check_repository_config};
use crate::safe_git;

/// Most files a report lists by name.
const LISTED_FILES: usize = 12;

/// One writer's worktrees as they are recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    task: String,
    branch: String,
    /// The folders the writer works in (the delegator's, with each repository's replaced).
    folders: Vec<String>,
    trees: Vec<Tree>,
}

/// A writer's worktree of one repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tree {
    /// The repository's folder (the delegator's checkout).
    repo: String,
    /// The worktree's top folder.
    root: String,
    /// The commit the branch was made from.
    base: String,
}

/// Makes and finishes the writer subagents' worktrees of one session.
#[derive(Debug)]
pub struct GitWriterTrees {
    session: String,
    /// `sessions.worktree_dir`, when set.
    dir: Option<PathBuf>,
    /// Where the writers' worktrees are recorded: beside the session's journal.
    registry: PathBuf,
    entries: Mutex<Vec<Entry>>,
}

fn registry_path(journal: &Path) -> PathBuf {
    journal.with_extension("writers.json")
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    safe_git::run(safe_git::command(dir).args(args)).map(|out| safe_git::text(&out))
}

impl GitWriterTrees {
    /// The writers of session `session`, whose journal is `journal`; worktrees go under `dir`
    /// when it is given (else inside their repositories).
    pub fn new(session: &str, journal: &Path, dir: Option<PathBuf>) -> Self {
        let registry = registry_path(journal);
        // A resumed session finds its writers' worktrees again.
        let entries = std::fs::read_to_string(&registry)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        GitWriterTrees {
            session: session.to_string(),
            dir,
            registry,
            entries: Mutex::new(entries),
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, entries: &[Entry]) {
        let written = serde_json::to_string(entries)
            .map_err(|error| error.to_string())
            .and_then(|text| std::fs::write(&self.registry, text).map_err(|e| e.to_string()));
        if let Err(error) = written {
            tracing::warn!(path = %self.registry.display(), %error, "could not record the writers' worktrees");
        }
    }

    /// The worktrees of the writers recorded beside `journal`, as session worktrees to remove.
    pub fn recorded(journal: &Path) -> Vec<SessionWorktree> {
        let entries: Vec<Entry> = std::fs::read_to_string(registry_path(journal))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        entries
            .iter()
            .flat_map(|entry| {
                entry.trees.iter().map(|tree| SessionWorktree {
                    folder: tree.root.clone(),
                    path: tree.root.clone(),
                    branch: entry.branch.clone(),
                    repo: tree.repo.clone(),
                })
            })
            .collect()
    }

    /// Forgets the record beside `journal` (the session was deleted).
    pub fn forget(journal: &Path) {
        let _ = std::fs::remove_file(registry_path(journal));
    }
}

/// What merging `branch` into the checkout at `repo` would do right now.
enum Merge {
    Clean,
    Conflicts(Vec<String>),
    Unknown(String),
}

/// Merges `branch` into `head` in memory (`git merge-tree`): the checkout, index and refs are
/// not touched, so the answer costs the delegator nothing.
fn merge_check(repo: &Path, head: &str, branch: &str) -> Merge {
    let output = safe_git::command(repo)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            head,
            branch,
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => Merge::Clean,
        Ok(out) if out.status.code() == Some(1) => {
            // The merged tree's id, then the conflicted files.
            let text = String::from_utf8_lossy(&out.stdout);
            Merge::Conflicts(
                text.lines()
                    .skip(1)
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        }
        Ok(out) => Merge::Unknown(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(error) => Merge::Unknown(error.to_string()),
    }
}

/// Commits everything changed in the worktree at `root` on its branch (nothing when there is
/// nothing to commit). Returns the commit the branch is at.
fn commit_all(root: &Path, task: &str) -> Result<String, String> {
    check_repository_config(root, Step::Commit, "commit it")?;
    git(root, &["add", "-A"])?;
    let staged = safe_git::command(root)
        .args([
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if staged.status.code() == Some(1) {
        let message = format!("Writer subagent {task}");
        let mut commit = safe_git::command(root);
        // A machine without a git identity still commits, as Imperium.
        let identified = git(root, &["config", "user.email"]).is_ok_and(|email| !email.is_empty());
        if !identified {
            commit.args([
                "-c",
                "user.name=Imperium",
                "-c",
                "user.email=imperium@localhost",
            ]);
        }
        commit.args(["commit", "--no-verify", "--quiet", "-m", &message]);
        safe_git::run(&mut commit)?;
    } else if !staged.status.success() {
        return Err(String::from_utf8_lossy(&staged.stderr).trim().to_string());
    }
    git(root, &["rev-parse", "HEAD"])
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// The report for one worktree: its commit, files, and merge outlook.
fn report_tree(entry: &Entry, tree: &Tree) -> String {
    let root = Path::new(&tree.root);
    let repo = Path::new(&tree.repo);
    let head = match commit_all(root, &entry.task) {
        Ok(head) => head,
        Err(error) => {
            return format!(
                "Branch {} in {} could not be committed ({error}); its changes are in the \
                 worktree, which stays.",
                entry.branch, tree.root
            );
        }
    };
    let files = git(
        root,
        &[
            "diff",
            "--name-status",
            "--no-renames",
            "--no-ext-diff",
            &tree.base,
            &head,
        ],
    )
    .unwrap_or_default();
    let lines: Vec<&str> = files.lines().collect();
    if lines.is_empty() {
        return format!(
            "Branch {} ({}): the writer changed no files; there is nothing to merge.",
            entry.branch, tree.root
        );
    }
    let mut listed: Vec<String> = lines
        .iter()
        .take(LISTED_FILES)
        .map(|line| line.replacen('\t', " ", 1))
        .collect();
    if lines.len() > LISTED_FILES {
        listed.push(format!("... and {} more", lines.len() - LISTED_FILES));
    }
    let parent = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let parent_head = git(repo, &["rev-parse", "HEAD"]).unwrap_or_default();
    let outlook = match merge_check(repo, &parent_head, &entry.branch) {
        Merge::Clean => format!(
            "It merges cleanly into {parent} ({}) as it is now: run `git merge --no-ff {}` in {}.",
            short(&parent_head),
            entry.branch,
            tree.repo
        ),
        Merge::Conflicts(files) => format!(
            "It does NOT merge cleanly into {parent} ({}): conflicts in {}. Nothing was \
             merged; merge it yourself in {} and resolve them, or ask the writer to rework \
             its change.",
            short(&parent_head),
            files.join(", "),
            tree.repo
        ),
        Merge::Unknown(why) => format!(
            "Whether it merges into {parent} could not be checked ({why}); merge it yourself \
             in {} with `git merge --no-ff {}`.",
            tree.repo, entry.branch
        ),
    };
    format!(
        "Branch {} ({}) holds the writer's work as commit {}, {} file(s) changed:\n{}\n{outlook}",
        entry.branch,
        tree.root,
        short(&head),
        lines.len(),
        listed.join("\n")
    )
}

impl WriterTrees for GitWriterTrees {
    fn start(&self, task: &str, folders: &[PathBuf]) -> Result<Option<WriterStart>, String> {
        let shown: Vec<String> = folders
            .iter()
            .map(|folder| safe_git::plain(folder).display().to_string())
            .collect();
        let branch = format!(
            "imperium/{}-{task}",
            &self.session[..self.session.len().min(8)]
        );
        let (used, records) = crate::worktrees::create(&shown, &branch, self.dir.as_deref())?;
        if records.is_empty() {
            return Ok(None);
        }
        let mut trees: Vec<Tree> = Vec::new();
        let mut uncommitted = Vec::new();
        for record in &records {
            let repo = Path::new(&record.repo);
            let root = match crate::worktrees::registered_root(repo, Path::new(&record.path)) {
                Ok(root) => root,
                Err(error) => {
                    // Not recorded, so it would never be cleaned: take it back out.
                    crate::worktrees::remove(&records);
                    return Err(error);
                }
            };
            if trees.iter().any(|tree| tree.root == root) {
                continue;
            }
            let base = git(Path::new(&root), &["rev-parse", "HEAD"])?;
            if git(repo, &["status", "--porcelain"]).is_ok_and(|status| !status.is_empty()) {
                uncommitted.push(record.repo.clone());
            }
            trees.push(Tree {
                repo: record.repo.clone(),
                root,
                base,
            });
        }
        let entry = Entry {
            task: task.to_string(),
            branch: branch.clone(),
            folders: used.clone(),
            trees,
        };
        {
            let mut entries = self.entries();
            entries.push(entry);
            self.save(&entries);
        }
        let notice = if uncommitted.is_empty() {
            String::new()
        } else {
            format!(
                "{} has uncommitted changes, which the writer's worktree does not hold (it \
                 starts from the last commit)",
                uncommitted.join(", ")
            )
        };
        Ok(Some(WriterStart {
            folders: used.into_iter().map(PathBuf::from).collect(),
            branch,
            notice,
        }))
    }

    fn folders(&self, task: &str) -> Option<Vec<PathBuf>> {
        let entries = self.entries();
        let entry = entries.iter().find(|entry| entry.task == task)?;
        let folders: Vec<PathBuf> = entry.folders.iter().map(PathBuf::from).collect();
        folders
            .iter()
            .all(|folder| folder.is_dir())
            .then_some(folders)
    }

    fn finish(&self, task: &str) -> String {
        let Some(entry) = self
            .entries()
            .iter()
            .find(|entry| entry.task == task)
            .cloned()
        else {
            return String::new();
        };
        entry
            .trees
            .iter()
            .map(|tree| report_tree(&entry, tree))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
