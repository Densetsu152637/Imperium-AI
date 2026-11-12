//! Worktrees for parallel writing subagents (roadmap item 26). A writer subagent started with
//! `access: "write"` gets a new git worktree of every repository its delegator's folders are
//! in, on its own branch made from that repository's current `HEAD`; its file tools and
//! commands work there, so writers running together never edit the same files. When it stops
//! working, what it changed is committed on its branch (unless a permission rule or plan mode
//! forbids committing) and its report says how the branch integrates: the files, whether
//! merging it into the delegator's checkout is clean or conflicts, and whether it conflicts with
//! another writer's branch (all found with `git merge-tree`, which touches no checkout). The
//! merge itself is left to the delegator (or the user), with conflicts theirs to resolve; the
//! daemon never merges, resets, or checks out anything in the delegator's folders.
//!
//! Worktrees are made and removed with the session worktree rules ([`crate::worktrees`]): in
//! `<sessions.worktree_dir>/<repo>/<branch>` or the repository's `.worktrees/<branch>`, kept for
//! the life of the session (so a finished writer can be resumed), recorded beside the session's
//! journal, and removed with `session.delete` and `remove_worktrees`, which keeps the branches.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use imperium_protocol::methods::SessionWorktree;
use imperium_tools::permissions::{CallKind, Target, safety_floor};
use imperium_tools::{WriterStart, WriterTrees};
use serde::{Deserialize, Serialize};

use crate::review::{Step, check_repository_config};
use crate::safe_git;

/// Most files a report lists by name.
const LISTED_FILES: usize = 12;
/// Longest a commit may take (a signing or credential program that waits for a person would
/// otherwise hold the writer's report for ever).
const COMMIT_SECONDS: u64 = 120;

/// One writer's worktrees as they are recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    task: String,
    branch: String,
    /// The folders the writer works in (the delegator's, with each repository's replaced).
    folders: Vec<String>,
    /// The delegator's folder each of `folders` stands in for.
    #[serde(default)]
    parents: Vec<String>,
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

fn corrupt_path(registry: &Path) -> PathBuf {
    registry.with_extension("json.corrupt")
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    safe_git::run(safe_git::command(dir).args(args)).map(|out| safe_git::text(&out))
}

/// The writers recorded in `path`. A record that cannot be read or parsed is reported, and a
/// corrupt one is kept beside the original (`.corrupt`) rather than overwritten.
fn load(path: &Path) -> Vec<Entry> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not read the writers' worktree record");
            return Vec::new();
        }
    };
    match serde_json::from_str(&text) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "the writers' worktree record is corrupt; it is kept as .corrupt and the writers' worktrees must be removed by hand");
            let _ = std::fs::rename(path, corrupt_path(path));
            Vec::new()
        }
    }
}

fn session_worktrees(entries: &[Entry]) -> Vec<SessionWorktree> {
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

/// Takes back worktrees made for a writer that could not be recorded: the worktrees go, and so
/// does the branch (it was just made and holds nothing).
fn undo(records: &[SessionWorktree], branch: &str) {
    crate::worktrees::remove(records);
    let mut repos: Vec<&str> = records.iter().map(|record| record.repo.as_str()).collect();
    repos.sort_unstable();
    repos.dedup();
    for repo in repos {
        let _ = git(Path::new(repo), &["branch", "-D", branch]);
        let _ = git(Path::new(repo), &["worktree", "prune"]);
    }
}

impl GitWriterTrees {
    /// The writers of session `session`, whose journal is `journal`; worktrees go under `dir`
    /// when it is given (else inside their repositories).
    pub fn new(session: &str, journal: &Path, dir: Option<PathBuf>) -> Self {
        let registry = registry_path(journal);
        // A resumed session finds its writers' worktrees again.
        let entries = load(&registry);
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

    /// Writes the record whole to a temporary file and moves it into place, so a crash leaves
    /// the old record or the new one, never half of one.
    fn save(&self, entries: &[Entry]) -> Result<(), String> {
        let text = serde_json::to_string(entries).map_err(|error| error.to_string())?;
        let temp = self.registry.with_extension("json.tmp");
        std::fs::write(&temp, text).map_err(|error| error.to_string())?;
        std::fs::rename(&temp, &self.registry).map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            error.to_string()
        })
    }

    /// The worktrees of the writers recorded beside `journal`, as session worktrees to remove.
    pub fn recorded(journal: &Path) -> Vec<SessionWorktree> {
        session_worktrees(&load(&registry_path(journal)))
    }

    /// Removes the worktrees of the writers recorded beside `journal` (the session is being
    /// deleted), keeping their branches. What a writer left uncommitted is first committed on
    /// its branch, as well as that can be done, so removing the worktree loses nothing (a
    /// writer stopped part-way has not had its work committed yet). Returns the folders removed
    /// and the failures.
    pub fn remove_recorded(journal: &Path) -> (Vec<String>, Vec<String>) {
        let entries = load(&registry_path(journal));
        for entry in &entries {
            for tree in &entry.trees {
                let _ = commit_all(Path::new(&tree.root), &entry.task, &entry.branch);
            }
        }
        let outcome = crate::worktrees::remove(&session_worktrees(&entries));
        // Git forgets worktrees whose folder is gone.
        let mut repos: Vec<&str> = entries
            .iter()
            .flat_map(|entry| entry.trees.iter().map(|tree| tree.repo.as_str()))
            .collect();
        repos.sort_unstable();
        repos.dedup();
        for repo in repos {
            let _ = git(Path::new(repo), &["worktree", "prune"]);
        }
        outcome
    }

    /// Forgets the record beside `journal` (the session was deleted).
    pub fn forget(journal: &Path) {
        let registry = registry_path(journal);
        let _ = std::fs::remove_file(corrupt_path(&registry));
        let _ = std::fs::remove_file(registry);
    }
}

/// What merging one commit into another would do.
enum Merge {
    Clean,
    Conflicts(Vec<String>),
    Unknown(String),
}

/// Merges `theirs` into `ours` in memory (`git merge-tree`, both commits): no checkout, index or
/// ref is touched, so the answer costs nobody anything.
fn merge_check(repo: &Path, ours: &str, theirs: &str) -> Merge {
    let output = safe_git::command(repo)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            ours,
            theirs,
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

/// Runs `command` for at most `seconds`; it is killed when it takes longer.
fn run_for(command: &mut Command, seconds: u64) -> Result<(), String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not run git: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git did not finish within {seconds} seconds"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(error.to_string()),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Commits everything changed in the worktree at `root` on its branch `branch` (nothing when
/// there is nothing to commit), except files the safety floor covers (secrets), which stay
/// uncommitted. Returns the commit the branch is at and the files held back.
fn commit_all(root: &Path, task: &str, branch: &str) -> Result<(String, Vec<String>), String> {
    // The writer may have switched branches (or detached HEAD); committing then would put its
    // work on a branch that is not its own.
    let wanted = format!("refs/heads/{branch}");
    let on = git(root, &["symbolic-ref", "-q", "HEAD"]).unwrap_or_default();
    if on != wanted {
        let shown = if on.is_empty() {
            "a detached HEAD".to_string()
        } else {
            on
        };
        return Err(format!(
            "the worktree is on {shown} instead of its branch {branch}"
        ));
    }
    check_repository_config(root, Step::Commit, "commit it")?;
    git(root, &["add", "-A"])?;
    // Files that hold secrets are not committed for the writer.
    let staged_names = git(root, &["diff", "--cached", "--name-only", "--no-ext-diff"])?;
    let held: Vec<String> = staged_names
        .lines()
        .filter(|name| {
            safety_floor(
                CallKind::Edit,
                &Target::Path {
                    absolute: format!("/{name}"),
                    relative: None,
                },
            )
        })
        .map(str::to_string)
        .collect();
    if !held.is_empty() {
        let mut reset = safe_git::command(root);
        reset.args(["reset", "-q", "HEAD", "--"]).args(&held);
        safe_git::run(&mut reset)?;
    }
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
        // Never signed (a signing program may wait for a person); a machine without a git
        // identity still commits, as Imperium.
        commit.args(["-c", "commit.gpgsign=false"]);
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
        run_for(&mut commit, COMMIT_SECONDS)?;
    } else if !staged.status.success() {
        return Err(String::from_utf8_lossy(&staged.stderr).trim().to_string());
    }
    Ok((git(root, &["rev-parse", "HEAD"])?, held))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// The files with uncommitted changes in the checkout at `repo`.
fn dirty_files(repo: &Path) -> Vec<String> {
    let out = safe_git::run(safe_git::command(repo).args([
        "status",
        "--porcelain",
        "--untracked-files=all",
    ]))
    .unwrap_or_default();
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| line.get(3..))
        .map(|path| {
            path.rsplit(" -> ")
                .next()
                .unwrap_or(path)
                .trim_matches('"')
                .to_string()
        })
        .collect()
}

/// The commit `branch` is at in `repo`, when it has commits beyond `base`.
fn tip_beyond(repo: &Path, branch: &str, base: &str) -> Option<String> {
    let tip = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    )
    .ok()?;
    let ahead = git(repo, &["rev-list", "--count", &format!("{base}..{tip}")]).ok()?;
    (ahead.parse::<u64>().unwrap_or(0) > 0).then_some(tip)
}

/// The report for one worktree: its commit, files, and merge outlook.
fn report_tree(entry: &Entry, tree: &Tree, others: &[Entry], commit: bool) -> String {
    let root = Path::new(&tree.root);
    let repo = Path::new(&tree.repo);
    if !commit {
        return format!(
            "Branch {} ({}): not committed, because a permission rule (git_commit) or plan mode \
             forbids committing. The writer's changes stay uncommitted in that worktree, which \
             stays; commit them there or let the user review them.",
            entry.branch, tree.root
        );
    }
    let (head, held) = match commit_all(root, &entry.task, &entry.branch) {
        Ok(done) => done,
        Err(error) => {
            return format!(
                "Branch {} in {} could not be committed ({error}); its changes are in the \
                 worktree, which stays.",
                entry.branch, tree.root
            );
        }
    };
    let held_note = if held.is_empty() {
        String::new()
    } else {
        format!(
            "\nNot committed (they look like secrets; left in the worktree): {}.",
            held.join(", ")
        )
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
            "Branch {} ({}): the writer changed no files; there is nothing to merge.{held_note}",
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
    let mut outlook = match merge_check(repo, &parent_head, &head) {
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
    // Writers' branches that changed something are compared too: two clean merges can still
    // collide with each other.
    for other in others {
        for other_tree in other.trees.iter().filter(|other| other.repo == tree.repo) {
            let Some(tip) = tip_beyond(repo, &other.branch, &other_tree.base) else {
                continue;
            };
            if let Merge::Conflicts(files) = merge_check(repo, &tip, &head) {
                outlook.push_str(&format!(
                    "\nIt conflicts with writer {} (branch {}) in {}: merge one of them first, \
                     then have the other reworked on top of it.",
                    other.task,
                    other.branch,
                    files.join(", ")
                ));
            }
        }
    }
    // The delegator's own uncommitted edits can make the merge refuse, or get mixed in.
    let dirty = dirty_files(repo);
    let clash: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.split_once('\t').map(|(_, name)| name))
        .filter(|name| dirty.iter().any(|path| path.as_str() == *name))
        .collect();
    if !clash.is_empty() {
        outlook.push_str(&format!(
            "\nCaution: {} has uncommitted changes in files this branch also changes ({}); \
             commit or stash them before merging.",
            tree.repo,
            clash.join(", ")
        ));
    }
    format!(
        "Branch {} ({}) holds the writer's work as commit {}, {} file(s) changed:\n{}\n{outlook}{held_note}",
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
        // A repository with no commit yet has nothing to start a worktree from.
        for folder in &shown {
            if let Some(top) = safe_git::toplevel(Path::new(folder))
                && safe_git::head(&top).is_none()
            {
                return Ok(None);
            }
        }
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
                    undo(&records, &branch);
                    return Err(error);
                }
            };
            if trees.iter().any(|tree| tree.root == root) {
                continue;
            }
            let base = match git(Path::new(&root), &["rev-parse", "HEAD"]) {
                Ok(base) => base,
                Err(error) => {
                    undo(&records, &branch);
                    return Err(error);
                }
            };
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
            parents: shown.clone(),
            trees,
        };
        {
            let mut entries = self.entries();
            entries.push(entry);
            if let Err(error) = self.save(&entries) {
                entries.pop();
                drop(entries);
                undo(&records, &branch);
                return Err(format!(
                    "could not record the writer's worktrees ({error}); none was made"
                ));
            }
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
            parents: shown.into_iter().map(PathBuf::from).collect(),
            branch,
            notice,
        }))
    }

    fn is_writer(&self, task: &str) -> bool {
        self.entries().iter().any(|entry| entry.task == task)
    }

    fn resume(&self, task: &str) -> Option<WriterStart> {
        let entries = self.entries();
        let entry = entries.iter().find(|entry| entry.task == task)?;
        let folders: Vec<PathBuf> = entry.folders.iter().map(PathBuf::from).collect();
        if !folders.iter().all(|folder| folder.is_dir()) {
            return None;
        }
        let parents: Vec<PathBuf> = if entry.parents.len() == folders.len() {
            entry.parents.iter().map(PathBuf::from).collect()
        } else {
            folders.clone()
        };
        Some(WriterStart {
            folders,
            parents,
            branch: entry.branch.clone(),
            notice: String::new(),
        })
    }

    fn finish(&self, task: &str, commit: bool) -> String {
        let (entry, others) = {
            let entries = self.entries();
            let Some(entry) = entries.iter().find(|entry| entry.task == task).cloned() else {
                return String::new();
            };
            let others: Vec<Entry> = entries
                .iter()
                .filter(|other| other.task != task)
                .cloned()
                .collect();
            (entry, others)
        };
        entry
            .trees
            .iter()
            .map(|tree| report_tree(&entry, tree, &others, commit))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
