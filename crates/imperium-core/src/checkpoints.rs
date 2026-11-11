//! Git-based checkpoints of a session's files (roadmap item 6), and rewinding to them.
//!
//! Before each root turn, the files of every git repository the session's folders are in are
//! listed and hashed into the repository's object store; after the turn they are listed again,
//! and when anything changed, the "before" state is kept as a commit on a private ref,
//! `refs/imperium/checkpoints/<session>/<n>`, whose parent is HEAD as it was. A checkpoint holds
//! every file git would see: tracked files and untracked ones that are not ignored, byte for
//! byte (no filters). The repository's index, HEAD, and branches are never touched: the commit
//! is built in a temporary index of the daemon's own with plumbing commands, run through
//! [`crate::safe_git`] so nothing a repository's config names runs in the daemon.
//!
//! Each checkpoint's record (`<data>/sessions/<id>.checkpoints/<n>.json`) links it to the
//! journal (its length when the turn started) and keeps the root conversation as it was, so
//! the conversation can be rewound too ([`rewind`]). Folders outside git, and sessions whose
//! folders are on another machine, have no checkpoints.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use imperium_protocol::methods::{
    CheckpointRepo, RewindMode, SESSION_CHECKPOINT, SessionCheckpoint, SessionCheckpointParams,
    SessionCheckpointsResult, SessionRewindParams, SessionRewindResult,
};
use serde::{Deserialize, Serialize};

use crate::safe_git::{self, command, run, run_with_input, text};
use crate::state::DaemonState;

/// Where checkpoint refs live in a repository.
pub(crate) const REF_PREFIX: &str = "refs/imperium/checkpoints";

/// Files changed this recently are hashed again next time even when their size and time match
/// (a write in the same clock tick would not change them).
const RACY: Duration = Duration::from_secs(2);

/// One file as git would store it: its mode (`100644`, `100755`, `120000`, or `160000` for a
/// submodule) and its blob (or commit) id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) mode: String,
    pub(crate) sha: String,
}

/// A working tree's files by path (relative, `/`-separated).
pub(crate) type Files = BTreeMap<String, Entry>;

/// Blob ids of files already hashed, by path, size, and modification time, so unchanged files
/// are not read again.
#[derive(Debug, Default)]
pub(crate) struct HashCache(HashMap<String, (u64, u128, String)>);

/// Whether a path from git is a plain relative path inside the working tree.
pub(crate) fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('"')
        && !path.contains('\n')
        && Path::new(path)
            .components()
            .all(|component| match component {
                Component::Normal(name) => !name.to_string_lossy().eq_ignore_ascii_case(".git"),
                _ => false,
            })
}

/// The files git would see in the working tree at `top`: tracked files that exist and untracked
/// files that are not ignored, each hashed into the object store as it is.
pub(crate) fn working_files(top: &Path, cache: &mut HashCache) -> Result<Files, String> {
    let staged = run(command(top).args(["ls-files", "-z", "--stage"]))?;
    let mut tracked: HashMap<String, Entry> = HashMap::new();
    for record in staged.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let Ok(record) = std::str::from_utf8(record) else {
            continue;
        };
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split(' ');
        if let (Some(mode), Some(sha)) = (parts.next(), parts.next()) {
            tracked.entry(path.to_string()).or_insert(Entry {
                mode: mode.to_string(),
                sha: sha.to_string(),
            });
        }
    }
    let others = run(command(top).args(["ls-files", "-z", "--others", "--exclude-standard"]))?;
    let mut paths: BTreeSet<String> = tracked.keys().cloned().collect();
    for path in others.split(|byte| *byte == 0).filter(|p| !p.is_empty()) {
        if let Ok(path) = std::str::from_utf8(path)
            && !path.ends_with('/')
        {
            paths.insert(path.to_string());
        }
    }
    let now = SystemTime::now();
    let mut files = Files::new();
    let mut to_hash: Vec<(String, String, u64, u128, bool)> = Vec::new();
    let mut links: Vec<(String, Vec<u8>)> = Vec::new();
    for path in paths {
        let known = tracked.get(&path);
        if let Some(entry) = known.filter(|entry| entry.mode == "160000") {
            // A submodule: its commit as recorded; its own files are its own repository's.
            files.insert(path, entry.clone());
            continue;
        }
        if !safe_relative(&path) {
            continue;
        }
        let full = top.join(&path);
        // A path under a linked folder could lead out of the working tree; it is not read.
        if !no_links_between(top, &full) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&full) {
                links.push((path, link_bytes(&target)));
            }
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let mode = file_mode(&meta, known.map(|entry| entry.mode.as_str()));
        let modified = meta.modified().ok();
        let mtime = modified
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        if let Some((len, time, sha)) = cache.0.get(&path)
            && *len == meta.len()
            && *time == mtime
            && mtime != 0
        {
            files.insert(
                path,
                Entry {
                    mode,
                    sha: sha.clone(),
                },
            );
            continue;
        }
        let settled = modified
            .and_then(|time| now.duration_since(time).ok())
            .is_some_and(|age| age > RACY);
        to_hash.push((path, mode, meta.len(), mtime, settled));
    }
    if !to_hash.is_empty() {
        let mut input = Vec::new();
        for (path, ..) in &to_hash {
            input.extend_from_slice(path.as_bytes());
            input.push(b'\n');
        }
        let out = run_with_input(
            command(top).args(["hash-object", "-w", "--no-filters", "--stdin-paths"]),
            input,
        )?;
        let out = String::from_utf8_lossy(&out);
        let shas: Vec<&str> = out.lines().collect();
        if shas.len() != to_hash.len() {
            return Err("git hash-object returned the wrong number of ids".into());
        }
        for ((path, mode, len, mtime, settled), sha) in to_hash.into_iter().zip(shas) {
            if settled {
                cache.0.insert(path.clone(), (len, mtime, sha.to_string()));
            }
            files.insert(
                path,
                Entry {
                    mode,
                    sha: sha.to_string(),
                },
            );
        }
    }
    for (path, target) in links {
        let out = run_with_input(
            command(top).args(["hash-object", "-w", "--no-filters", "--stdin"]),
            target,
        )?;
        files.insert(
            path,
            Entry {
                mode: "120000".into(),
                sha: text(&out),
            },
        );
    }
    Ok(files)
}

#[cfg(unix)]
fn file_mode(meta: &std::fs::Metadata, _tracked: Option<&str>) -> String {
    use std::os::unix::fs::PermissionsExt;
    if meta.permissions().mode() & 0o111 != 0 {
        "100755".into()
    } else {
        "100644".into()
    }
}

/// Windows has no executable bit: a tracked file keeps the mode git has for it.
#[cfg(not(unix))]
fn file_mode(_meta: &std::fs::Metadata, tracked: Option<&str>) -> String {
    match tracked {
        Some("100755") => "100755".into(),
        _ => "100644".into(),
    }
}

#[cfg(unix)]
fn link_bytes(target: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    target.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn link_bytes(target: &Path) -> Vec<u8> {
    target.to_string_lossy().replace('\\', "/").into_bytes()
}

/// Writes `files` as a tree object, through a temporary index at `index` (never the
/// repository's own), returning the tree's id.
pub(crate) fn write_tree(top: &Path, files: &Files, index: &Path) -> Result<String, String> {
    let _ = std::fs::remove_file(index);
    let mut input = Vec::new();
    for (path, entry) in files {
        input.extend_from_slice(format!("{} {}\t{path}", entry.mode, entry.sha).as_bytes());
        input.push(0);
    }
    let result = run_with_input(
        command(top)
            .env("GIT_INDEX_FILE", index)
            .args(["update-index", "-z", "--index-info"]),
        input,
    )
    .and_then(|_| run(command(top).env("GIT_INDEX_FILE", index).arg("write-tree")));
    let _ = std::fs::remove_file(index);
    result.map(|out| text(&out))
}

/// Makes a commit of `tree` (with `parent`, when there is one), not on any branch.
pub(crate) fn commit_tree(
    top: &Path,
    tree: &str,
    parent: Option<&str>,
    message: &str,
) -> Result<String, String> {
    let mut git = command(top);
    git.args(["commit-tree", "--no-gpg-sign", "-m", message]);
    if let Some(parent) = parent {
        git.args(["-p", parent]);
    }
    git.arg(tree)
        .env("GIT_AUTHOR_NAME", "Imperium")
        .env("GIT_AUTHOR_EMAIL", "imperium@localhost")
        .env("GIT_COMMITTER_NAME", "Imperium")
        .env("GIT_COMMITTER_EMAIL", "imperium@localhost");
    run(&mut git).map(|out| text(&out))
}

/// The files in commit `commit`'s tree.
pub(crate) fn commit_files(top: &Path, commit: &str) -> Result<Files, String> {
    let out = run(command(top).args(["ls-tree", "-r", "-z", "--full-tree", commit]))?;
    let mut files = Files::new();
    for record in out.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let Ok(record) = std::str::from_utf8(record) else {
            continue;
        };
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        let parts: Vec<&str> = meta.split(' ').collect();
        if let [mode, _kind, sha] = parts[..] {
            files.insert(
                path.to_string(),
                Entry {
                    mode: mode.to_string(),
                    sha: sha.to_string(),
                },
            );
        }
    }
    Ok(files)
}

/// A blob's contents, as stored.
pub(crate) fn blob(top: &Path, sha: &str) -> Result<Vec<u8>, String> {
    run(command(top).args(["cat-file", "blob", sha]))
}

/// Whether every folder between `top` and `full` is a real folder (not a link that could lead
/// out of the working tree).
fn no_links_between(top: &Path, full: &Path) -> bool {
    let Ok(inside) = full.strip_prefix(top) else {
        return false;
    };
    let mut at = top.to_path_buf();
    let mut components: Vec<_> = inside.components().collect();
    components.pop();
    for component in components {
        at.push(component);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => return false,
            _ => {}
        }
    }
    true
}

/// Writes one file of a checkpoint (or of a reviewed change) into the working tree.
pub(crate) fn write_file(top: &Path, path: &str, entry: &Entry) -> Result<PathBuf, String> {
    if !safe_relative(path) {
        return Err(format!("{path}: not a path inside the repository"));
    }
    let full = top.join(path);
    if !no_links_between(top, &full) {
        return Err(format!("{path}: a folder on its way is a link"));
    }
    let contents = blob(top, &entry.sha)?;
    match std::fs::symlink_metadata(&full) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir(&full)
            .map_err(|error| format!("{path}: a folder is in the way: {error}"))?,
        // Removed rather than written through: it may be a link.
        Ok(_) => std::fs::remove_file(&full).map_err(|error| format!("{path}: {error}"))?,
        Err(_) => {}
    }
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{path}: {error}"))?;
    }
    #[cfg(unix)]
    if entry.mode == "120000" {
        use std::os::unix::ffi::OsStrExt;
        let target = std::ffi::OsStr::from_bytes(&contents);
        std::os::unix::fs::symlink(target, &full).map_err(|error| format!("{path}: {error}"))?;
        return Ok(full);
    }
    std::fs::write(&full, &contents).map_err(|error| format!("{path}: {error}"))?;
    #[cfg(unix)]
    if entry.mode == "100755" {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755));
    }
    Ok(full)
}

/// Removes one file from the working tree, and the folders it leaves empty.
pub(crate) fn remove_file(top: &Path, path: &str) -> Result<PathBuf, String> {
    if !safe_relative(path) {
        return Err(format!("{path}: not a path inside the repository"));
    }
    let full = top.join(path);
    if !no_links_between(top, &full) {
        return Err(format!("{path}: a folder on its way is a link"));
    }
    std::fs::remove_file(&full).map_err(|error| format!("{path}: {error}"))?;
    let mut dir = full.parent();
    while let Some(folder) = dir {
        if folder == top || !folder.starts_with(top) || std::fs::remove_dir(folder).is_err() {
            break;
        }
        dir = folder.parent();
    }
    Ok(full)
}

/// Puts the working tree at `top` back to `target`: files that differ or are missing are
/// written, files git would see that `target` lacks are removed. Ignored files and submodules
/// are left alone. Returns the files written and removed.
pub(crate) fn restore(
    top: &Path,
    target: &Files,
    cache: &mut HashCache,
) -> Result<(Vec<String>, Vec<String>), String> {
    let current = working_files(top, cache)?;
    let mut restored = Vec::new();
    let mut removed = Vec::new();
    let mut errors = Vec::new();
    for (path, entry) in &current {
        if entry.mode != "160000" && !target.contains_key(path) {
            match remove_file(top, path) {
                Ok(full) => removed.push(full.display().to_string()),
                Err(error) => errors.push(error),
            }
        }
    }
    for (path, entry) in target {
        if entry.mode == "160000" || current.get(path) == Some(entry) {
            continue;
        }
        match write_file(top, path, entry) {
            Ok(full) => restored.push(full.display().to_string()),
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Ok((restored, removed))
    } else {
        Err(format!(
            "some files could not be put back: {}",
            errors.join("; ")
        ))
    }
}

/// A checkpoint's record, beside the session's journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    checkpoint: SessionCheckpoint,
    /// The journal's length in bytes when the turn started.
    #[serde(default)]
    journal_offset: u64,
    /// For `turn`: the files the turn changed, by repository root, so a review lists the
    /// session's changes and not edits made between turns.
    #[serde(default)]
    touched: BTreeMap<String, Vec<String>>,
}

/// The paths whose entries differ between `before` and `after`, submodules aside.
pub(crate) fn differing(before: &Files, after: &Files) -> Vec<String> {
    let mut paths: BTreeSet<&String> = BTreeSet::new();
    paths.extend(before.keys());
    paths.extend(after.keys());
    paths
        .into_iter()
        .filter(|path| {
            let (a, b) = (before.get(*path), after.get(*path));
            a != b
                && a.is_none_or(|entry| entry.mode != "160000")
                && b.is_none_or(|entry| entry.mode != "160000")
        })
        .cloned()
        .collect()
}

/// One repository's files when a turn started.
#[derive(Debug)]
struct Snapshot {
    top: PathBuf,
    files: Files,
    tree: String,
    parent: Option<String>,
}

/// What a turn started with, kept until it ends.
#[derive(Debug)]
struct Pending {
    prompt: String,
    at_unix_ms: u64,
    journal_offset: u64,
    messages: Option<Vec<u8>>,
    snapshots: Vec<Snapshot>,
}

#[derive(Debug, Default)]
struct Inner {
    caches: HashMap<PathBuf, HashCache>,
    pending: Option<Pending>,
}

/// One session's checkpoints: told about each root turn by the engine.
#[derive(Debug)]
pub(crate) struct SessionCheckpoints {
    session_id: String,
    /// `<data>/sessions/<id>.checkpoints`.
    dir: PathBuf,
    journal: PathBuf,
    folders: Vec<PathBuf>,
    inner: Mutex<Inner>,
    /// Told about each checkpoint, to announce it (`session.checkpoint`).
    daemon: Weak<DaemonState>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The folder a session's checkpoint records are kept in.
pub(crate) fn records_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir
        .join("sessions")
        .join(format!("{session_id}.checkpoints"))
}

/// The ref keeping checkpoint `number`'s commit in the session's `index`th repository.
fn reference(session_id: &str, number: u32, index: usize) -> String {
    if index == 0 {
        format!("{REF_PREFIX}/{session_id}/{number}")
    } else {
        format!("{REF_PREFIX}/{session_id}/{number}-{index}")
    }
}

impl SessionCheckpoints {
    pub(crate) fn new(
        daemon: Weak<DaemonState>,
        data_dir: &Path,
        session_id: &str,
        journal: PathBuf,
        folders: Vec<PathBuf>,
    ) -> Self {
        SessionCheckpoints {
            session_id: session_id.to_string(),
            dir: records_dir(data_dir, session_id),
            journal,
            folders,
            inner: Mutex::default(),
            daemon,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The working trees of the git repositories the session's folders are in.
    pub(crate) fn repositories(&self) -> Vec<PathBuf> {
        let mut tops: Vec<PathBuf> = Vec::new();
        for folder in &self.folders {
            if let Some(top) = safe_git::toplevel(folder)
                && !tops.contains(&top)
            {
                tops.push(top);
            }
        }
        tops
    }

    /// The files git would see in the working tree at `top`, with the session's hash cache.
    pub(crate) fn working_files(&self, top: &Path) -> Result<Files, String> {
        let mut inner = self.lock();
        working_files(top, inner.caches.entry(top.to_path_buf()).or_default())
    }

    /// Whether a root turn is running (between `before_turn` and `after_turn`).
    pub(crate) fn turn_running(&self) -> bool {
        self.lock().pending.is_some()
    }

    /// The session's checkpoints, oldest first.
    pub(crate) fn list(&self) -> Vec<SessionCheckpoint> {
        self.records()
            .into_iter()
            .map(|record| record.checkpoint)
            .collect()
    }

    fn records(&self) -> Vec<Record> {
        let mut records: Vec<Record> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.ends_with(".json") && !name.ends_with(".messages.json")
            })
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        records.sort_by_key(|record| record.checkpoint.number);
        records
    }

    fn record(&self, number: u32) -> Option<Record> {
        let text = std::fs::read_to_string(self.dir.join(format!("{number}.json"))).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn next_number(&self) -> u32 {
        self.records()
            .last()
            .map_or(1, |record| record.checkpoint.number + 1)
    }

    fn snapshot(&self, inner: &mut Inner, top: &Path) -> Result<Snapshot, String> {
        let cache = inner.caches.entry(top.to_path_buf()).or_default();
        let files = working_files(top, cache)?;
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let tree = write_tree(top, &files, &self.dir.join("index.tmp"))?;
        Ok(Snapshot {
            top: top.to_path_buf(),
            files,
            tree,
            parent: safe_git::head(top),
        })
    }

    /// Keeps `snapshots` as checkpoint commits on private refs, with a record.
    fn keep(
        &self,
        kind: &str,
        taken: Pending,
        touched: BTreeMap<String, Vec<String>>,
    ) -> Result<SessionCheckpoint, String> {
        let Pending {
            prompt,
            at_unix_ms,
            journal_offset,
            messages,
            snapshots,
        } = taken;
        let number = self.next_number();
        let headline = prompt.lines().next().unwrap_or_default();
        let message = format!(
            "Imperium checkpoint {number} of session {}\n\n{headline}",
            self.session_id
        );
        let mut repos = Vec::new();
        for (index, snapshot) in snapshots.iter().enumerate() {
            let commit = commit_tree(
                &snapshot.top,
                &snapshot.tree,
                snapshot.parent.as_deref(),
                &message,
            )?;
            let name = reference(&self.session_id, number, index);
            run(command(&snapshot.top).args(["update-ref", &name, &commit]))?;
            repos.push(CheckpointRepo {
                root: snapshot.top.display().to_string(),
                commit,
                reference: name,
            });
        }
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let conversation = kind == "turn";
        if let (true, Some(messages)) = (conversation, messages.as_deref()) {
            std::fs::write(self.dir.join(format!("{number}.messages.json")), messages)
                .map_err(|error| error.to_string())?;
        }
        let checkpoint = SessionCheckpoint {
            number,
            at_unix_ms,
            kind: kind.to_string(),
            prompt,
            repos,
            conversation,
        };
        let record = Record {
            checkpoint: checkpoint.clone(),
            journal_offset,
            touched,
        };
        let text = serde_json::to_string_pretty(&record).map_err(|error| error.to_string())?;
        std::fs::write(self.dir.join(format!("{number}.json")), text)
            .map_err(|error| error.to_string())?;
        if let Some(daemon) = self.daemon.upgrade() {
            daemon.announce(
                SESSION_CHECKPOINT,
                SessionCheckpointParams {
                    session_id: self.session_id.clone(),
                    checkpoint: checkpoint.clone(),
                },
            );
        }
        Ok(checkpoint)
    }

    /// Records the files as they are now as a `rewind` checkpoint.
    fn keep_now(&self, inner: &mut Inner, repos: &[PathBuf]) -> Result<SessionCheckpoint, String> {
        let mut snapshots = Vec::new();
        for top in repos {
            snapshots.push(self.snapshot(inner, top)?);
        }
        let offset = std::fs::metadata(&self.journal).map_or(0, |meta| meta.len());
        let taken = Pending {
            prompt: String::new(),
            at_unix_ms: now_ms(),
            journal_offset: offset,
            messages: None,
            snapshots,
        };
        self.keep("rewind", taken, BTreeMap::new())
    }

    /// The files the session's turns changed from checkpoint `since` on, by repository root.
    pub(crate) fn touched_since(&self, since: u32) -> HashMap<String, BTreeSet<String>> {
        let mut touched: HashMap<String, BTreeSet<String>> = HashMap::new();
        for record in self.records() {
            if record.checkpoint.number < since {
                continue;
            }
            for (root, paths) in record.touched {
                touched.entry(root).or_default().extend(paths);
            }
        }
        touched
    }

    /// Puts the files back to checkpoint `record`, first keeping them as they are as a `rewind`
    /// checkpoint. Returns that checkpoint and the files written and removed.
    fn rewind_files(&self, record: &Record) -> Result<(u32, Vec<String>, Vec<String>), String> {
        let mut inner = self.lock();
        if inner.pending.is_some() {
            return Err("a turn is running; wait for it to finish or stop it first".into());
        }
        let repos: Vec<PathBuf> = record
            .checkpoint
            .repos
            .iter()
            .map(|repo| PathBuf::from(&repo.root))
            .collect();
        let undo = self.keep_now(&mut inner, &repos)?;
        let mut restored = Vec::new();
        let mut removed = Vec::new();
        // Every repository is tried; a failure in one does not leave the rest unrewound.
        let mut errors = Vec::new();
        for repo in &record.checkpoint.repos {
            let top = PathBuf::from(&repo.root);
            let cache = inner.caches.entry(top.clone()).or_default();
            match commit_files(&top, &repo.commit).and_then(|target| restore(&top, &target, cache))
            {
                Ok((written, gone)) => {
                    restored.extend(written);
                    removed.extend(gone);
                }
                Err(error) => errors.push(format!("{}: {error}", repo.root)),
            }
        }
        if !errors.is_empty() {
            return Err(format!(
                "the files were only partly rewound ({}; {} restored, {} removed elsewhere: {}); \
                 rewinding the files to checkpoint {} undoes it",
                errors.join("; "),
                restored.len(),
                removed.len(),
                restored
                    .iter()
                    .chain(&removed)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                undo.number
            ));
        }
        Ok((undo.number, restored, removed))
    }
}

impl imperium_engine::TurnCheckpoints for SessionCheckpoints {
    fn before_turn(&self, prompt: &str) {
        let mut inner = self.lock();
        let mut snapshots = Vec::new();
        for top in self.repositories() {
            match self.snapshot(&mut inner, &top) {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(error) => {
                    tracing::warn!(session = %self.session_id, repo = %top.display(), %error, "could not checkpoint the repository");
                }
            }
        }
        let journal_offset = std::fs::metadata(&self.journal).map_or(0, |meta| meta.len());
        let messages = std::fs::read(imperium_engine::messages_path(&self.journal)).ok();
        inner.pending = Some(Pending {
            prompt: prompt.to_string(),
            at_unix_ms: now_ms(),
            journal_offset,
            messages,
            snapshots,
        });
    }

    fn after_turn(&self) {
        let mut inner = self.lock();
        let Some(pending) = inner.pending.take() else {
            return;
        };
        let mut touched = BTreeMap::new();
        for snapshot in &pending.snapshots {
            let cache = inner.caches.entry(snapshot.top.clone()).or_default();
            match working_files(&snapshot.top, cache) {
                Ok(files) => {
                    let changed = differing(&snapshot.files, &files);
                    if !changed.is_empty() {
                        touched.insert(snapshot.top.display().to_string(), changed);
                    }
                }
                Err(error) => {
                    tracing::warn!(session = %self.session_id, %error, "could not list the repository's files after the turn");
                }
            }
        }
        if touched.is_empty() {
            return;
        }
        if let Err(error) = self.keep("turn", pending, touched) {
            tracing::warn!(session = %self.session_id, %error, "could not record a checkpoint");
        }
    }
}

/// Deletes a session's checkpoints: their refs in each repository and their records.
pub(crate) fn forget(data_dir: &Path, session_id: &str) {
    let dir = records_dir(data_dir, session_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut repos: BTreeSet<String> = BTreeSet::new();
    for entry in entries.flatten() {
        let Some(record) = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|text| serde_json::from_str::<Record>(&text).ok())
        else {
            continue;
        };
        for repo in record.checkpoint.repos {
            repos.insert(repo.root);
        }
    }
    let prefix = format!("{REF_PREFIX}/{session_id}/");
    // The review's bases: `refs/imperium/review/<session>` and `<session>-<k>`.
    let review = format!("{}/{session_id}", crate::review::REF_PREFIX);
    for root in repos {
        let top = Path::new(&root);
        let Ok(out) = run(command(top).args([
            "for-each-ref",
            "--format=%(refname)",
            &prefix,
            crate::review::REF_PREFIX,
        ])) else {
            continue;
        };
        for name in String::from_utf8_lossy(&out).lines() {
            let reviewed = name == review || name.starts_with(&format!("{review}-"));
            if name.starts_with(&prefix) || reviewed {
                let _ = run(command(top).args(["update-ref", "-d", name]));
            }
        }
    }
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        tracing::warn!(path = %dir.display(), %error, "could not delete the session's checkpoints");
    }
}

/// `session.checkpoints`.
pub(crate) fn list(
    state: &DaemonState,
    session_id: &str,
) -> Result<SessionCheckpointsResult, String> {
    if state.session(session_id).is_none() {
        return Err(format!("no running session {session_id}"));
    }
    let Some(checkpoints) = state.checkpoints_of(session_id) else {
        let reason = if state.settings().sessions.checkpoints {
            "the session's folders are not on this machine"
        } else {
            "checkpoints are turned off (sessions.checkpoints)"
        };
        return Ok(SessionCheckpointsResult {
            available: false,
            reason: Some(reason.into()),
            checkpoints: Vec::new(),
        });
    };
    let available = !checkpoints.repositories().is_empty();
    Ok(SessionCheckpointsResult {
        available,
        reason: (!available)
            .then(|| "none of the session's folders is in a git repository".to_string()),
        checkpoints: checkpoints.list(),
    })
}

/// `session.rewind`: files first (so a failure leaves the conversation alone), then the
/// conversation, forked into a new session.
pub(crate) fn rewind(
    state: &Arc<DaemonState>,
    params: &SessionRewindParams,
) -> Result<SessionRewindResult, String> {
    let id = &params.session_id;
    if state.session(id).is_none() {
        return Err(format!("no running session {id}"));
    }
    let checkpoints = state
        .checkpoints_of(id)
        .ok_or("this session has no checkpoints")?;
    if checkpoints.turn_running() {
        return Err("a turn is running; wait for it to finish or stop it first".into());
    }
    let record = checkpoints
        .record(params.checkpoint)
        .ok_or_else(|| format!("no checkpoint {}", params.checkpoint))?;
    let files = matches!(params.mode, RewindMode::Both | RewindMode::Files);
    let conversation = matches!(params.mode, RewindMode::Both | RewindMode::Conversation);
    if conversation && !record.checkpoint.conversation {
        return Err(format!(
            "checkpoint {} was taken by a rewind and has no conversation; rewind its files only",
            params.checkpoint
        ));
    }
    let mut result = SessionRewindResult::default();
    if files {
        let (undo, restored, removed) = checkpoints.rewind_files(&record)?;
        result.undo_checkpoint = Some(undo);
        result.restored = restored;
        result.removed = removed;
    }
    if conversation {
        let messages = std::fs::read(
            checkpoints
                .dir
                .join(format!("{}.messages.json", record.checkpoint.number)),
        )
        .ok();
        let forked = state.fork_session(id, record.journal_offset, messages)?;
        result.session_id = Some(forked);
        result.prompt = Some(record.checkpoint.prompt.clone());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    fn git(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    }

    /// An index entry under a folder that is a link out of the working tree is not read.
    #[cfg(unix)]
    #[test]
    fn snapshots_do_not_follow_linked_folders_out_of_the_tree() {
        use super::*;
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "private key\n").unwrap();
        let repo = tempfile::tempdir().unwrap();
        let top = repo.path().canonicalize().unwrap();
        git(&top, &["init", "-q"]);
        std::fs::write(top.join("a.txt"), "a\n").unwrap();
        git(&top, &["add", "a.txt"]);
        // `d/secret` in the index without touching the disk, then `d` made a link outside.
        let (_, empty) = git(&top, &["hash-object", "-w", "/dev/null"]);
        let entry = format!("100644,{empty},d/secret");
        git(&top, &["update-index", "--add", "--cacheinfo", &entry]);
        std::os::unix::fs::symlink(outside.path(), top.join("d")).unwrap();
        let files = working_files(&top, &mut HashCache::default()).unwrap();
        assert!(files.contains_key("a.txt"));
        assert!(
            !files.contains_key("d/secret"),
            "nothing is read through the link"
        );
        let secret = outside.path().join("secret").display().to_string();
        let (_, id) = git(&top, &["hash-object", &secret]);
        let (stored, _) = git(&top, &["cat-file", "-e", &id]);
        assert!(!stored, "the outside file was not stored");
    }
}
