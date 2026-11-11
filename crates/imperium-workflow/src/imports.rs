//! Instruction files (`AGENTS.md`, `CLAUDE.md`) and the `@path` imports inside them, in the way
//! Claude Code reads them: an import is `@path` outside code, relative to the importing file,
//! `@~/` for the home folder, with `\ ` for a space in the path. Imports nest to
//! [`MAX_IMPORT_DEPTH`] levels, a file is included once however often it is imported, and an
//! import that loops back, goes too deep, or leaves the allowed locations is reported instead of
//! followed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The instruction files looked for in a directory, in the order they are served.
pub const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
/// Imports nest this deep at most (the file itself is level 0).
pub const MAX_IMPORT_DEPTH: usize = 5;
/// The longest one file is served; the rest is cut with a note.
pub const MAX_FILE_BYTES: usize = 16 * 1024;
/// The longest a file and everything it imports is served.
pub const MAX_EXPANDED_BYTES: usize = 64 * 1024;

/// Where imports may be read from.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Folders imports may point into.
    pub allowed: Vec<PathBuf>,
    /// Whether imports outside `allowed` are read too (a workflow pack the user configured).
    pub trusted: bool,
    /// What `@~/` means.
    pub home: Option<PathBuf>,
    /// Files already served elsewhere: imports of them are left out without a note.
    pub skip: Vec<PathBuf>,
}

/// A file with its imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    /// The file's text, then each imported file in a labelled block, in the order imported.
    pub text: String,
    /// Every file in `text`, the first being the file itself.
    pub files: Vec<PathBuf>,
    /// Imports that were not followed, and why.
    pub skipped: Vec<String>,
    /// Whether any file was cut to fit its limit.
    pub truncated: bool,
}

/// The home folder, from the environment.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// A path as people write it: no Windows verbatim prefix, forward slashes.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.replace('\\', "/")
}

/// The instruction files in `dir` and each parent up to and including `root`, the outermost
/// directory first and `AGENTS.md` before `CLAUDE.md`, so the nearest file comes last. Empty
/// when `dir` is not inside `root`.
pub fn instruction_files(dir: &Path, root: &Path) -> Vec<PathBuf> {
    if !dir.starts_with(root) {
        return Vec::new();
    }
    let mut chain: Vec<&Path> = Vec::new();
    for ancestor in dir.ancestors() {
        chain.push(ancestor);
        if ancestor == root {
            break;
        }
    }
    chain
        .into_iter()
        .rev()
        .flat_map(|directory| {
            INSTRUCTION_FILES
                .iter()
                .map(|name| directory.join(name))
                .filter(|file| file.is_file())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `text` cut to at most `max` bytes at a line or character boundary, with a note saying how
/// much was left out. Returns whether it was cut.
pub fn cap(text: &str, max: usize) -> (String, bool) {
    cap_of(text, max, text.len())
}

/// [`cap`] for text read from a file of `total` bytes, of which only a prefix was read.
fn cap_of(text: &str, max: usize, total: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(line) = text[..end].rfind('\n') {
        end = line;
    }
    (
        format!(
            "{}\n[truncated: {} of {} bytes shown]",
            &text[..end],
            end,
            total
        ),
        true,
    )
}

/// At most [`MAX_FILE_BYTES`] + 1 bytes of `file` (so a longer file is known to be cut without
/// reading it all), and the file's size.
fn read_bounded(file: &Path) -> std::io::Result<(String, usize)> {
    use std::io::Read;
    let handle = std::fs::File::open(file)?;
    let total = usize::try_from(handle.metadata()?.len()).unwrap_or(usize::MAX);
    let mut bytes = Vec::new();
    handle
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), total))
}

/// Reads `file` and the files it imports under `policy`.
pub fn expand(file: &Path, policy: &Policy) -> std::io::Result<Expanded> {
    let canonical = std::fs::canonicalize(file)?;
    // A symbolic link must not lead the file out of the allowed folders.
    if !policy.trusted
        && !policy
            .allowed
            .iter()
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .any(|root| canonical.starts_with(root))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is outside the allowed locations",
                display_path(&canonical)
            ),
        ));
    }
    let (text, total) = read_bounded(&canonical)?;
    let mut state = State {
        policy,
        allowed: policy
            .allowed
            .iter()
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .collect(),
        skip: policy
            .skip
            .iter()
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .collect(),
        home: policy.home.clone().or_else(home_dir),
        stack: vec![canonical.clone()],
        seen: HashSet::from([canonical.clone()]),
        out: Expanded {
            text: String::new(),
            files: vec![canonical.clone()],
            skipped: Vec::new(),
            truncated: false,
        },
        imported_bytes: 0,
    };
    let (body, cut) = cap_of(&text, MAX_FILE_BYTES, total);
    state.out.truncated |= cut;
    state.out.text = body;
    let mut blocks = String::new();
    state.visit(&canonical, &text, 0, &mut blocks);
    state.out.text.push_str(&blocks);
    Ok(state.out)
}

struct State<'a> {
    policy: &'a Policy,
    allowed: Vec<PathBuf>,
    skip: Vec<PathBuf>,
    home: Option<PathBuf>,
    /// The chain of files being read now, for cycle detection.
    stack: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
    out: Expanded,
    imported_bytes: usize,
}

impl State<'_> {
    /// Appends the files `text` (the content of `file`, at `depth`) imports to `blocks`.
    fn visit(&mut self, file: &Path, text: &str, depth: usize, blocks: &mut String) {
        let base = file.parent().unwrap_or(Path::new("."));
        for token in import_tokens(text) {
            let Some(target) = self.locate(&token, base) else {
                continue;
            };
            let Ok(canonical) = std::fs::canonicalize(&target) else {
                continue;
            };
            if !canonical.is_file() {
                continue;
            }
            let shown = display_path(&canonical);
            if self.stack.contains(&canonical) {
                self.skip_note(blocks, &shown, "it imports itself (cycle)");
                continue;
            }
            if self.seen.contains(&canonical) || self.skip.contains(&canonical) {
                continue;
            }
            if depth + 1 > MAX_IMPORT_DEPTH {
                self.skip_note(
                    blocks,
                    &shown,
                    &format!("imports nest at most {MAX_IMPORT_DEPTH} levels"),
                );
                continue;
            }
            if !self.policy.trusted && !self.allowed.iter().any(|root| canonical.starts_with(root))
            {
                self.skip_note(blocks, &shown, "it is outside the allowed locations");
                continue;
            }
            let Ok((imported, total)) = read_bounded(&canonical) else {
                self.skip_note(blocks, &shown, "it could not be read");
                continue;
            };
            if self.imported_bytes + imported.len().min(MAX_FILE_BYTES) > MAX_EXPANDED_BYTES {
                self.skip_note(blocks, &shown, "the imports are too large");
                continue;
            }
            let (body, cut) = cap_of(&imported, MAX_FILE_BYTES, total);
            self.imported_bytes += body.len();
            self.out.truncated |= cut;
            self.seen.insert(canonical.clone());
            self.out.files.push(canonical.clone());
            blocks.push_str(&format!(
                "\n\n<imported-file path=\"{shown}\">\n{}\n</imported-file>",
                body.replace("</imported-file>", "<\\/imported-file>")
            ));
            self.stack.push(canonical.clone());
            self.visit(&canonical, &imported, depth + 1, blocks);
            self.stack.pop();
        }
    }

    fn skip_note(&mut self, blocks: &mut String, path: &str, reason: &str) {
        let note = format!("not imported: {path} ({reason})");
        blocks.push_str(&format!("\n\n[{note}]"));
        self.out.skipped.push(note);
    }

    /// The file an import token names.
    fn locate(&self, token: &str, base: &Path) -> Option<PathBuf> {
        if let Some(rest) = token
            .strip_prefix("~/")
            .or_else(|| token.strip_prefix("~\\"))
        {
            return Some(self.home.as_ref()?.join(rest));
        }
        let path = Path::new(token);
        Some(if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        })
    }
}

/// The paths `text` imports, in order and without repeats: `@path` words outside code. `\ `
/// stands for a space in the path, and punctuation that ends a sentence is not part of it.
pub fn import_tokens(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut fenced: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let fence = ["```", "~~~"]
            .into_iter()
            .find(|fence| trimmed.starts_with(fence));
        match (fenced, fence) {
            (None, Some(fence)) => {
                fenced = Some(fence);
                continue;
            }
            (Some(open), Some(fence)) if open == fence => {
                fenced = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, None) => {}
        }
        let chars: Vec<char> = line.chars().collect();
        let mut index = 0;
        let mut in_code = false;
        while index < chars.len() {
            let c = chars[index];
            if c == '`' {
                in_code = !in_code;
            } else if c == '@'
                && !in_code
                && (index == 0 || chars[index - 1].is_whitespace() || chars[index - 1] == '(')
            {
                let mut path = String::new();
                let mut end = index + 1;
                while end < chars.len() {
                    let c = chars[end];
                    if c == '\\' && chars.get(end + 1) == Some(&' ') {
                        path.push(' ');
                        end += 2;
                    } else if c.is_whitespace() || c == '`' {
                        break;
                    } else {
                        path.push(c);
                        end += 1;
                    }
                }
                let path =
                    path.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
                if !path.is_empty() && !found.iter().any(|known| known == path) {
                    found.push(path.to_string());
                }
                index = end;
                continue;
            }
            index += 1;
        }
    }
    found
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn policy(root: &Path) -> Policy {
        Policy {
            allowed: vec![root.to_path_buf()],
            ..Policy::default()
        }
    }

    #[test]
    fn tokens_skip_code_emails_and_repeats() {
        let text = "See @docs/a.md and @docs/a.md, plus (@b.md).\n\
                    Mail me at me@example.com or write `@code.md`.\n\
                    ```\n@fenced.md\n```\n\
                    Spaced: @My\\ Notes/x.md.\n";
        assert_eq!(import_tokens(text), ["docs/a.md", "b.md", "My Notes/x.md"]);
    }

    #[test]
    fn imports_resolve_relative_to_the_importing_file_and_nest() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "AGENTS.md", "root\n@guides/one.md\n");
        write(dir.path(), "guides/one.md", "one\n@two.md\n");
        write(dir.path(), "guides/two.md", "two\n");
        let expanded = expand(&dir.path().join("AGENTS.md"), &policy(dir.path())).unwrap();
        assert_eq!(expanded.files.len(), 3);
        let one = expanded.text.find("\none\n").unwrap();
        let two = expanded.text.find("\ntwo\n").unwrap();
        assert!(one < two, "imports follow in the order they are reached");
        assert!(expanded.skipped.is_empty());
    }

    #[test]
    fn a_cycle_is_reported_once_and_each_file_is_included_once() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "AGENTS.md", "a\n@b.md\n");
        write(dir.path(), "b.md", "b\n@AGENTS.md\n@c.md\n");
        write(dir.path(), "c.md", "c\n@b.md\n");
        let expanded = expand(&dir.path().join("AGENTS.md"), &policy(dir.path())).unwrap();
        assert_eq!(expanded.files.len(), 3);
        assert_eq!(expanded.skipped.len(), 2, "{:?}", expanded.skipped);
        assert!(expanded.skipped.iter().all(|note| note.contains("cycle")));
        assert_eq!(expanded.text.matches("<imported-file").count(), 2);
    }

    #[test]
    fn a_chain_deeper_than_the_cap_stops_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "AGENTS.md", "level 0\n@l1.md\n");
        for level in 1..=7 {
            write(
                dir.path(),
                &format!("l{level}.md"),
                &format!("level {level}\n@l{}.md\n", level + 1),
            );
        }
        let expanded = expand(&dir.path().join("AGENTS.md"), &policy(dir.path())).unwrap();
        assert_eq!(expanded.files.len(), MAX_IMPORT_DEPTH + 1);
        assert!(expanded.text.contains("level 5"));
        assert!(!expanded.text.contains("level 6\n"));
        assert_eq!(expanded.skipped.len(), 1);
        assert!(expanded.skipped[0].contains("l6.md"));
    }

    #[test]
    fn outside_imports_need_a_trusted_source() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = write(outside.path(), "Shared Rules/CLAUDE.md", "shared\n");
        let escaped = display_path(&std::fs::canonicalize(&target).unwrap()).replace(' ', "\\ ");
        write(dir.path(), "AGENTS.md", &format!("@{escaped}\n"));
        let untrusted = expand(&dir.path().join("AGENTS.md"), &policy(dir.path())).unwrap();
        assert!(!untrusted.text.contains("shared"));
        assert!(untrusted.skipped[0].contains("outside"));
        let trusted = expand(
            &dir.path().join("AGENTS.md"),
            &Policy {
                trusted: true,
                ..policy(dir.path())
            },
        )
        .unwrap();
        assert!(trusted.text.contains("shared"));
    }

    #[test]
    fn home_imports_resolve_against_the_home_folder() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(home.path(), "rules.md", "from home\n");
        write(dir.path(), "AGENTS.md", "@~/rules.md\n");
        let expanded = expand(
            &dir.path().join("AGENTS.md"),
            &Policy {
                trusted: true,
                home: Some(home.path().to_path_buf()),
                ..policy(dir.path())
            },
        )
        .unwrap();
        assert!(expanded.text.contains("from home"));
    }

    #[test]
    fn skipped_files_are_left_out_silently() {
        let dir = tempfile::tempdir().unwrap();
        let agents = write(dir.path(), "AGENTS.md", "rules\n");
        write(dir.path(), "CLAUDE.md", "@AGENTS.md\n");
        let expanded = expand(
            &dir.path().join("CLAUDE.md"),
            &Policy {
                skip: vec![agents],
                ..policy(dir.path())
            },
        )
        .unwrap();
        assert_eq!(expanded.files.len(), 1);
        assert!(expanded.skipped.is_empty());
    }

    #[test]
    fn instruction_files_run_from_the_folder_root_down() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "AGENTS.md", "r");
        write(root, "a/CLAUDE.md", "a");
        write(root, "a/b/AGENTS.md", "b1");
        write(root, "a/b/CLAUDE.md", "b2");
        let found = instruction_files(&root.join("a/b"), root);
        let names: Vec<String> = found
            .iter()
            .map(|file| display_path(file.strip_prefix(root).unwrap()))
            .collect();
        assert_eq!(
            names,
            ["AGENTS.md", "a/CLAUDE.md", "a/b/AGENTS.md", "a/b/CLAUDE.md"]
        );
        assert!(instruction_files(&root.join("a"), &root.join("a/b")).is_empty());
    }

    /// Makes `link` a symbolic link to `target`; false when the platform does not permit it.
    pub(crate) fn try_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(target, link);
        made.is_ok()
    }

    #[test]
    fn a_symlinked_file_leading_outside_is_not_read_unless_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = write(outside.path(), "secret.md", "secret\n");
        if !try_symlink(&secret, &dir.path().join("AGENTS.md")) {
            return;
        }
        let error = expand(&dir.path().join("AGENTS.md"), &policy(dir.path())).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        let trusted = Policy {
            trusted: true,
            ..policy(dir.path())
        };
        assert!(
            expand(&dir.path().join("AGENTS.md"), &trusted)
                .unwrap()
                .text
                .contains("secret")
        );
    }

    #[test]
    fn long_files_are_read_only_up_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let big = format!("{}\n", "x".repeat(100)).repeat(1_000);
        let file = write(dir.path(), "AGENTS.md", &big);
        let expanded = expand(&file, &policy(dir.path())).unwrap();
        assert!(expanded.truncated);
        assert!(expanded.text.len() < MAX_FILE_BYTES + 100);
        assert!(expanded.text.contains(&format!("of {} bytes", big.len())));
    }

    #[test]
    fn cap_cuts_at_a_line_with_a_note() {
        let text = "line one\nline two\nline three\n";
        let (cut, truncated) = cap(text, 12);
        assert!(truncated);
        assert!(cut.starts_with("line one\n[truncated:"));
        assert_eq!(cap(text, 100), (text.to_string(), false));
    }
}
