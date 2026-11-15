//! Policy pack updates: a pack fetched from a source the user configured is compared with the
//! version already pinned, shown as a diff in plain words, and applied only when the user
//! accepts it. Accepted versions are pinned by the SHA-256 of their content, which is checked
//! again every time a session loads the pack.
//!
//! This module holds the parts that need no network: reading a folder into a [`Snapshot`],
//! hashing it, the [`Diff`] between two snapshots, the user's source list ([`Sources`]) and the
//! on-disk [`PackStore`] of pins and pending updates. Fetching is the caller's job.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pack::{Pack, ROLES};

/// The manifest file a pack may carry.
const MANIFEST: &str = "imperium-workflow.toml";
/// Most files a pack may hold.
pub const MAX_FILES: usize = 2000;
/// Largest single file in a pack.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Largest pack, in bytes.
pub const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
/// Shortest hash prefix that accepts an update.
pub const MIN_HASH_PREFIX: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> UpdateError + '_ {
    move |error| UpdateError::Io(path.display().to_string(), error)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether `name` may name a source: it becomes a folder name and a pack reference prefix.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn check_name(name: &str) -> Result<(), UpdateError> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(UpdateError::Invalid(format!(
            "{name:?} cannot name a pack source: use 1-40 letters, digits, '-', '_' or '.', not starting with '.'"
        )))
    }
}

/// The files of a pack, by path relative to its folder (always `/`-separated).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    files: BTreeMap<String, Vec<u8>>,
    /// Entries left out: symbolic links, which a pack may not use.
    pub skipped: Vec<String>,
}

impl Snapshot {
    /// Builds a snapshot from `(path, content)` pairs.
    pub fn from_files<P: Into<String>, C: Into<Vec<u8>>>(
        files: impl IntoIterator<Item = (P, C)>,
    ) -> Self {
        Snapshot {
            files: files
                .into_iter()
                .map(|(path, content)| (path.into(), content.into()))
                .collect(),
            skipped: Vec::new(),
        }
    }

    /// Reads every regular file under `root`, leaving out `.git` and symbolic links, within
    /// the size limits.
    pub fn read(root: &Path) -> Result<Self, UpdateError> {
        let mut snapshot = Snapshot::default();
        let mut total = 0u64;
        read_dir(root, "", &mut snapshot, &mut total)?;
        Ok(snapshot)
    }

    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The SHA-256 of the pack: every path and the hash of its content, in path order. It
    /// covers nothing else, so it is the same wherever the files came from.
    pub fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        for (path, content) in &self.files {
            hasher.update(path.as_bytes());
            hasher.update(b"\0");
            hasher.update(hex(&Sha256::digest(content)).as_bytes());
            hasher.update(b"\n");
        }
        hex(&hasher.finalize())
    }

    /// Writes the files under `dest`, creating it.
    pub fn write(&self, dest: &Path) -> Result<(), UpdateError> {
        std::fs::create_dir_all(dest).map_err(io(dest))?;
        for (path, content) in &self.files {
            let target = dest.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io(parent))?;
            }
            std::fs::write(&target, content).map_err(io(&target))?;
        }
        Ok(())
    }
}

fn read_dir(
    dir: &Path,
    prefix: &str,
    snapshot: &mut Snapshot,
    total: &mut u64,
) -> Result<(), UpdateError> {
    let entries = std::fs::read_dir(dir).map_err(io(dir))?;
    for entry in entries {
        let entry = entry.map_err(io(dir))?;
        let full = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(UpdateError::Invalid(format!(
                "a file name in the pack is not valid text: {}",
                full.display()
            )));
        };
        if name == ".git" {
            continue;
        }
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry.file_type().map_err(io(&full))?;
        if kind.is_symlink() {
            snapshot.skipped.push(path);
        } else if kind.is_dir() {
            read_dir(&full, &path, snapshot, total)?;
        } else if kind.is_file() {
            let size = entry.metadata().map_err(io(&full))?.len();
            *total += size;
            if size > MAX_FILE_BYTES
                || *total > MAX_TOTAL_BYTES
                || snapshot.files.len() >= MAX_FILES
            {
                return Err(UpdateError::Invalid(format!(
                    "the pack is too large ({path}): at most {MAX_FILES} files, {} MiB each and {} MiB in all",
                    MAX_FILE_BYTES / (1024 * 1024),
                    MAX_TOTAL_BYTES / (1024 * 1024)
                )));
            }
            let content = std::fs::read(&full).map_err(io(&full))?;
            snapshot.files.insert(path, content);
        }
    }
    Ok(())
}

/// What kind of thing a changed file is, in the words the diff uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    /// The manifest: which files agents must load and which of the user's settings it reads.
    Manifest,
    /// The entry index, workflows and guides.
    Rules,
    /// Personalities, profiles and templates.
    Prompts,
    Lessons,
    Other,
}

impl Area {
    fn of(path: &str) -> Area {
        let first = path.split('/').next().unwrap_or(path);
        if path == MANIFEST {
            Area::Manifest
        } else if matches!(first, "personalities" | "profiles" | "templates") {
            Area::Prompts
        } else if first == "learning" {
            Area::Lessons
        } else if path.ends_with(".md") && first != "local-paths" {
            Area::Rules
        } else {
            Area::Other
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Area::Manifest => {
                "Pack settings (what agents must load, which of your settings it reads)"
            }
            Area::Rules => "Rules and workflows",
            Area::Prompts => "Prompts and personalities",
            Area::Lessons => "Lessons",
            Area::Other => "Other files",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Removed,
    Changed,
}

impl Change {
    fn word(self) -> &'static str {
        match self {
            Change::Added => "added  ",
            Change::Removed => "removed",
            Change::Changed => "changed",
        }
    }
}

/// One file that differs between two versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub change: Change,
    pub area: Area,
    /// Lines only the new version has (all of a new file's lines).
    pub added: Vec<String>,
    /// Lines only the old version has (all of a removed file's lines).
    pub removed: Vec<String>,
}

/// Something in an update the user should read before accepting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Whether it may loosen what agents are required to do. Accepting an update with any
    /// such finding needs the user's separate, explicit yes.
    pub loosening: bool,
    pub path: String,
    pub text: String,
}

/// Wording that, newly added, reduces how much agents ask or check.
const LOOSENING_ADDED: [&str; 24] = [
    "without asking",
    "without approval",
    "without permission",
    "without confirmation",
    "without confirming",
    "skip approval",
    "skip the approval",
    "no approval",
    "do not ask",
    "don't ask",
    "never ask",
    "need not ask",
    "no need to ask",
    "auto-approve",
    "approve everything",
    "bypass",
    "--no-verify",
    "force push",
    "force-push",
    "disable the safety",
    "ignore the safety",
    "ignore previous",
    "override the user",
    "dangerously",
];

/// Wording that, removed, drops a rule that makes agents ask, confirm or hold back.
const RESTRICTING_REMOVED: [&str; 18] = [
    "ask before",
    "ask first",
    "must ask",
    "always ask",
    "stop and ask",
    "explicit permission",
    "explicit approval",
    "requires approval",
    "require approval",
    "needs approval",
    "need approval",
    "confirm before",
    "never push",
    "do not push",
    "must not",
    "prohibited",
    "forbidden",
    "safety floor",
];

/// The longest line quoted in a finding.
const QUOTE: usize = 140;

fn quote(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() > QUOTE {
        format!("{}...", line.chars().take(QUOTE).collect::<String>())
    } else {
        line.to_string()
    }
}

/// The differences between a pack's pinned version and a fetched one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub old_hash: Option<String>,
    pub new_hash: String,
    pub files: Vec<FileChange>,
    pub findings: Vec<Finding>,
}

impl Diff {
    /// `old` is the version the user has pinned, if any. With none, every file is new and
    /// there are no findings: there is nothing to loosen, so the user reads the pack in full.
    pub fn between(old: Option<&Snapshot>, new: &Snapshot) -> Diff {
        let empty = Snapshot::default();
        let before = old.unwrap_or(&empty);
        let paths: BTreeSet<&String> = before.files.keys().chain(new.files.keys()).collect();
        let mut files = Vec::new();
        for path in paths {
            let (old_bytes, new_bytes) = (before.files.get(path), new.files.get(path));
            let (change, added, removed) = match (old_bytes, new_bytes) {
                (None, Some(bytes)) => (Change::Added, lines(bytes), Vec::new()),
                (Some(bytes), None) => (Change::Removed, Vec::new(), lines(bytes)),
                (Some(a), Some(b)) if a != b => {
                    let (a, b) = (lines(a), lines(b));
                    let in_a: BTreeSet<&String> = a.iter().collect();
                    let in_b: BTreeSet<&String> = b.iter().collect();
                    (
                        Change::Changed,
                        b.iter().filter(|l| !in_a.contains(l)).cloned().collect(),
                        a.iter().filter(|l| !in_b.contains(l)).cloned().collect(),
                    )
                }
                _ => continue,
            };
            files.push(FileChange {
                area: Area::of(path),
                path: path.clone(),
                change,
                added,
                removed,
            });
        }
        let findings = if old.is_some() {
            findings(before, new, &files)
        } else {
            Vec::new()
        };
        Diff {
            old_hash: old.map(Snapshot::hash),
            new_hash: new.hash(),
            files,
            findings,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn has_loosening(&self) -> bool {
        self.findings.iter().any(|finding| finding.loosening)
    }

    /// The diff in plain words. `full` also lists each changed line.
    pub fn render(&self, full: bool) -> String {
        let count = |change| self.files.iter().filter(|f| f.change == change).count();
        let mut text = if self.old_hash.is_none() {
            format!(
                "First version of this pack: {} files. Nothing is pinned yet to compare with, so \
                 read it in full (imperium pack diff <name> --full) before accepting.\n",
                self.files.len()
            )
        } else {
            format!(
                "{} files differ: {} changed, {} added, {} removed.\n",
                self.files.len(),
                count(Change::Changed),
                count(Change::Added),
                count(Change::Removed)
            )
        };
        let loosening: Vec<&Finding> = self.findings.iter().filter(|f| f.loosening).collect();
        if !loosening.is_empty() {
            text.push_str(
                "\n!! These changes may loosen what agents are required or allowed to do. \
                 Accepting needs your separate, explicit yes (--allow-loosening):\n",
            );
            for finding in loosening {
                text.push_str(&format!("  ! {}: {}\n", finding.path, finding.text));
            }
        }
        let notes: Vec<&Finding> = self.findings.iter().filter(|f| !f.loosening).collect();
        if !notes.is_empty() {
            text.push_str("\nAlso worth knowing:\n");
            for finding in notes {
                text.push_str(&format!("  - {}: {}\n", finding.path, finding.text));
            }
        }
        let mut areas: Vec<Area> = self.files.iter().map(|f| f.area).collect();
        areas.sort();
        areas.dedup();
        for area in areas {
            text.push_str(&format!("\n{}\n", area.title()));
            for file in self.files.iter().filter(|f| f.area == area) {
                text.push_str(&format!(
                    "  {} {} (+{} -{} lines)\n",
                    file.change.word(),
                    file.path,
                    file.added.len(),
                    file.removed.len()
                ));
                if full {
                    for line in &file.removed {
                        text.push_str(&format!("      - {line}\n"));
                    }
                    for line in &file.added {
                        text.push_str(&format!("      + {line}\n"));
                    }
                }
            }
        }
        if !full && !self.files.is_empty() {
            text.push_str("\nSee every changed line with: imperium pack diff <name> --full\n");
        }
        text
    }
}

fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_string)
        .collect()
}

fn role_file(path: &str) -> Option<&str> {
    let file = Path::new(path).file_name()?.to_str()?;
    let role = file.strip_prefix("AGENTS.")?.strip_suffix(".md")?;
    ROLES.contains(&role).then_some(role)
}

#[derive(Default)]
struct Facts {
    entry: Option<String>,
    roles: BTreeMap<String, BTreeSet<String>>,
    local_paths: BTreeMap<String, String>,
    unreadable: bool,
}

fn facts(snapshot: &Snapshot) -> Facts {
    let Some(bytes) = snapshot.files.get(MANIFEST) else {
        return Facts::default();
    };
    let table = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok());
    let Some(table) = table else {
        return Facts {
            unreadable: true,
            ..Facts::default()
        };
    };
    let mut facts = Facts {
        entry: table
            .get("entry")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ..Facts::default()
    };
    if let Some(roles) = table.get("roles").and_then(|v| v.as_table()) {
        for (role, files) in roles {
            let names: BTreeSet<String> = files
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f.as_str())
                .map(str::to_string)
                .collect();
            facts.roles.insert(role.clone(), names);
        }
    }
    if let Some(paths) = table.get("local_paths").and_then(|v| v.as_table()) {
        for (file, setting) in paths {
            if let Some(setting) = setting.as_str() {
                facts.local_paths.insert(file.clone(), setting.to_string());
            }
        }
    }
    facts
}

fn findings(old: &Snapshot, new: &Snapshot, files: &[FileChange]) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let mut push = |loosening: bool, path: &str, text: String| {
        out.push(Finding {
            loosening,
            path: path.to_string(),
            text,
        });
    };
    let (before, after) = (facts(old), facts(new));
    if after.unreadable && !before.unreadable {
        push(
            true,
            MANIFEST,
            "the manifest can no longer be read, so the pack would not load as before".into(),
        );
    }
    if before.entry != after.entry && !after.unreadable {
        push(
            true,
            MANIFEST,
            format!(
                "the always-loaded index changes from {} to {}",
                before.entry.as_deref().unwrap_or("AGENTS.md"),
                after.entry.as_deref().unwrap_or("AGENTS.md")
            ),
        );
    }
    let no_files = BTreeSet::new();
    let roles: BTreeSet<&String> = before.roles.keys().chain(after.roles.keys()).collect();
    for role in roles {
        let was = before.roles.get(role).unwrap_or(&no_files);
        let now = after.roles.get(role).unwrap_or(&no_files);
        for file in was.difference(now) {
            push(
                true,
                MANIFEST,
                format!("agents in the {role} role are no longer required to load {file}"),
            );
        }
        for file in now.difference(was) {
            push(
                false,
                MANIFEST,
                format!("agents in the {role} role are now required to load {file}"),
            );
        }
    }
    for (file, setting) in &after.local_paths {
        match before.local_paths.get(file) {
            None => push(
                true,
                MANIFEST,
                format!("{file} will now be filled from your setting {setting}"),
            ),
            Some(was) if was != setting => push(
                true,
                MANIFEST,
                format!("{file} will now be filled from your setting {setting} (was {was})"),
            ),
            Some(_) => {}
        }
    }
    for file in files {
        if file.change == Change::Removed
            && let Some(role) = role_file(&file.path)
        {
            push(
                true,
                &file.path,
                format!("the {role} role's instruction file is removed"),
            );
        }
        if !matches!(file.area, Area::Rules | Area::Prompts) {
            continue;
        }
        for line in &file.added {
            let low = line.to_ascii_lowercase();
            if LOOSENING_ADDED.iter().any(|phrase| low.contains(phrase)) {
                push(
                    true,
                    &file.path,
                    format!(
                        "new wording that reduces asking or checking: \"{}\"",
                        quote(line)
                    ),
                );
            }
        }
        for line in &file.removed {
            let low = line.to_ascii_lowercase();
            if RESTRICTING_REMOVED
                .iter()
                .any(|phrase| low.contains(phrase))
            {
                push(
                    true,
                    &file.path,
                    format!(
                        "a line that made agents ask or hold back is gone or reworded: \"{}\"",
                        quote(line)
                    ),
                );
            }
        }
    }
    out.truncate(200);
    out
}

/// One update source, from the user's own `pack-sources.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// A Git URL (`https://`, `ssh://`, `git@host:path` or `file://`), with no credentials.
    pub url: String,
    /// A branch or tag; none is the repository's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// Checks that `url` is one of the transports packs are fetched over.
pub fn check_url(url: &str) -> Result<(), UpdateError> {
    let bad = |why: &str| -> Result<(), UpdateError> {
        Err(UpdateError::Invalid(format!(
            "{url:?} cannot be a pack source: {why}"
        )))
    };
    if url.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("it is empty or has spaces");
    }
    if url.starts_with('-') || url.contains("::") {
        return bad("it looks like an option or a git transport helper");
    }
    let rest = ["https://", "ssh://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme));
    if let Some(rest) = rest {
        let authority = rest.split('/').next().unwrap_or(rest);
        if authority.is_empty() {
            return bad("it has no host");
        }
        // `ssh://git@host/...` names a user, not a secret; a password or a token is the problem.
        if authority.contains(':') && authority.contains('@') {
            return bad("do not put a password or token in the address");
        }
        if url.starts_with("https://") && authority.contains('@') {
            return bad("do not put credentials in the address");
        }
        return Ok(());
    }
    if url.starts_with("file://") && url.len() > "file://".len() {
        return Ok(());
    }
    if url.starts_with("git@") && url.contains(':') {
        return Ok(());
    }
    bad("use https://, ssh://, git@host:path or file://")
}

/// Checks a branch or tag name.
pub fn check_revision(revision: &str) -> Result<(), UpdateError> {
    if !revision.is_empty()
        && revision.len() <= 100
        && !revision.starts_with('-')
        && !revision.contains("..")
        && revision
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        Ok(())
    } else {
        Err(UpdateError::Invalid(format!(
            "{revision:?} is not a branch or tag name"
        )))
    }
}

/// The user's pack sources. They live in `<config>/pack-sources.toml` and nowhere else: a
/// project's files and environment variables cannot add one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    #[serde(default)]
    sources: BTreeMap<String, Source>,
}

impl Sources {
    pub const FILE: &'static str = "pack-sources.toml";

    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join(Self::FILE)
    }

    pub fn load(config_dir: &Path) -> Result<Self, UpdateError> {
        let path = Self::path(config_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Sources::default());
            }
            Err(error) => return Err(io(&path)(error)),
        };
        let sources: Sources = toml::from_str(&text)
            .map_err(|error| UpdateError::Invalid(format!("{}: {error}", path.display())))?;
        for (name, source) in &sources.sources {
            check_name(name)?;
            check_url(&source.url)?;
            if let Some(revision) = &source.revision {
                check_revision(revision)?;
            }
        }
        Ok(sources)
    }

    pub fn save(&self, config_dir: &Path) -> Result<(), UpdateError> {
        std::fs::create_dir_all(config_dir).map_err(io(config_dir))?;
        let text = toml::to_string(self).map_err(|error| {
            UpdateError::Invalid(format!("could not write the sources: {error}"))
        })?;
        write_atomic(&Self::path(config_dir), &text)
    }

    pub fn get(&self, name: &str) -> Option<&Source> {
        self.sources.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Source)> {
        self.sources.iter()
    }

    pub fn add(&mut self, name: &str, source: Source) -> Result<(), UpdateError> {
        check_name(name)?;
        check_url(&source.url)?;
        if let Some(revision) = &source.revision {
            check_revision(revision)?;
        }
        if self.sources.contains_key(name) {
            return Err(UpdateError::Invalid(format!(
                "a pack source named {name} already exists; remove it first to change it"
            )));
        }
        self.sources.insert(name.to_string(), source);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.sources.remove(name).is_some()
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), UpdateError> {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text).map_err(io(&temp))?;
    std::fs::rename(&temp, path).map_err(io(path))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// An accepted version of a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub url: String,
    /// The SHA-256 of the pack's content ([`Snapshot::hash`]).
    pub hash: String,
    /// The commit it was fetched at.
    pub commit: String,
    /// When it was accepted, in seconds since the Unix epoch.
    pub accepted: u64,
}

/// A fetched version waiting for the user's decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub url: String,
    pub hash: String,
    pub commit: String,
    pub fetched: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Pins {
    #[serde(default)]
    packs: BTreeMap<String, Pin>,
}

/// A pinned pack that is ready to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedPack {
    pub name: String,
    pub dir: PathBuf,
    pub hash: String,
}

/// The pinned packs that passed their hash check, and what was skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pinned {
    pub packs: Vec<PinnedPack>,
    pub notices: Vec<String>,
}

/// A pinned version with its content.
#[derive(Debug, Clone)]
pub struct PinnedSnapshot {
    pub pin: Pin,
    pub snapshot: Snapshot,
}

#[derive(Debug, thiserror::Error)]
pub enum AcceptError {
    #[error("{0} has no pending update; run imperium pack check first")]
    NoPending(String),
    #[error(
        "that is not the pending version ({0}); read it with imperium pack diff and pass its hash"
    )]
    HashMismatch(String),
    #[error(
        "this update has changes that may loosen what agents must do (see the marked items); read them, then accept again with --allow-loosening"
    )]
    Loosening,
    #[error("the pending update is not a loadable pack: {0}")]
    NotAPack(String),
    #[error("the staged files no longer match their hash; run imperium pack check again")]
    Tampered,
    #[error(transparent)]
    Update(#[from] UpdateError),
}

/// Pins and pending updates, under `<data>/packs/`.
#[derive(Debug, Clone)]
pub struct PackStore {
    root: PathBuf,
}

impl PackStore {
    pub fn new(data_dir: &Path) -> Self {
        PackStore {
            root: data_dir.join("packs"),
        }
    }

    /// Where fetches may be unpacked before they are staged.
    pub fn scratch(&self) -> PathBuf {
        self.root.join(".scratch")
    }

    fn pins_file(&self) -> PathBuf {
        self.root.join("pins.toml")
    }

    fn pack_dir(&self, name: &str) -> Result<PathBuf, UpdateError> {
        check_name(name)?;
        Ok(self.root.join(name))
    }

    fn pending_dir(&self, name: &str) -> Result<PathBuf, UpdateError> {
        Ok(self.pack_dir(name)?.join("pending"))
    }

    fn read_pins(&self) -> Result<Pins, UpdateError> {
        let path = self.pins_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|error| UpdateError::Invalid(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Pins::default()),
            Err(error) => Err(io(&path)(error)),
        }
    }

    fn write_pins(&self, pins: &Pins) -> Result<(), UpdateError> {
        std::fs::create_dir_all(&self.root).map_err(io(&self.root))?;
        let text = toml::to_string(pins)
            .map_err(|error| UpdateError::Invalid(format!("could not write the pins: {error}")))?;
        write_atomic(&self.pins_file(), &text)
    }

    pub fn pins(&self) -> Result<BTreeMap<String, Pin>, UpdateError> {
        Ok(self.read_pins()?.packs)
    }

    /// The pinned version of `name` with its files, checked against its hash.
    pub fn pinned_snapshot(&self, name: &str) -> Result<Option<PinnedSnapshot>, UpdateError> {
        let Some(pin) = self.read_pins()?.packs.remove(name) else {
            return Ok(None);
        };
        let dir = self.pack_dir(name)?.join(&pin.hash);
        let snapshot = Snapshot::read(&dir)?;
        if snapshot.hash() != pin.hash {
            return Err(UpdateError::Invalid(format!(
                "the files of pack {name} no longer match the hash you accepted ({}); \
                 run imperium pack check to fetch it again",
                &pin.hash[..MIN_HASH_PREFIX.min(pin.hash.len())]
            )));
        }
        Ok(Some(PinnedSnapshot { pin, snapshot }))
    }

    /// Every pinned pack whose files still match their pin. A pack that does not is skipped
    /// with a notice rather than loaded.
    pub fn pinned(&self) -> Pinned {
        let mut pinned = Pinned::default();
        let pins = match self.pins() {
            Ok(pins) => pins,
            Err(error) => {
                pinned
                    .notices
                    .push(format!("pinned packs skipped: {error}"));
                return pinned;
            }
        };
        for name in pins.keys() {
            match self.pinned_snapshot(name) {
                Ok(Some(found)) => pinned.packs.push(PinnedPack {
                    name: name.clone(),
                    dir: self.root.join(name).join(&found.pin.hash),
                    hash: found.pin.hash,
                }),
                Ok(None) => {}
                Err(error) => pinned.notices.push(format!("pack {name} skipped: {error}")),
            }
        }
        pinned
    }

    /// Keeps a fetched version for review, replacing any earlier one. Nothing is applied.
    pub fn stage(
        &self,
        name: &str,
        url: &str,
        commit: &str,
        snapshot: &Snapshot,
    ) -> Result<Pending, UpdateError> {
        let dir = self.pending_dir(name)?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io(&dir))?;
        }
        snapshot.write(&dir.join("files"))?;
        let pending = Pending {
            url: url.to_string(),
            hash: snapshot.hash(),
            commit: commit.to_string(),
            fetched: now(),
        };
        let text = toml::to_string(&pending).map_err(|error| {
            UpdateError::Invalid(format!("could not write the update: {error}"))
        })?;
        let file = dir.join("pending.toml");
        std::fs::write(&file, text).map_err(io(&file))?;
        Ok(pending)
    }

    /// The staged update for `name` with its files, checked against its hash.
    pub fn pending(&self, name: &str) -> Result<Option<(Pending, Snapshot)>, AcceptError> {
        let dir = self.pending_dir(name)?;
        let file = dir.join("pending.toml");
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io(&file)(error).into()),
        };
        let pending: Pending = toml::from_str(&text)
            .map_err(|error| UpdateError::Invalid(format!("{}: {error}", file.display())))?;
        let snapshot = Snapshot::read(&dir.join("files"))?;
        if snapshot.hash() != pending.hash {
            return Err(AcceptError::Tampered);
        }
        Ok(Some((pending, snapshot)))
    }

    /// Throws the staged update away. Returns whether there was one.
    pub fn reject(&self, name: &str) -> Result<bool, UpdateError> {
        let dir = self.pending_dir(name)?;
        if !dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir).map_err(io(&dir))?;
        Ok(true)
    }

    /// Removes everything held for `name`: its pin, its files and any staged update.
    pub fn forget(&self, name: &str) -> Result<(), UpdateError> {
        let mut pins = self.read_pins()?;
        if pins.packs.remove(name).is_some() {
            self.write_pins(&pins)?;
        }
        let dir = self.pack_dir(name)?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io(&dir))?;
        }
        Ok(())
    }

    /// Pins the staged update. `hash` is what the user read: a prefix of at least
    /// [`MIN_HASH_PREFIX`] hex digits of the staged version's hash, so an update that changed
    /// since is not accepted by mistake. Changes that may loosen what agents must do need
    /// `allow_loosening` as well.
    pub fn accept(
        &self,
        name: &str,
        hash: &str,
        allow_loosening: bool,
    ) -> Result<Pin, AcceptError> {
        let (pending, snapshot) = self
            .pending(name)?
            .ok_or_else(|| AcceptError::NoPending(name.to_string()))?;
        let hash = hash
            .trim()
            .trim_start_matches("sha256:")
            .to_ascii_lowercase();
        if hash.len() < MIN_HASH_PREFIX || !pending.hash.starts_with(&hash) {
            return Err(AcceptError::HashMismatch(
                pending.hash[..MIN_HASH_PREFIX].to_string(),
            ));
        }
        // A pin that no longer verifies is no baseline; the explicit hash is the user's decision.
        let old = self.pinned_snapshot(name).ok().flatten();
        let diff = Diff::between(old.as_ref().map(|found| &found.snapshot), &snapshot);
        if diff.has_loosening() && !allow_loosening {
            return Err(AcceptError::Loosening);
        }
        let staged = self.pending_dir(name)?.join("files");
        Pack::load(&staged).map_err(|error| AcceptError::NotAPack(error.to_string()))?;

        let target = self.pack_dir(name)?.join(&pending.hash);
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(io(&target))?;
        }
        std::fs::rename(&staged, &target).map_err(io(&target))?;
        let pin = Pin {
            url: pending.url,
            hash: pending.hash,
            commit: pending.commit,
            accepted: now(),
        };
        let mut pins = self.read_pins()?;
        let previous = pins.packs.insert(name.to_string(), pin.clone());
        self.write_pins(&pins)?;
        if let Some(previous) = previous
            && previous.hash != pin.hash
        {
            let _ = std::fs::remove_dir_all(self.pack_dir(name)?.join(&previous.hash));
        }
        let _ = std::fs::remove_dir_all(self.pending_dir(name)?);
        Ok(pin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(files: &[(&str, &str)]) -> Snapshot {
        let mut all = vec![(
            "AGENTS.md",
            "# Index\n\n- Leaf: [Leaf](workflows/AGENTS.leaf.md)\n",
        )];
        all.extend_from_slice(files);
        Snapshot::from_files(all)
    }

    #[test]
    fn the_hash_covers_paths_and_content_only() {
        let a = Snapshot::from_files([("a.md", "one"), ("b/c.md", "two")]);
        let b = Snapshot::from_files([("b/c.md", "two"), ("a.md", "one")]);
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.hash().len(), 64);
        let changed = Snapshot::from_files([("a.md", "one!"), ("b/c.md", "two")]);
        assert_ne!(a.hash(), changed.hash());
        let renamed = Snapshot::from_files([("a2.md", "one"), ("b/c.md", "two")]);
        assert_ne!(a.hash(), renamed.hash());
    }

    #[test]
    fn reading_a_folder_skips_git_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = pack(&[
            ("workflows/AGENTS.leaf.md", "leaf\n"),
            ("guides/a.md", "a\n"),
        ]);
        snapshot.write(dir.path()).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "x").unwrap();
        let read = Snapshot::read(dir.path()).unwrap();
        assert_eq!(read.files(), snapshot.files());
        assert_eq!(read.hash(), snapshot.hash());
    }

    #[test]
    fn a_diff_lists_added_removed_and_changed_files_in_plain_words() {
        let old = pack(&[
            ("guides/a.md", "keep\nold line\n"),
            ("guides/gone.md", "bye\n"),
        ]);
        let new = pack(&[
            ("guides/a.md", "keep\nnew line\n"),
            ("personalities/p.md", "voice\n"),
        ]);
        let diff = Diff::between(Some(&old), &new);
        let by_path: BTreeMap<&str, &FileChange> =
            diff.files.iter().map(|f| (f.path.as_str(), f)).collect();
        assert_eq!(by_path["guides/a.md"].change, Change::Changed);
        assert_eq!(by_path["guides/a.md"].added, ["new line"]);
        assert_eq!(by_path["guides/a.md"].removed, ["old line"]);
        assert_eq!(by_path["guides/gone.md"].change, Change::Removed);
        assert_eq!(by_path["personalities/p.md"].change, Change::Added);
        assert_eq!(by_path["personalities/p.md"].area, Area::Prompts);
        assert!(!diff.has_loosening());
        let text = diff.render(true);
        assert!(
            text.contains("3 files differ: 1 changed, 1 added, 1 removed."),
            "{text}"
        );
        assert!(text.contains("Rules and workflows"));
        assert!(text.contains("Prompts and personalities"));
        assert!(text.contains("+ new line") && text.contains("- old line"));
        assert!(!diff.render(false).contains("+ new line"));
    }

    #[test]
    fn identical_packs_have_an_empty_diff() {
        let a = pack(&[("guides/a.md", "x\n")]);
        assert!(Diff::between(Some(&a), &a.clone()).is_empty());
    }

    #[test]
    fn a_first_version_has_no_findings_and_says_to_read_it() {
        let new = pack(&[("guides/a.md", "do this without asking\n")]);
        let diff = Diff::between(None, &new);
        assert!(diff.findings.is_empty());
        assert!(diff.render(false).contains("First version"));
    }

    #[test]
    fn loosening_wording_and_dropped_restrictions_are_highlighted() {
        let old = pack(&[("guides/git.md", "Ask before every push.\nBe brief.\n")]);
        let new = pack(&[(
            "guides/git.md",
            "Push whenever, without asking.\nBe brief.\n",
        )]);
        let diff = Diff::between(Some(&old), &new);
        assert!(diff.has_loosening());
        let text = diff.render(false);
        assert!(text.contains("may loosen"), "{text}");
        assert!(text.contains("without asking") && text.contains("Ask before every push"));
        assert!(text.contains("--allow-loosening"));
    }

    #[test]
    fn tightening_alone_is_not_loosening() {
        let old = pack(&[("guides/git.md", "Push freely.\n")]);
        let new = pack(&[("guides/git.md", "Ask before every push.\nPush freely.\n")]);
        assert!(!Diff::between(Some(&old), &new).has_loosening());
    }

    #[test]
    fn manifest_changes_that_weaken_requirements_or_read_settings_are_loosening() {
        let old = pack(&[(
            MANIFEST,
            "entry = \"AGENTS.md\"\n[roles]\nleaf = [\"workflows/AGENTS.leaf.md\", \"guides/safety.md\"]\n",
        )]);
        let new = pack(&[(
            MANIFEST,
            "entry = \"OTHER.md\"\n[roles]\nleaf = [\"workflows/AGENTS.leaf.md\", \"guides/new.md\"]\n[local_paths]\n\"x.md\" = \"worktrees.custom_path\"\n",
        )]);
        let diff = Diff::between(Some(&old), &new);
        let text = diff.render(false);
        assert!(
            text.contains("always-loaded index changes from AGENTS.md to OTHER.md"),
            "{text}"
        );
        assert!(text.contains("no longer required to load guides/safety.md"));
        assert!(text.contains("filled from your setting worktrees.custom_path"));
        // A newly required file is a note, not a loosening.
        let note = diff
            .findings
            .iter()
            .find(|f| f.text.contains("now required to load guides/new.md"))
            .unwrap();
        assert!(!note.loosening);
    }

    #[test]
    fn removing_a_role_file_is_loosening() {
        let old = pack(&[("workflows/AGENTS.leaf.md", "leaf\n")]);
        let new = pack(&[]);
        let diff = Diff::between(Some(&old), &new);
        assert!(diff.has_loosening());
        assert!(
            diff.render(false)
                .contains("leaf role's instruction file is removed")
        );
    }

    #[test]
    fn source_urls_and_names_are_checked() {
        for url in [
            "https://github.com/a/b",
            "ssh://git@host/a/b",
            "git@github.com:a/b.git",
            "file:///tmp/pack",
        ] {
            assert!(check_url(url).is_ok(), "{url}");
        }
        for url in [
            "http://github.com/a/b",
            "https://user:token@github.com/a/b",
            "https://token@github.com/a/b",
            "ext::sh -c evil",
            "-oProxyCommand=evil",
            "https://a b",
            "/local/folder",
            "",
        ] {
            assert!(check_url(url).is_err(), "{url}");
        }
        assert!(valid_name("acw") && valid_name("my_pack-1.2"));
        let long = "x".repeat(41);
        for name in ["", ".hidden", "a/b", "..", "a b", long.as_str()] {
            assert!(!valid_name(name), "{name}");
        }
        assert!(check_revision("main").is_ok() && check_revision("v1.2/x").is_ok());
        assert!(check_revision("--upload-pack=x").is_err() && check_revision("a..b").is_err());
    }

    #[test]
    fn sources_round_trip_and_reject_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = Sources::load(dir.path()).unwrap();
        assert_eq!(sources.iter().count(), 0);
        let source = Source {
            url: "https://github.com/a/b".into(),
            revision: Some("main".into()),
        };
        sources.add("acw", source.clone()).unwrap();
        assert!(sources.add("acw", source.clone()).is_err());
        assert!(sources.add("bad name", source.clone()).is_err());
        sources.save(dir.path()).unwrap();
        let mut again = Sources::load(dir.path()).unwrap();
        assert_eq!(again.get("acw"), Some(&source));
        assert!(again.remove("acw") && !again.remove("acw"));
    }

    #[test]
    fn a_hand_edited_source_with_a_bad_url_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            Sources::path(dir.path()),
            "[sources.evil]\nurl = \"ext::sh -c touch /tmp/x\"\n",
        )
        .unwrap();
        assert!(Sources::load(dir.path()).is_err());
    }

    fn stage(store: &PackStore, snapshot: &Snapshot) -> Pending {
        store
            .stage("acw", "https://example.com/acw", "abc123", snapshot)
            .unwrap()
    }

    #[test]
    fn nothing_is_pinned_until_accept_and_accept_needs_the_hash() {
        let data = tempfile::tempdir().unwrap();
        let store = PackStore::new(data.path());
        let v1 = pack(&[("guides/a.md", "one\n")]);
        let pending = stage(&store, &v1);
        assert!(store.pinned().packs.is_empty(), "staged is not pinned");

        assert!(matches!(
            store.accept("acw", "deadbeefdeadbeef", false),
            Err(AcceptError::HashMismatch(_))
        ));
        assert!(matches!(
            store.accept("acw", &pending.hash[..8], false),
            Err(AcceptError::HashMismatch(_))
        ));
        let pin = store.accept("acw", &pending.hash[..16], false).unwrap();
        assert_eq!(pin.hash, v1.hash());
        let pinned = store.pinned();
        assert_eq!(pinned.packs.len(), 1);
        assert_eq!(pinned.packs[0].name, "acw");
        assert!(Pack::load(&pinned.packs[0].dir).is_ok());
        assert!(
            store.pending("acw").unwrap().is_none(),
            "pending is cleared"
        );
        assert!(matches!(
            store.accept("acw", &pending.hash, false),
            Err(AcceptError::NoPending(_))
        ));
    }

    #[test]
    fn loosening_updates_need_a_separate_yes_and_rejecting_changes_nothing() {
        let data = tempfile::tempdir().unwrap();
        let store = PackStore::new(data.path());
        let v1 = pack(&[("guides/git.md", "Ask before every push.\n")]);
        let first = stage(&store, &v1);
        store.accept("acw", &first.hash, false).unwrap();

        let v2 = pack(&[("guides/git.md", "Push without asking.\n")]);
        let second = stage(&store, &v2);
        assert!(matches!(
            store.accept("acw", &second.hash, false),
            Err(AcceptError::Loosening)
        ));
        assert_eq!(store.pins().unwrap()["acw"].hash, v1.hash(), "still v1");
        assert!(store.reject("acw").unwrap());
        assert!(!store.reject("acw").unwrap());
        assert_eq!(store.pinned().packs[0].hash, v1.hash());

        stage(&store, &v2);
        let pin = store.accept("acw", &second.hash, true).unwrap();
        assert_eq!(pin.hash, v2.hash());
        let dirs: Vec<_> = std::fs::read_dir(data.path().join("packs/acw"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(dirs.len(), 1, "the old version's files are removed");
    }

    #[test]
    fn a_pinned_pack_whose_files_changed_is_not_loaded() {
        let data = tempfile::tempdir().unwrap();
        let store = PackStore::new(data.path());
        let pending = stage(&store, &pack(&[("guides/a.md", "one\n")]));
        store.accept("acw", &pending.hash, false).unwrap();
        let file = data
            .path()
            .join("packs/acw")
            .join(&pending.hash)
            .join("guides/a.md");
        std::fs::write(file, "tampered\n").unwrap();
        let pinned = store.pinned();
        assert!(pinned.packs.is_empty());
        assert!(
            pinned.notices[0].contains("no longer match"),
            "{:?}",
            pinned.notices
        );
    }

    #[test]
    fn a_staged_update_whose_files_changed_is_not_accepted() {
        let data = tempfile::tempdir().unwrap();
        let store = PackStore::new(data.path());
        let pending = stage(&store, &pack(&[("guides/a.md", "one\n")]));
        let file = data.path().join("packs/acw/pending/files/guides/a.md");
        std::fs::write(file, "swapped\n").unwrap();
        assert!(matches!(
            store.accept("acw", &pending.hash, true),
            Err(AcceptError::Tampered)
        ));
    }

    #[test]
    fn a_pack_that_does_not_load_is_not_accepted() {
        let data = tempfile::tempdir().unwrap();
        let store = PackStore::new(data.path());
        let broken = Snapshot::from_files([("README.md", "no entry index here\n")]);
        let pending = stage(&store, &broken);
        assert!(matches!(
            store.accept("acw", &pending.hash, true),
            Err(AcceptError::NotAPack(_))
        ));
    }

    #[test]
    fn store_names_cannot_escape_the_store() {
        let data = tempfile::tempdir().unwrap();
        let store = PackStore::new(data.path());
        assert!(store.reject("../x").is_err());
        assert!(store.forget("a/b").is_err());
    }

    #[test]
    fn forgetting_a_pack_removes_its_pin_and_files() {
        let data = tempfile::tempdir().unwrap();
        let store = PackStore::new(data.path());
        let pending = stage(&store, &pack(&[]));
        store.accept("acw", &pending.hash, false).unwrap();
        store.forget("acw").unwrap();
        assert!(store.pins().unwrap().is_empty());
        assert!(!data.path().join("packs/acw").exists());
    }
}
