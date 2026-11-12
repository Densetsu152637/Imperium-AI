//! Folding the older project memory into the environment's `Memory.md`
//! (docs/17-memory.md#environment-memory-implemented).
//!
//! Memory used to have two project scopes besides the environment's file: the user's private
//! notes about a project folder (`<data>/memory/projects/<id>/`, one file per entry) and the
//! project's shared notes (`<project>/.imperium/memory/`, which can be committed). There is one
//! scope for a project now, the environment's, so the entries of both are merged into the
//! `Memory.md` of the environment the folder belongs to:
//!
//! - an entry whose name is not in `Memory.md` becomes a `## <name>` section;
//! - one whose name is there with the same body is a duplicate and is skipped;
//! - one whose name is there with a different body is kept as well, under the name with a
//!   `-legacy` suffix (`-legacy-2`, ...), so neither version is lost;
//! - one that does not fit (a body over 2,000 characters, a name `Memory.md` cannot hold, text
//!   that looks like a secret, or a file that would pass 24,000 characters) goes to
//!   `Memory.archive.md` beside `Memory.md`, which agents do not read into their instructions.
//!
//! Before anything is merged the source folder is copied byte for byte to
//! `<data>/memory/legacy/<label>/`. The private originals are removed once their entries are in
//! `Memory.md`, the archive and that backup; a project's shared folder is in its repository and
//! is left where it is. Each merged source is recorded in `<data>/memory/migration.json`, and a
//! recorded source is never merged again, so the migration can run at every start. If the
//! record is lost, running again changes nothing either: the duplicates are recognised.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::environment::{
    EnvironmentMemory, Section, read_existing, section_text, sections, upsert_section, within_cap,
    write_atomic,
};
use crate::{Entry, KINDS, MemoryError, Scope, io, project_id, read_entry, today};

/// The migration's record, in `<data>/memory/`.
pub const RECORD_FILE: &str = "migration.json";

/// Where backups of merged source folders are kept, in `<data>/memory/`.
pub const BACKUP_DIR: &str = "legacy";

/// Records take turns, so two migrations cannot overwrite each other's line.
static RECORD: Mutex<()> = Mutex::new(());

/// Which of the two older scopes a folder of entries was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// `<data>/memory/projects/<id>/`: the user's own notes about a project.
    ProjectPrivate,
    /// `<project>/.imperium/memory/`: the project's shared notes, in its repository.
    ProjectShared,
}

/// A folder of older memory entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub dir: PathBuf,
    pub kind: SourceKind,
    /// A folder name for its backup.
    label: String,
}

/// The two folders older memory of the project at `folder` was kept in.
pub fn sources_for_folder(data_dir: &Path, folder: &Path) -> [Source; 2] {
    let id = project_id(folder);
    [
        Source {
            dir: private_root(data_dir).join(&id),
            kind: SourceKind::ProjectPrivate,
            label: id.clone(),
        },
        Source {
            dir: folder.join(".imperium").join("memory"),
            kind: SourceKind::ProjectShared,
            label: format!("{id}-shared"),
        },
    ]
}

fn private_root(data_dir: &Path) -> PathBuf {
    data_dir.join("memory").join("projects")
}

/// What one merge did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migrated {
    /// The source folder.
    pub source: String,
    pub kind: SourceKind,
    pub environment: String,
    /// The day it ran, `YYYY-MM-DD`.
    pub date: String,
    /// Entries found in the source.
    pub entries: usize,
    /// Became sections of `Memory.md`.
    pub added: usize,
    /// Skipped: `Memory.md` already had a section of that name with the same body.
    pub duplicates: usize,
    /// Of those that added, the ones kept beside a different section of the same name, under a
    /// `-legacy` name.
    pub kept_both: usize,
    /// Moved to `Memory.archive.md` because they did not fit.
    pub archived: usize,
    /// Where the source folder was copied.
    pub backup: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    migrated: Vec<Migrated>,
}

fn record_path(data_dir: &Path) -> PathBuf {
    data_dir.join("memory").join(RECORD_FILE)
}

fn read_record(data_dir: &Path) -> Record {
    std::fs::read_to_string(record_path(data_dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_record(data_dir: &Path, record: &Record) -> Result<(), MemoryError> {
    let text = serde_json::to_string_pretty(record).unwrap_or_default();
    // Flushed to disk before the rename, as Memory.md and the archive are.
    write_atomic(&record_path(data_dir), &text)
}

/// Every merge recorded so far.
pub fn recorded(data_dir: &Path) -> Vec<Migrated> {
    read_record(data_dir).migrated
}

/// The key a source is recorded under.
fn key(dir: &Path) -> String {
    dir.canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .display()
        .to_string()
}

/// The entries in `dir` (one `.md` file each, not `INDEX.md`), sorted by name.
fn read_all(dir: &Path) -> Vec<Entry> {
    let Ok(files) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = files
        .flatten()
        .map(|file| file.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "md")
                && path.file_name().is_some_and(|name| name != "INDEX.md")
        })
        .filter_map(|path| read_entry(&path, Scope::Environment))
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    entries
}

/// Folders of private project memory that belong to none of `folders` (the folders of the
/// environments there are), with entries in them. They are left alone: the folder's name is a
/// hash of its path, so they cannot be matched to a folder, and they are merged by
/// [`migrate`] when an environment is given the folder.
pub fn unclaimed(data_dir: &Path, folders: &[PathBuf]) -> Vec<PathBuf> {
    let claimed: Vec<PathBuf> = folders
        .iter()
        .map(|folder| private_root(data_dir).join(project_id(folder)))
        .collect();
    let Ok(dirs) = std::fs::read_dir(private_root(data_dir)) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = dirs
        .flatten()
        .map(|dir| dir.path())
        .filter(|path| path.is_dir() && !claimed.contains(path) && !read_all(path).is_empty())
        .collect();
    found.sort();
    found
}

/// Merges the entries of `source` into `memory`, the `Memory.md` of environment
/// `environment_id`, as described in the module documentation. `None` when there was nothing
/// to do: the source is already recorded as merged, or has no entries.
pub fn migrate(
    data_dir: &Path,
    environment_id: &str,
    memory: &EnvironmentMemory,
    source: &Source,
) -> Result<Option<Migrated>, MemoryError> {
    let _turn = RECORD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let source_key = key(&source.dir);
    let mut record = read_record(data_dir);
    if record.migrated.iter().any(|done| done.source == source_key) {
        return Ok(None);
    }
    let entries = read_all(&source.dir);
    if entries.is_empty() {
        return Ok(None);
    }

    // The originals are copied before anything is merged, so nothing below can lose them.
    let backup = data_dir.join("memory").join(BACKUP_DIR).join(&source.label);
    copy_folder(&source.dir, &backup)?;

    let mut counts = Migrated {
        source: source_key,
        kind: source.kind,
        environment: environment_id.to_string(),
        date: today(),
        entries: entries.len(),
        added: 0,
        duplicates: 0,
        kept_both: 0,
        archived: 0,
        backup: backup.display().to_string(),
    };
    let mut overflow: Vec<(Entry, String)> = Vec::new();
    memory.update(|existing| {
        let mut text = existing.to_string();
        let mut changed = false;
        for entry in &entries {
            match place(&mut text, entry) {
                Placed::Added { renamed } => {
                    counts.added += 1;
                    counts.kept_both += usize::from(renamed);
                    changed = true;
                }
                Placed::Duplicate => counts.duplicates += 1,
                Placed::Overflow(reason) => {
                    counts.archived += 1;
                    overflow.push((entry.clone(), reason));
                }
            }
        }
        Ok(changed.then_some(text))
    })?;
    append_overflow(memory, &overflow, &source.label)?;

    record.version = 1;
    record.migrated.push(counts.clone());
    write_record(data_dir, &record)?;

    // The entries are in Memory.md, the archive and the backup: the private originals are no
    // longer needed. A project's shared notes are in its repository and stay.
    if source.kind == SourceKind::ProjectPrivate {
        for entry in &entries {
            let _ = std::fs::remove_file(&entry.path);
        }
        let _ = std::fs::remove_file(source.dir.join("INDEX.md"));
        let _ = std::fs::remove_dir(&source.dir);
    }
    Ok(Some(counts))
}

enum Placed {
    /// Now a section of the file; `renamed` when it got a `-legacy` name.
    Added {
        renamed: bool,
    },
    Duplicate,
    /// Does not fit; why.
    Overflow(String),
}

/// Puts `entry` into `text` (the text of `Memory.md`) by the rules in the module documentation.
fn place(text: &mut String, entry: &Entry) -> Placed {
    // Windows checkouts may hold the files with CRLF line ends; the checks and the dedupe compare
    // the text with LF (the archive keeps the original).
    let normalised = entry.body.replace("\r\n", "\n");
    let body = normalised.trim();
    let description = match entry.description.trim() {
        "" => entry.name.as_str(),
        description => description,
    };
    let kind = if KINDS.contains(&entry.kind.as_str()) {
        entry.kind.as_str()
    } else {
        "project"
    };
    // The name it would get, or a duplicate when a section of that name (or of one of the
    // `-legacy` names) already has this body.
    let mut name = entry.name.clone();
    let mut renamed = false;
    for attempt in 1.. {
        match sections(text).into_iter().find(|s| s.name == name) {
            None => break,
            Some(found) if found.body.trim() == body => return Placed::Duplicate,
            Some(_) => {
                renamed = true;
                name = legacy_name(&entry.name, attempt);
            }
        }
    }
    if let Err(error) = EnvironmentMemory::check(&name, description, kind, body) {
        return Placed::Overflow(error.to_string());
    }
    let section = section_text(&name, description, kind, body);
    let (updated, _) = upsert_section(text, &name, &section);
    if let Err(error) = within_cap(&updated) {
        return Placed::Overflow(error.to_string());
    }
    // The body must not change how the file divides into sections: it adds exactly the one
    // section, whole, and leaves every other as it was; and its code fences must balance, or
    // everything written after it would read as code.
    let before = sections(text);
    let after = sections(&updated);
    let others_before: Vec<&Section> = before.iter().filter(|s| s.name != name).collect();
    let others_after: Vec<&Section> = after.iter().filter(|s| s.name != name).collect();
    let mine: Vec<&Section> = after.iter().filter(|s| s.name == name).collect();
    let fences = body
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count();
    if fences % 2 != 0
        || mine.len() != 1
        || mine[0].body.trim() != body
        || others_before != others_after
    {
        return Placed::Overflow("would change Memory.md's structure".into());
    }
    *text = updated;
    Placed::Added { renamed }
}

/// `name` with the `-legacy` suffix, numbered from the second attempt, within 64 characters.
fn legacy_name(name: &str, attempt: usize) -> String {
    let suffix = if attempt <= 1 {
        "-legacy".to_string()
    } else {
        format!("-legacy-{attempt}")
    };
    let base: String = name.chars().take(64 - suffix.len()).collect();
    format!("{}{suffix}", base.trim_end_matches('-'))
}

/// Appends the entries that did not fit to `Memory.archive.md`, skipping any already there.
fn append_overflow(
    memory: &EnvironmentMemory,
    overflow: &[(Entry, String)],
    label: &str,
) -> Result<(), MemoryError> {
    if overflow.is_empty() {
        return Ok(());
    }
    let archived = read_existing(&memory.archive_file())?;
    let mut text = String::new();
    for (entry, reason) in overflow {
        let section = section_text(
            &entry.name,
            entry.description.trim(),
            &entry.kind,
            entry.body.trim(),
        );
        if archived.contains(&section) {
            continue;
        }
        text.push_str(&format!(
            "<!-- from {label}; not in Memory.md because: {} -->\n{section}\n",
            reason.replace("-->", "--")
        ));
    }
    memory.append_archive(&text)
}

/// Copies the files of `from` to `to`, creating it.
fn copy_folder(from: &Path, to: &Path) -> Result<(), MemoryError> {
    std::fs::create_dir_all(to).map_err(|error| io(to, error))?;
    let files = std::fs::read_dir(from).map_err(|error| io(from, error))?;
    for file in files.flatten() {
        let path = file.path();
        if path.is_file() {
            let target = to.join(file.file_name());
            std::fs::copy(&path, &target).map_err(|error| io(&target, error))?;
            // On disk, not only in the cache, before the originals can be removed.
            // (Opened for writing: Windows refuses to flush a file opened only for reading.)
            std::fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .and_then(|copy| copy.sync_all())
                .map_err(|error| io(&target, error))?;
        }
    }
    // The folder itself, where the platform lets a folder be opened.
    if let Ok(folder) = std::fs::File::open(to) {
        let _ = folder.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::{FILE_NAME, MAX_BODY_CHARS, MAX_CONTEXT_CHARS};

    struct Setup {
        data: tempfile::TempDir,
        project: tempfile::TempDir,
        memory: EnvironmentMemory,
    }

    fn setup() -> Setup {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let memory =
            EnvironmentMemory::new(data.path().join("workspaces").join("env").join(FILE_NAME));
        Setup {
            data,
            project,
            memory,
        }
    }

    fn write_entry(dir: &Path, name: &str, description: &str, kind: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.md")),
            format!(
                "---\nname: {name}\ndescription: {description}\ntype: {kind}\nscope: project-private\ncreated: 2026-01-02\nsource: test\n---\n\n{body}\n"
            ),
        )
        .unwrap();
    }

    fn private(setup: &Setup) -> Source {
        sources_for_folder(setup.data.path(), setup.project.path())[0].clone()
    }

    fn shared(setup: &Setup) -> Source {
        sources_for_folder(setup.data.path(), setup.project.path())[1].clone()
    }

    fn run(setup: &Setup, source: &Source) -> Option<Migrated> {
        migrate(setup.data.path(), "env", &setup.memory, source).unwrap()
    }

    #[test]
    fn private_entries_are_merged_and_the_originals_backed_up() {
        let setup = setup();
        let source = private(&setup);
        write_entry(
            &source.dir,
            "build",
            "How it builds",
            "project",
            "Run make.",
        );
        write_entry(&source.dir, "owner", "Who owns it", "reference", "Ask Sam.");
        std::fs::create_dir_all(setup.memory.file().parent().unwrap()).unwrap();
        std::fs::write(setup.memory.file(), "# Notes\n\nhand written\n").unwrap();

        let done = run(&setup, &source).expect("migrated");
        assert_eq!(
            (done.entries, done.added, done.duplicates, done.archived),
            (2, 2, 0, 0)
        );
        let text = setup.memory.read();
        assert!(text.starts_with("# Notes\n\nhand written"), "{text}");
        assert!(
            text.contains("## build\n_How it builds (project)_\n\nRun make."),
            "{text}"
        );
        assert!(
            text.contains("## owner") && text.contains("Ask Sam."),
            "{text}"
        );
        // The originals are in the backup, byte for byte, and gone from the old place.
        let backup = PathBuf::from(&done.backup);
        assert!(backup.join("build.md").is_file() && backup.join("owner.md").is_file());
        assert!(!source.dir.exists());
        assert_eq!(recorded(setup.data.path()).len(), 1);
        assert_eq!(recorded(setup.data.path())[0].environment, "env");
    }

    #[test]
    fn it_runs_again_without_changing_anything() {
        let setup = setup();
        let source = private(&setup);
        write_entry(
            &source.dir,
            "build",
            "How it builds",
            "project",
            "Run make.",
        );
        run(&setup, &source).expect("first run");
        let text = setup.memory.read();
        assert_eq!(run(&setup, &source), None, "recorded sources are skipped");
        assert_eq!(setup.memory.read(), text);

        // Even with the record lost and the originals back, nothing is added twice.
        std::fs::remove_file(record_path(setup.data.path())).unwrap();
        write_entry(
            &source.dir,
            "build",
            "How it builds",
            "project",
            "Run make.",
        );
        let again = run(&setup, &source).expect("runs again without the record");
        assert_eq!((again.added, again.duplicates), (0, 1));
        assert_eq!(setup.memory.read(), text);
        assert!(!setup.memory.archive_file().exists());
    }

    #[test]
    fn the_same_name_with_the_same_body_is_one_section_and_a_different_body_keeps_both() {
        let setup = setup();
        setup
            .memory
            .remember("same", "already here", "project", "Same body.")
            .unwrap();
        setup
            .memory
            .remember("differs", "already here", "project", "Environment version.")
            .unwrap();
        let source = private(&setup);
        write_entry(&source.dir, "same", "old copy", "project", "Same body.");
        write_entry(
            &source.dir,
            "differs",
            "old copy",
            "project",
            "Project version.",
        );
        let done = run(&setup, &source).unwrap();
        assert_eq!((done.added, done.duplicates, done.kept_both), (1, 1, 1));
        let text = setup.memory.read();
        assert_eq!(text.matches("## same").count(), 1, "{text}");
        assert!(text.contains("Environment version."), "{text}");
        assert!(
            text.contains("## differs-legacy\n_old copy (project)_\n\nProject version."),
            "{text}"
        );

        // A second source with yet another version of the name gets the next suffix.
        let shared = shared(&setup);
        write_entry(
            &shared.dir,
            "differs",
            "shared copy",
            "project",
            "Shared version.",
        );
        run(&setup, &shared).unwrap();
        assert_eq!(
            run(&setup, &shared),
            None,
            "a recorded source is not merged again"
        );
        let text = setup.memory.read();
        assert!(
            text.contains("## differs-legacy-2") && text.contains("Shared version."),
            "{text}"
        );
        // The shared notes live in the repository and stay there.
        assert!(shared.dir.join("differs.md").is_file());
    }

    #[test]
    fn what_does_not_fit_goes_to_the_archive_and_nothing_is_lost() {
        let setup = setup();
        // A file nearly at its cap.
        let filler = "f".repeat(MAX_BODY_CHARS);
        let mut index = 0;
        while setup
            .memory
            .check_fits(&format!("filler-{index}"), "d", "project", &filler)
            .is_ok()
        {
            setup
                .memory
                .remember(&format!("filler-{index}"), "d", "project", &filler)
                .unwrap();
            index += 1;
        }
        let source = private(&setup);
        let long = "L".repeat(MAX_BODY_CHARS + 500);
        let key = "token sk-abcdefghijklmnopqrstuvwxyz123456";
        let entries = [
            ("fits", "Short", "project", "short"),
            ("too-long", "Over the entry limit", "project", long.as_str()),
            ("key", "A key", "reference", key),
            ("Bad Name", "Cannot be a heading name", "project", "odd"),
        ];
        for (name, description, kind, body) in entries {
            write_entry(&source.dir, name, description, kind, body);
        }

        let done = run(&setup, &source).unwrap();
        assert_eq!(done.entries, 4);
        assert_eq!(done.added + done.archived + done.duplicates, 4);
        assert!(done.archived >= 3, "the long, secret and oddly named ones");
        let text = setup.memory.read();
        assert!(text.chars().count() <= MAX_CONTEXT_CHARS);
        let archive = std::fs::read_to_string(setup.memory.archive_file()).unwrap();
        assert!(archive.starts_with("# Memory archive"), "{archive}");
        // Every entry's body is in Memory.md or in the archive.
        for body in [
            long.as_str(),
            "short",
            "sk-abcdefghijklmnopqrstuvwxyz123456",
            "odd",
        ] {
            assert!(
                text.contains(body) || archive.contains(body),
                "{body} was lost"
            );
        }
        assert!(archive.contains(&long) && archive.contains("sk-abcdef"));
        assert!(
            !text.contains("sk-abcdef"),
            "a secret never reaches Memory.md"
        );
        assert!(archive.contains("Bad Name"));
        // Running again archives nothing twice.
        std::fs::remove_file(record_path(setup.data.path())).unwrap();
        for (name, description, kind, body) in entries {
            write_entry(&source.dir, name, description, kind, body);
        }
        run(&setup, &source).unwrap();
        let archive_again = std::fs::read_to_string(setup.memory.archive_file()).unwrap();
        assert_eq!(archive_again, archive, "nothing is archived twice");
        assert_eq!(setup.memory.read(), text);
    }

    #[test]
    fn nothing_happens_without_entries_and_the_folder_is_made_when_missing() {
        let setup = setup();
        let source = private(&setup);
        assert_eq!(run(&setup, &source), None, "a source that is not there");
        assert!(recorded(setup.data.path()).is_empty());
        assert!(!setup.memory.file().exists());
        // An empty folder is not recorded either, so entries put there later are still merged.
        std::fs::create_dir_all(&source.dir).unwrap();
        std::fs::write(source.dir.join("INDEX.md"), "# Memory index\n").unwrap();
        assert_eq!(run(&setup, &source), None);
        write_entry(&source.dir, "late", "Arrived later", "lesson", "Later.");
        assert!(run(&setup, &source).is_some());
        assert!(
            setup.memory.file().is_file(),
            "Memory.md and its folder are made"
        );
    }

    #[test]
    fn private_folders_that_belong_to_no_environment_are_left_alone() {
        let setup = setup();
        let claimed = private(&setup);
        write_entry(
            &claimed.dir,
            "mine",
            "In an environment",
            "project",
            "Mine.",
        );
        let other = setup
            .data
            .path()
            .join("memory/projects/elsewhere-0123456789abcdef");
        write_entry(&other, "orphan", "Nobody's folder", "project", "Orphan.");
        let empty = setup
            .data
            .path()
            .join("memory/projects/empty-fedcba9876543210");
        std::fs::create_dir_all(&empty).unwrap();

        let folders = vec![setup.project.path().to_path_buf()];
        assert_eq!(unclaimed(setup.data.path(), &folders), vec![other.clone()]);
        run(&setup, &claimed).unwrap();
        // The unclaimed folder is untouched and still listed; the claimed one is merged.
        assert!(other.join("orphan.md").is_file());
        assert_eq!(unclaimed(setup.data.path(), &folders), vec![other]);
        assert!(!setup.memory.read().contains("Orphan."));
    }

    #[test]
    fn a_body_that_would_change_the_files_structure_is_archived() {
        let setup = setup();
        setup
            .memory
            .remember("kept", "Already here", "project", "Stays.")
            .unwrap();
        let before = setup.memory.read();
        let source = private(&setup);
        write_entry(
            &source.dir,
            "heading",
            "Has a heading line",
            "project",
            "a\n## injected\nb",
        );
        write_entry(
            &source.dir,
            "fence",
            "An open fence",
            "project",
            "```\nnever closed",
        );
        write_entry(
            &source.dir,
            "fine",
            "Balanced",
            "project",
            "```\ncode\n```\nok",
        );

        let done = run(&setup, &source).unwrap();
        assert_eq!((done.added, done.archived), (1, 2));
        let text = setup.memory.read();
        assert!(text.starts_with(&before), "{text}");
        assert!(
            !text.contains("injected") && !text.contains("never closed"),
            "{text}"
        );
        let names: Vec<_> = sections(&text).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["kept", "fine"]);
        let archive = std::fs::read_to_string(setup.memory.archive_file()).unwrap();
        assert!(
            archive.contains("injected") && archive.contains("never closed"),
            "{archive}"
        );
        assert!(
            archive.contains("would change Memory.md's structure"),
            "{archive}"
        );
    }

    #[test]
    fn files_with_crlf_line_ends_are_compared_and_checked_as_lf() {
        let setup = setup();
        setup
            .memory
            .remember("build", "How it builds", "project", "line one\nline two")
            .unwrap();
        let source = private(&setup);
        std::fs::create_dir_all(&source.dir).unwrap();
        let crlf = |name: &str, body: &str| {
            let text = format!(
                "---\r\nname: {name}\r\ndescription: About {name}\r\ntype: project\r\n---\r\n\r\n{body}\r\n"
            );
            std::fs::write(source.dir.join(format!("{name}.md")), text).unwrap();
        };
        crlf("build", "line one\r\nline two");
        crlf("split", "a\r\n## injected\r\nb");
        crlf("code", "```\r\n## fine in a fence\r\n```\r\nok");

        let done = run(&setup, &source).unwrap();
        assert_eq!(
            (done.duplicates, done.added, done.archived, done.kept_both),
            (1, 1, 1, 0),
            "the CRLF copy of a section is its duplicate, the heading line is archived"
        );
        let text = setup.memory.read();
        assert!(
            !text.contains('\r') && !text.contains("injected"),
            "{text:?}"
        );
        assert!(
            text.contains("## code") && text.contains("fine in a fence"),
            "{text}"
        );
        let archive = std::fs::read_to_string(setup.memory.archive_file()).unwrap();
        assert!(
            archive.contains("injected") && archive.contains("would change Memory.md's structure")
        );
    }

    #[test]
    fn a_memory_md_that_cannot_be_read_stops_the_migration_before_anything_is_removed() {
        let setup = setup();
        let source = private(&setup);
        write_entry(
            &source.dir,
            "build",
            "How it builds",
            "project",
            "Run make.",
        );
        std::fs::create_dir_all(setup.memory.file().parent().unwrap()).unwrap();
        std::fs::write(setup.memory.file(), [0xff, 0xfe, 0x00, 0x41]).unwrap();

        let error = migrate(setup.data.path(), "env", &setup.memory, &source);
        assert!(error.is_err());
        assert_eq!(
            std::fs::read(setup.memory.file()).unwrap(),
            [0xff, 0xfe, 0x00, 0x41],
            "the file is not replaced"
        );
        assert!(
            source.dir.join("build.md").is_file(),
            "the original is still there"
        );
        assert!(recorded(setup.data.path()).is_empty());
    }
}
