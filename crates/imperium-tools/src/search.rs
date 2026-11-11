//! `glob` and `grep`: find files by name and search their contents, read-only. The folder to
//! search is checked like `list_dir`'s path; the walk stays inside it (symbolic links are
//! followed only to files that are themselves inside the workspace), skips `.git`, honours
//! `.gitignore` and `.ignore` files unless `include_ignored` is set, and leaves out files a deny
//! rule covers. Results are sorted by path and paged with `offset`.

use std::path::PathBuf;

use ignore::WalkBuilder;
use regex::RegexBuilder;

use crate::permissions::{RuleAction, glob};
use crate::{ToolError, Workspace};

/// Default and largest number of results one call returns.
pub(crate) const DEFAULT_RESULTS: usize = 100;
pub(crate) const MAX_RESULTS: usize = 1000;
/// Entries one search may visit before it stops, so a huge tree cannot stall the agent.
const MAX_VISITED: usize = 200_000;
/// Files larger than this are not searched by `grep`.
const MAX_GREP_FILE: u64 = 10 * 1024 * 1024;
/// Longest line `grep` shows, in characters.
const MAX_LINE_CHARS: usize = 300;
/// Most context lines around each match.
pub(crate) const MAX_CONTEXT: usize = 10;
/// Largest text returned from one call, in characters.
const MAX_OUTPUT_CHARS: usize = 100_000;

/// What a search looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub path: String,
    pub include_ignored: bool,
    pub offset: usize,
    pub max_results: usize,
}

/// A walked file.
struct Found {
    full: PathBuf,
}

/// Files under the scope's folder, sorted by path, minus those deny rules cover for `tool`.
/// The flag says whether the walk stopped early at [`MAX_VISITED`].
fn walk(
    workspace: &Workspace,
    tool: &str,
    scope: &Scope,
    mut keep: impl FnMut(&str) -> bool,
) -> Result<(Vec<Found>, bool), ToolError> {
    if workspace.runtime().is_some() {
        return Err(ToolError::Invalid(format!(
            "{tool} is not available in remote runtimes yet; use run_command with find or grep"
        )));
    }
    let base = workspace.resolve(&scope.path)?;
    if !base.exists() {
        return Err(ToolError::Io(format!("{}: no such folder", scope.path)));
    }
    let respect = !scope.include_ignored;
    let mut builder = WalkBuilder::new(&base);
    builder
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_ignore(respect)
        .git_global(respect)
        .git_exclude(respect)
        .ignore(respect)
        .parents(respect)
        .filter_entry(|entry| entry.file_name() != ".git")
        .sort_by_file_name(|a, b| a.cmp(b));
    let permissions = workspace.permissions();
    let check_rules = permissions.has_deny_rules();
    let mut found = Vec::new();
    let mut visited = 0;
    let mut stopped = false;
    for entry in builder.build() {
        visited += 1;
        if visited > MAX_VISITED {
            stopped = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        let mut full = entry.path().to_path_buf();
        if entry.path_is_symlink() {
            // Only a link to a file inside the workspace counts, as for read_file.
            match workspace.resolve(&full.to_string_lossy()) {
                Ok(target) if target.is_file() => full = target,
                _ => continue,
            }
        } else if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(&base)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if !keep(&relative) {
            continue;
        }
        if check_rules {
            let target = workspace.rule_target_resolved(&full);
            let denied = |name: &str| permissions.evaluate(name, &target) == Some(RuleAction::Deny);
            if denied(tool) || denied("read_file") {
                continue;
            }
        }
        found.push(Found { full });
    }
    Ok((found, stopped))
}

/// `pattern` with `{a,b}` alternatives expanded into separate patterns.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close) = pattern[open..].find('}').map(|at| open + at) else {
        return vec![pattern.to_string()];
    };
    let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
    pattern[open + 1..close]
        .split(',')
        .flat_map(|choice| expand_braces(&format!("{head}{choice}{tail}")))
        .collect()
}

/// A file-name pattern: without `/` it matches the file name at any depth (`*.rs`); with one it
/// matches the path relative to the searched folder (`src/**/*.rs`). `{a,b}` alternatives are
/// expanded; matching ignores case on Windows.
#[derive(Debug, Clone)]
struct NamePattern(Vec<String>);

impl NamePattern {
    fn new(pattern: &str) -> Result<Self, ToolError> {
        let pattern = pattern.trim().replace('\\', "/");
        let pattern = pattern.trim_start_matches("./");
        if pattern.is_empty() {
            return Err(ToolError::Invalid("the pattern is empty".into()));
        }
        Ok(NamePattern(expand_braces(pattern)))
    }

    fn matches(&self, relative: &str) -> bool {
        let fold = cfg!(windows);
        let name = relative.rsplit('/').next().unwrap_or(relative);
        self.0.iter().any(|pattern| {
            if pattern.contains('/') {
                glob(pattern, relative, fold)
            } else {
                glob(pattern, name, fold)
            }
        })
    }
}

fn page_note(shown: usize, total: usize, offset: usize, more: bool, noun: &str) -> String {
    if more {
        format!(
            "[showing {noun} {}-{}; more: call again with offset={}]",
            offset + 1,
            offset + shown,
            offset + shown
        )
    } else if offset > 0 && shown == 0 {
        format!("[no {noun} past offset {offset} ({total} in all)]")
    } else {
        String::new()
    }
}

/// Files whose names match `pattern` under the scope's folder.
pub fn glob_files(
    workspace: &Workspace,
    pattern: &str,
    scope: &Scope,
) -> Result<String, ToolError> {
    let names = NamePattern::new(pattern)?;
    let (found, stopped) = walk(workspace, "glob", scope, |relative| names.matches(relative))?;
    let total = found.len();
    let page: Vec<String> = found
        .iter()
        .skip(scope.offset)
        .take(scope.max_results)
        .map(|file| workspace.display(&file.full))
        .collect();
    let more = scope.offset + page.len() < total;
    let mut out = if page.is_empty() && scope.offset == 0 {
        format!("no files match {pattern}")
    } else {
        page.join("\n")
    };
    let note = page_note(page.len(), total, scope.offset, more, "files");
    if !note.is_empty() {
        out.push('\n');
        out.push_str(&note);
    }
    if stopped {
        out.push_str(&format!(
            "\n[stopped after {MAX_VISITED} entries: search a smaller folder]"
        ));
    }
    Ok(out)
}

/// What `grep` looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepQuery {
    pub pattern: String,
    pub glob: Option<String>,
    pub case_insensitive: bool,
    pub context: usize,
}

/// Lines matching a regular expression in the files under the scope's folder, as
/// `path:line:text`, with `path-line-text` context lines and `--` between separate groups.
pub fn grep_files(
    workspace: &Workspace,
    query: &GrepQuery,
    scope: &Scope,
) -> Result<String, ToolError> {
    let regex = RegexBuilder::new(&query.pattern)
        .case_insensitive(query.case_insensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|error| ToolError::Invalid(format!("bad pattern: {error}")))?;
    let names = query.glob.as_deref().map(NamePattern::new).transpose()?;
    let (found, stopped) = walk(workspace, "grep", scope, |relative| {
        names.as_ref().is_none_or(|names| names.matches(relative))
    })?;
    let context = query.context.min(MAX_CONTEXT);
    let mut seen = 0usize;
    let mut shown = 0usize;
    let mut more = false;
    let mut out = String::new();
    let mut truncated = false;
    'files: for file in &found {
        let Ok(metadata) = std::fs::metadata(&file.full) else {
            continue;
        };
        if metadata.len() > MAX_GREP_FILE {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file.full) else {
            continue;
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue; // binary
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let shown_path = workspace.display(&file.full);
        // The matching lines of this file that fall in the page.
        let mut hits = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            seen += 1;
            if seen <= scope.offset {
                continue;
            }
            if shown == scope.max_results {
                more = true;
                break;
            }
            shown += 1;
            hits.push(index);
        }
        if !hits.is_empty() {
            let mut last_printed: Option<usize> = None;
            for &hit in &hits {
                let from = hit.saturating_sub(context);
                let to = (hit + context).min(lines.len() - 1);
                let from = match last_printed {
                    Some(last) if from <= last + 1 => last + 1,
                    Some(_) if context > 0 => {
                        out.push_str("--\n");
                        from
                    }
                    _ => from,
                };
                for (index, line) in lines.iter().enumerate().take(to + 1).skip(from) {
                    let separator = if hits.contains(&index) { ':' } else { '-' };
                    let line = if line.chars().count() > MAX_LINE_CHARS {
                        let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
                        format!("{cut}…")
                    } else {
                        line.to_string()
                    };
                    out.push_str(&format!(
                        "{shown_path}{separator}{}{separator}{line}\n",
                        index + 1
                    ));
                    last_printed = Some(index);
                }
                if out.len() > MAX_OUTPUT_CHARS {
                    truncated = true;
                    break 'files;
                }
            }
            if context > 0 {
                out.push_str("--\n");
            }
        }
        if more {
            break;
        }
    }
    if out.is_empty() && scope.offset == 0 {
        out = format!("no matches for {}", query.pattern);
    }
    let note = page_note(shown, seen, scope.offset, more, "matches");
    if !note.is_empty() {
        out.push_str(&note);
        out.push('\n');
    }
    if truncated {
        out.push_str(&format!(
            "[output cut at {MAX_OUTPUT_CHARS} characters: narrow the search, lower context, or page with offset={}]\n",
            scope.offset + shown
        ));
    }
    if stopped {
        out.push_str(&format!(
            "[stopped after {MAX_VISITED} entries: search a smaller folder]\n"
        ));
    }
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{PermissionRule, Permissions, SharedRules};

    fn scope(path: &str) -> Scope {
        Scope {
            path: path.into(),
            include_ignored: false,
            offset: 0,
            max_results: DEFAULT_RESULTS,
        }
    }

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        std::fs::write(root.join("src/nested/deep.rs"), "// Alpha here\n").unwrap();
        std::fs::write(root.join("src/notes.md"), "alpha notes\n").unwrap();
        std::fs::write(root.join("target/debug/out.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(root.join("build.log"), "alpha\n").unwrap();
        std::fs::write(root.join(".env"), "ALPHA_KEY=1\n").unwrap();
        dir
    }

    #[test]
    fn glob_matches_names_at_any_depth_and_honours_gitignore() {
        let dir = tree();
        let workspace = Workspace::new(dir.path()).unwrap();
        let found = glob_files(&workspace, "*.rs", &scope(".")).unwrap();
        assert_eq!(found, "src/lib.rs\nsrc/nested/deep.rs");
        let found = glob_files(&workspace, "src/*.{rs,md}", &scope(".")).unwrap();
        assert_eq!(found, "src/lib.rs\nsrc/notes.md");
        let all = Scope {
            include_ignored: true,
            ..scope(".")
        };
        let found = glob_files(&workspace, "*.rs", &all).unwrap();
        assert!(found.contains("target/debug/out.rs"), "{found}");
        let found = glob_files(&workspace, "*.log", &scope(".")).unwrap();
        assert_eq!(found, "no files match *.log");
        let found = glob_files(&workspace, "*.rs", &scope("src/nested")).unwrap();
        assert_eq!(found, "src/nested/deep.rs");
    }

    #[test]
    fn glob_pages_with_offset() {
        let dir = tree();
        let workspace = Workspace::new(dir.path()).unwrap();
        let first = Scope {
            max_results: 1,
            ..scope(".")
        };
        let found = glob_files(&workspace, "*.rs", &first).unwrap();
        assert!(found.starts_with("src/lib.rs\n"), "{found}");
        assert!(found.contains("offset=1"), "{found}");
        let second = Scope { offset: 1, ..first };
        assert_eq!(
            glob_files(&workspace, "*.rs", &second).unwrap(),
            "src/nested/deep.rs"
        );
    }

    #[test]
    fn grep_finds_lines_with_context_and_options() {
        let dir = tree();
        let workspace = Workspace::new(dir.path()).unwrap();
        let query = |pattern: &str| GrepQuery {
            pattern: pattern.into(),
            glob: None,
            case_insensitive: false,
            context: 0,
        };
        let found = grep_files(&workspace, &query("alpha"), &scope(".")).unwrap();
        assert!(found.contains("src/lib.rs:1:fn alpha() {}"), "{found}");
        assert!(found.contains("src/notes.md:1:alpha notes"), "{found}");
        assert!(!found.contains("deep.rs"), "case matters: {found}");
        assert!(!found.contains("target/"), "ignored: {found}");
        assert!(!found.contains("build.log"), "ignored: {found}");

        let insensitive = GrepQuery {
            case_insensitive: true,
            glob: Some("*.rs".into()),
            ..query("alpha")
        };
        let found = grep_files(&workspace, &insensitive, &scope(".")).unwrap();
        assert!(
            found.contains("src/nested/deep.rs:1:// Alpha here"),
            "{found}"
        );
        assert!(!found.contains("notes.md"), "{found}");

        let around = GrepQuery {
            context: 1,
            ..query("beta")
        };
        let found = grep_files(&workspace, &around, &scope("src")).unwrap();
        assert!(found.contains("src/lib.rs-1-fn alpha() {}"), "{found}");
        assert!(found.contains("src/lib.rs:2:fn beta() {}"), "{found}");

        assert!(matches!(
            grep_files(&workspace, &query("("), &scope(".")),
            Err(ToolError::Invalid(_))
        ));
        let paged = Scope {
            max_results: 1,
            ..scope(".")
        };
        let found = grep_files(&workspace, &query("alpha"), &paged).unwrap();
        assert!(found.contains("offset=1"), "{found}");
    }

    #[test]
    fn searches_stay_inside_the_workspace_and_skip_denied_files() {
        let dir = tree();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.rs"), "alpha\n").unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let elsewhere = Scope {
            path: outside.path().to_string_lossy().to_string(),
            ..scope(".")
        };
        assert!(matches!(
            glob_files(&workspace, "*.rs", &elsewhere),
            Err(ToolError::OutsideWorkspace(_))
        ));
        assert!(matches!(
            glob_files(&workspace, "*", &scope("..")),
            Err(ToolError::OutsideWorkspace(_))
        ));

        let query = GrepQuery {
            pattern: "ALPHA".into(),
            glob: None,
            case_insensitive: false,
            context: 0,
        };
        assert!(
            grep_files(&workspace, &query, &scope("."))
                .unwrap()
                .contains(".env:1:")
        );
        let denied = Workspace::new(dir.path())
            .unwrap()
            .with_permissions(Permissions::new(
                SharedRules::new(vec![PermissionRule {
                    tool: "*".into(),
                    pattern: ".env".into(),
                    action: RuleAction::Deny,
                }]),
                SharedRules::default(),
            ));
        let found = grep_files(&denied, &query, &scope(".")).unwrap();
        assert!(!found.contains(".env"), "{found}");
        assert!(
            !glob_files(&denied, ".*", &scope("."))
                .unwrap()
                .contains(".env")
        );
    }

    #[test]
    fn braces_expand_into_alternatives() {
        assert_eq!(expand_braces("*.{ts,tsx}"), ["*.ts", "*.tsx"]);
        assert_eq!(expand_braces("{a,b}/{c,d}").len(), 4);
        assert_eq!(expand_braces("plain"), ["plain"]);
    }
}
