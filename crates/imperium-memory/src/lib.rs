//! Agent memory: one fact per Markdown file with front matter, in three scopes (global, the
//! project's shared folder, and the user's private notes about a project), with an index,
//! term-based search, and a redaction check that keeps secrets out.

mod search;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use imperium_extensions::markdown::front_matter;

pub use search::score;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// About the user, across every project: `<data>/memory/global/`.
    Global,
    /// The project's shared memory, which can be committed: `<project>/.imperium/memory/`.
    Project,
    /// The user's own notes about a project: `<data>/memory/projects/<id>/`.
    ProjectPrivate,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
            Scope::ProjectPrivate => "project-private",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "global" => Scope::Global,
            "project" => Scope::Project,
            "project-private" | "project_private" | "private" => Scope::ProjectPrivate,
            _ => return None,
        })
    }
}

/// What an entry records.
pub const KINDS: [&str; 5] = ["user", "feedback", "project", "reference", "lesson"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub description: String,
    /// One of [`KINDS`].
    pub kind: String,
    pub scope: Scope,
    pub created: String,
    pub source: String,
    pub body: String,
    pub path: PathBuf,
}

impl Entry {
    /// The entry as its file holds it.
    fn to_markdown(&self) -> String {
        format!(
            "---\nname: {}\ndescription: {}\ntype: {}\nscope: {}\ncreated: {}\nsource: {}\n---\n\n{}\n",
            self.name,
            self.description.replace('\n', " "),
            self.kind,
            self.scope.as_str(),
            self.created,
            self.source,
            self.body.trim()
        )
    }
}

/// A memory to save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    pub name: String,
    pub description: String,
    pub kind: String,
    pub scope: Scope,
    pub body: String,
    /// Where it came from, such as a session id.
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("names are 1–64 lowercase letters, digits, and hyphens: {0:?}")]
    BadName(String),
    #[error("type must be one of user, feedback, project, reference, lesson: {0:?}")]
    BadKind(String),
    #[error("a description is required")]
    NoDescription,
    #[error("{0} memory is not available in this session (no project)")]
    NoScope(&'static str),
    #[error("refused: the entry looks like it contains a secret ({0})")]
    LooksSecret(&'static str),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

/// The memory folders for one session.
#[derive(Debug, Clone)]
pub struct Store {
    global: PathBuf,
    project: Option<PathBuf>,
    private: Option<PathBuf>,
}

impl Store {
    /// `data_dir` is Imperium's data folder; `project_dir` the session's project, if any.
    pub fn new(data_dir: &Path, project_dir: Option<&Path>) -> Self {
        let memory = data_dir.join("memory");
        Store {
            global: memory.join("global"),
            project: project_dir.map(|dir| dir.join(".imperium").join("memory")),
            private: project_dir.map(|dir| memory.join("projects").join(project_id(dir))),
        }
    }

    pub fn dir(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::Global => Some(&self.global),
            Scope::Project => self.project.as_deref(),
            Scope::ProjectPrivate => self.private.as_deref(),
        }
    }

    /// Every entry, global first, then project, then private, each sorted by name.
    pub fn list(&self) -> Vec<Entry> {
        [Scope::Global, Scope::Project, Scope::ProjectPrivate]
            .into_iter()
            .flat_map(|scope| self.list_scope(scope))
            .collect()
    }

    fn list_scope(&self, scope: Scope) -> Vec<Entry> {
        let Some(dir) = self.dir(scope) else {
            return Vec::new();
        };
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
            .filter_map(|path| read_entry(&path, scope))
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    pub fn get(&self, name: &str) -> Option<Entry> {
        self.list().into_iter().find(|entry| entry.name == name)
    }

    /// Entries matching `query`, best first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Entry> {
        let entries = self.list();
        let mut scored: Vec<(f64, Entry)> = score(&entries, query)
            .into_iter()
            .zip(entries)
            .filter(|(score, _)| *score > 0.0)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry)
            .collect()
    }

    /// Saves an entry, replacing one of the same name in the same scope, and rewrites the
    /// scope's `INDEX.md`.
    pub fn save(&self, new: NewEntry) -> Result<Entry, MemoryError> {
        validate(&new)?;
        let dir = self
            .dir(new.scope)
            .ok_or(MemoryError::NoScope(new.scope.as_str()))?;
        std::fs::create_dir_all(dir).map_err(|error| io(dir, error))?;
        let path = dir.join(format!("{}.md", new.name));
        let created = read_entry(&path, new.scope).map_or_else(today, |old| old.created);
        let entry = Entry {
            name: new.name,
            description: new.description.trim().to_string(),
            kind: new.kind,
            scope: new.scope,
            created,
            source: new.source,
            body: new.body,
            path: path.clone(),
        };
        std::fs::write(&path, entry.to_markdown()).map_err(|error| io(&path, error))?;
        self.write_index(entry.scope)?;
        Ok(entry)
    }

    /// Deletes the named entry in `scope`; false when there was none.
    pub fn forget(&self, name: &str, scope: Scope) -> Result<bool, MemoryError> {
        let Some(dir) = self.dir(scope) else {
            return Ok(false);
        };
        let path = dir.join(format!("{name}.md"));
        match std::fs::remove_file(&path) {
            Ok(()) => {
                self.write_index(scope)?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io(&path, error)),
        }
    }

    /// The index lines agents always see, cut at `max_chars`.
    pub fn index(&self, max_chars: usize) -> String {
        let mut text = String::new();
        for entry in self.list() {
            let line = format!(
                "- {} ({}, {}): {}\n",
                entry.name,
                entry.scope.as_str(),
                entry.kind,
                entry.description
            );
            if text.len() + line.len() > max_chars {
                text.push_str("- ... more entries; use memory_search\n");
                break;
            }
            text.push_str(&line);
        }
        text
    }

    fn write_index(&self, scope: Scope) -> Result<(), MemoryError> {
        let Some(dir) = self.dir(scope) else {
            return Ok(());
        };
        let mut text = format!(
            "# Memory index ({})\n\nGenerated by Imperium; edit the entries, not this file.\n\n",
            scope.as_str()
        );
        for entry in self.list_scope(scope) {
            text.push_str(&format!(
                "- [{}]({}.md): {}\n",
                entry.name, entry.name, entry.description
            ));
        }
        let path = dir.join("INDEX.md");
        std::fs::write(&path, text).map_err(|error| io(&path, error))
    }
}

fn io(path: &Path, error: std::io::Error) -> MemoryError {
    MemoryError::Io(path.display().to_string(), error)
}

fn read_entry(path: &Path, scope: Scope) -> Option<Entry> {
    let text = std::fs::read_to_string(path).ok()?;
    let (fields, body) = front_matter(&text);
    let name = fields.get("name").cloned().or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    })?;
    Some(Entry {
        name,
        description: fields.get("description").cloned().unwrap_or_default(),
        kind: fields
            .get("type")
            .cloned()
            .unwrap_or_else(|| "project".into()),
        scope,
        created: fields.get("created").cloned().unwrap_or_default(),
        source: fields.get("source").cloned().unwrap_or_default(),
        body: body.trim().to_string(),
        path: path.to_path_buf(),
    })
}

fn validate(new: &NewEntry) -> Result<(), MemoryError> {
    let name_ok = !new.name.is_empty()
        && new.name.len() <= 64
        && new
            .name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !new.name.starts_with('-');
    if !name_ok {
        return Err(MemoryError::BadName(new.name.clone()));
    }
    if !KINDS.contains(&new.kind.as_str()) {
        return Err(MemoryError::BadKind(new.kind.clone()));
    }
    if new.description.trim().is_empty() {
        return Err(MemoryError::NoDescription);
    }
    if let Some(reason) = looks_secret(&format!("{}\n{}", new.description, new.body)) {
        return Err(MemoryError::LooksSecret(reason));
    }
    Ok(())
}

/// A reason the text looks like it holds a credential, if it does.
pub fn looks_secret(text: &str) -> Option<&'static str> {
    const PREFIXES: [(&str, &str); 8] = [
        ("-----BEGIN", "a private key"),
        ("sk-", "an API key"),
        ("ghp_", "a GitHub token"),
        ("github_pat_", "a GitHub token"),
        ("xox", "a Slack token"),
        ("AKIA", "an AWS key"),
        ("AIza", "a Google API key"),
        ("hf_", "a Hugging Face token"),
    ];
    for word in text.split(|c: char| c.is_whitespace() || "\"'`=:,;()[]{}<>".contains(c)) {
        for (prefix, reason) in PREFIXES {
            if word.starts_with(prefix) && word.len() >= prefix.len() + 16 {
                return Some(reason);
            }
        }
        if word.starts_with("-----BEGIN") {
            return Some("a private key");
        }
        // Long unbroken runs of key-like characters with both letters and digits.
        let keyish = word.len() >= 32
            && word.bytes().all(|b| {
                b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'+' || b == b'/'
            })
            && word.bytes().any(|b| b.is_ascii_digit())
            && word.bytes().any(|b| b.is_ascii_alphabetic())
            && !word.contains('/');
        if keyish {
            return Some("a long token-like string");
        }
    }
    None
}

/// A stable folder name for a project: FNV-1a of its canonical path, in hex.
pub fn project_id(dir: &Path) -> String {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.to_string_lossy().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    format!("{slug}-{hash:016x}")
}

/// Today's date (UTC) as `YYYY-MM-DD`.
fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, scope: Scope, description: &str, body: &str) -> NewEntry {
        NewEntry {
            name: name.into(),
            description: description.into(),
            kind: "feedback".into(),
            scope,
            body: body.into(),
            source: "test".into(),
        }
    }

    #[test]
    fn entries_save_list_search_and_forget_across_scopes() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), Some(project.path()));
        store
            .save(entry(
                "prefers-pnpm",
                Scope::Global,
                "Uses pnpm, never npm",
                "Run pnpm install.",
            ))
            .unwrap();
        store
            .save(entry(
                "ci-on-windows",
                Scope::Project,
                "CI must pass on Windows",
                "Test with cmd quoting.",
            ))
            .unwrap();
        let saved = store
            .save(entry(
                "my-notes",
                Scope::ProjectPrivate,
                "Personal notes",
                "Ask about deploys.",
            ))
            .unwrap();
        assert!(saved.path.starts_with(data.path()));
        assert!(
            project
                .path()
                .join(".imperium/memory/ci-on-windows.md")
                .is_file()
        );
        let index =
            std::fs::read_to_string(project.path().join(".imperium/memory/INDEX.md")).unwrap();
        assert!(
            index.contains("[ci-on-windows](ci-on-windows.md)"),
            "{index}"
        );

        let names: Vec<_> = store.list().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["prefers-pnpm", "ci-on-windows", "my-notes"]);
        assert_eq!(
            store.search("which package manager, npm or pnpm?", 5)[0].name,
            "prefers-pnpm"
        );
        assert_eq!(store.search("windows quoting", 5)[0].name, "ci-on-windows");
        assert!(store.search("kubernetes", 5).is_empty());
        assert!(
            store
                .index(1000)
                .contains("- prefers-pnpm (global, feedback): Uses pnpm, never npm")
        );

        // Saving again replaces the entry and keeps its creation date.
        let created = store.get("prefers-pnpm").unwrap().created;
        store
            .save(entry(
                "prefers-pnpm",
                Scope::Global,
                "Uses pnpm",
                "Updated.",
            ))
            .unwrap();
        let updated = store.get("prefers-pnpm").unwrap();
        assert_eq!(
            (updated.body.as_str(), updated.created),
            ("Updated.", created)
        );

        assert!(store.forget("my-notes", Scope::ProjectPrivate).unwrap());
        assert!(!store.forget("my-notes", Scope::ProjectPrivate).unwrap());
        assert_eq!(store.list().len(), 2);
    }

    #[test]
    fn invalid_or_secret_entries_are_refused() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path(), None);
        let bad_name = store.save(entry("Has Spaces", Scope::Global, "d", "b"));
        assert!(matches!(bad_name, Err(MemoryError::BadName(_))));
        let no_project = store.save(entry("x", Scope::Project, "d", "b"));
        assert!(matches!(no_project, Err(MemoryError::NoScope(_))));
        let key = store.save(entry(
            "key",
            Scope::Global,
            "the key",
            "token is sk-abcdefghijklmnopqrstuvwx",
        ));
        assert!(matches!(key, Err(MemoryError::LooksSecret(_))));
        assert_eq!(
            looks_secret("see docs/12-roadmap.md and https://example.com/a/b"),
            None
        );
        assert!(looks_secret("ghp_0123456789abcdefghijABCDEFGHIJ").is_some());
    }

    #[test]
    fn dates_and_project_ids_are_well_formed() {
        let date = today();
        assert_eq!(date.len(), 10);
        assert!(date.starts_with("20"));
        let id = project_id(Path::new("/some/My Project"));
        assert!(id.starts_with("my-project-"), "{id}");
    }
}
