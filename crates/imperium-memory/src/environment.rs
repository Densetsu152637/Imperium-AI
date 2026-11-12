//! An environment's memory: one Markdown file, `Memory.md`, in the environment's working
//! folder (see docs/17-memory.md#environment-memory). The user edits it freely; agents add and
//! replace `## <name>` sections through `memory_remember` with the `environment` scope.
//!
//! The file is workflow context: sessions of that environment read it into their instructions
//! when they start, so compaction never touches it. It is created on the first write.

use std::path::{Path, PathBuf};

use crate::{KINDS, MemoryError, io, looks_secret};

/// The file's name inside an environment's working folder.
pub const FILE_NAME: &str = "Memory.md";

/// Most characters of `Memory.md` put into a session's instructions; the rest stays on disk
/// and can be read with `memory_read` (name `Memory.md`).
pub const MAX_CONTEXT_CHARS: usize = 24_000;

/// One environment's `Memory.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentMemory {
    file: PathBuf,
}

impl EnvironmentMemory {
    pub fn new(file: PathBuf) -> Self {
        EnvironmentMemory { file }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The file's text; empty when it does not exist yet or cannot be read.
    pub fn read(&self) -> String {
        std::fs::read_to_string(&self.file).unwrap_or_default()
    }

    /// Adds the section `name`, or replaces the one of that name, and returns whether it
    /// replaced one. Text that looks like a secret is refused. The file and its folder are
    /// created when missing; the write goes through a temporary file, so a failure never
    /// leaves half a file.
    pub fn remember(
        &self,
        name: &str,
        description: &str,
        kind: &str,
        body: &str,
    ) -> Result<bool, MemoryError> {
        let name_ok = !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('-')
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !name_ok {
            return Err(MemoryError::BadName(name.to_string()));
        }
        if !KINDS.contains(&kind) {
            return Err(MemoryError::BadKind(kind.to_string()));
        }
        if description.trim().is_empty() {
            return Err(MemoryError::NoDescription);
        }
        if let Some(reason) = looks_secret(&format!("{description}\n{body}")) {
            return Err(MemoryError::LooksSecret(reason));
        }
        let section = format!(
            "## {name}\n_{} ({kind})_\n\n{}\n",
            description.trim().replace('\n', " "),
            body.trim()
        );
        let existing = self.read();
        let (updated, replaced) = upsert_section(&existing, name, &section);
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io(parent, error))?;
        }
        let temporary = self.file.with_extension("md.tmp");
        std::fs::write(&temporary, updated).map_err(|error| io(&temporary, error))?;
        std::fs::rename(&temporary, &self.file).map_err(|error| io(&self.file, error))?;
        Ok(replaced)
    }
}

/// `text` with `section` (starting `## <name>`) replacing the section of that name, or
/// appended; true when one was replaced. Lines inside fenced code blocks never start a section.
fn upsert_section(text: &str, name: &str, section: &str) -> (String, bool) {
    let heading = format!("## {name}");
    let mut lines: Vec<&str> = text.lines().collect();
    let mut fenced = false;
    let mut start = None;
    let mut end = lines.len();
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if fenced || !line.starts_with("## ") {
            continue;
        }
        if start.is_some() {
            end = index;
            break;
        }
        if line.trim_end() == heading {
            start = Some(index);
        }
    }
    match start {
        Some(start) => {
            let tail: Vec<&str> = lines.split_off(end);
            lines.truncate(start);
            let mut out = lines.join("\n");
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(section);
            if !tail.is_empty() {
                out.push('\n');
                out.push_str(&tail.join("\n"));
                out.push('\n');
            }
            (out, true)
        }
        None => {
            let mut out = text.trim_end().to_string();
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(section);
            (out, false)
        }
    }
}

/// The text of `Memory.md` as put into instructions: cut at [`MAX_CONTEXT_CHARS`].
pub fn for_context(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_CONTEXT_CHARS {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(MAX_CONTEXT_CHARS).collect();
    cut.push_str("\n... (cut; read the rest with memory_read, name \"Memory.md\")");
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> (tempfile::TempDir, EnvironmentMemory) {
        let dir = tempfile::tempdir().unwrap();
        let memory = EnvironmentMemory::new(dir.path().join("env").join(FILE_NAME));
        (dir, memory)
    }

    #[test]
    fn the_file_is_created_on_the_first_write_and_read_back() {
        let (_dir, memory) = memory();
        assert_eq!(memory.read(), "");
        assert!(!memory.file().exists());
        let replaced = memory
            .remember("uses-pnpm", "Package manager", "project", "Use pnpm.")
            .unwrap();
        assert!(!replaced);
        let text = memory.read();
        assert!(text.contains("## uses-pnpm"));
        assert!(text.contains("Use pnpm."));
    }

    #[test]
    fn a_section_of_the_same_name_is_replaced_and_the_rest_kept() {
        let (_dir, memory) = memory();
        std::fs::create_dir_all(memory.file().parent().unwrap()).unwrap();
        std::fs::write(memory.file(), "# Notes\n\nhand written\n").unwrap();
        memory.remember("a", "first", "project", "one").unwrap();
        memory.remember("b", "second", "project", "two").unwrap();
        let replaced = memory
            .remember("a", "first again", "project", "uno")
            .unwrap();
        assert!(replaced);
        let text = memory.read();
        assert!(text.starts_with("# Notes\n\nhand written"));
        assert!(text.contains("uno") && !text.contains("\none\n"));
        assert!(text.contains("## b"));
        assert_eq!(text.matches("## a").count(), 1);
    }

    #[test]
    fn secrets_and_bad_names_are_refused_and_nothing_is_written() {
        let (_dir, memory) = memory();
        assert!(matches!(
            memory.remember("Bad Name", "d", "project", "b"),
            Err(MemoryError::BadName(_))
        ));
        assert!(matches!(
            memory.remember(
                "ok",
                "d",
                "project",
                "token sk-abcdefghijklmnopqrstuvwxyz123456"
            ),
            Err(MemoryError::LooksSecret(_))
        ));
        assert!(!memory.file().exists());
    }

    #[test]
    fn long_text_is_cut_for_context() {
        let long = "x".repeat(MAX_CONTEXT_CHARS + 10);
        assert!(for_context(&long).contains("memory_read"));
        assert_eq!(for_context("short"), "short");
    }
}
