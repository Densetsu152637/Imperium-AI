//! The file picker behind `@` mentions: a fuzzy search over the session's folders (and the
//! project worktrees open beside them) that returns files and directories as the user may name
//! them. It walks like `glob` does: `.git` is skipped, `.gitignore` and `.ignore` files are
//! honoured, links leave the workspace for nowhere, and entries a permission rule asks about or
//! denies are left out. In a remote runtime the listing comes from `git ls-files` there (or
//! `find` outside a repository), and each path is checked against the rules afterwards.

use std::collections::HashSet;
use std::time::Duration;

use imperium_api::RuleAction;

use crate::runtime::sh_quote;
use crate::search::{walker, withheld_by_rule};
use crate::{ToolError, Workspace, remote_files};

/// Most entries one search may visit, so a huge tree cannot stall the picker.
const MAX_VISITED: usize = 200_000;
/// The most matches one search returns.
pub const MAX_MATCHES: usize = 200;
/// How long the listing may take in a remote runtime.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(30);
/// Paths a remote listing returns at most.
const REMOTE_PATHS: usize = 50_000;

/// A file or directory the picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatch {
    /// The absolute path, as `session.attach` takes it.
    pub path: String,
    /// The session folder it is in.
    pub folder: String,
    /// The path inside that folder, with `/` separators.
    pub relative: String,
    pub directory: bool,
}

/// What a search found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSearch {
    pub matches: Vec<FileMatch>,
    /// More entries matched than were returned, or the walk stopped early.
    pub truncated: bool,
}

/// How well `query` matches `candidate`: `None` when its characters (spaces ignored) do not
/// appear in order, else a score that is higher for matches at the start of a path segment, in
/// one run, inside the file name, and in a short path. Case is ignored.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    let wanted: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let text: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut score = 0i64;
    let mut at = 0;
    let mut previous: Option<usize> = None;
    for &want in &wanted {
        let found = (at..text.len()).find(|&index| text[index] == want)?;
        score += 10;
        if previous.is_some_and(|last| last + 1 == found) {
            score += 15;
        }
        if found == 0 || matches!(text[found - 1], '/' | '-' | '_' | '.') {
            score += 20;
        }
        previous = Some(found);
        at = found + 1;
    }
    if !wanted.is_empty() {
        let start = text.iter().rposition(|&c| c == '/').map_or(0, |i| i + 1);
        let name: String = text[start..].iter().collect();
        let wanted: String = wanted.iter().collect();
        if name.contains(&wanted) {
            score += 80;
            if name.starts_with(&wanted) {
                score += 40;
            }
        }
    }
    Some(score - text.len() as i64 / 4)
}

/// Files and directories of the session's folders matching `query` (every one, shallowest
/// first, when it is empty), best first, at most `limit` (and [`MAX_MATCHES`]).
pub async fn find_files(
    workspace: &Workspace,
    query: &str,
    limit: usize,
) -> Result<FileSearch, ToolError> {
    let limit = limit.clamp(1, MAX_MATCHES);
    let mut found = if workspace.runtime().is_some() {
        remote_candidates(workspace, query).await?
    } else {
        let workspace = workspace.clone();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || local_candidates(&workspace, &query))
            .await
            .map_err(|error| ToolError::Io(error.to_string()))??
    };
    found.entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.relative.len().cmp(&b.1.relative.len()))
            .then_with(|| a.1.relative.cmp(&b.1.relative))
    });
    let truncated = found.stopped || found.entries.len() > limit;
    found.entries.truncate(limit);
    Ok(FileSearch {
        matches: found.entries.into_iter().map(|(_, entry)| entry).collect(),
        truncated,
    })
}

struct Candidates {
    entries: Vec<(i64, FileMatch)>,
    stopped: bool,
}

/// The score of a path for `query`; with no query, shallower paths come first.
fn score(query: &str, relative: &str) -> Option<i64> {
    if query.trim().is_empty() {
        Some(-(relative.matches('/').count() as i64) * 1000 - relative.len() as i64)
    } else {
        fuzzy_score(query, relative)
    }
}

fn local_candidates(workspace: &Workspace, query: &str) -> Result<Candidates, ToolError> {
    let mut entries = Vec::new();
    let mut visited = 0;
    let mut stopped = false;
    'folders: for folder in workspace.command_roots() {
        let base = workspace.resolve(&folder.to_string_lossy())?;
        let shown = crate::plain_path(&base).replace('\\', "/");
        for entry in walker(&base, true).build() {
            visited += 1;
            if visited > MAX_VISITED {
                stopped = true;
                break 'folders;
            }
            let Ok(entry) = entry else { continue };
            if entry.depth() == 0 {
                continue;
            }
            let mut full = entry.path().to_path_buf();
            let mut directory = entry.file_type().is_some_and(|kind| kind.is_dir());
            if entry.path_is_symlink() {
                // Only a link to a file inside the workspace counts, as for read_file.
                match workspace.resolve(&full.to_string_lossy()) {
                    Ok(target) if target.is_file() => full = target,
                    _ => continue,
                }
                directory = false;
            } else if !directory && !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&base)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            let Some(points) = score(query, &relative) else {
                continue;
            };
            let tools: &[&str] = if directory {
                &["list_dir", "read_file"]
            } else {
                &["read_file"]
            };
            if withheld_by_rule(workspace, tools, &full) {
                continue;
            }
            entries.push((
                points,
                FileMatch {
                    path: crate::plain_path(&full).replace('\\', "/"),
                    folder: shown.clone(),
                    relative,
                    directory,
                },
            ));
        }
    }
    Ok(Candidates { entries, stopped })
}

/// The files of each folder in the runtime (the repository's tracked and unignored files, else
/// everything outside `.git` and `node_modules`) and the directories they sit in.
async fn remote_candidates(workspace: &Workspace, query: &str) -> Result<Candidates, ToolError> {
    let mut entries = Vec::new();
    let mut stopped = false;
    for folder in workspace.folders_display() {
        let script = format!(
            "cd -- {} || exit 1; if git rev-parse --is-inside-work-tree >/dev/null 2>&1; \
             then git ls-files -co --exclude-standard; \
             else find . \\( -name .git -o -name node_modules \\) -prune -o -type f -print | sed 's|^\\./||'; \
             fi | head -n {}",
            sh_quote(&folder),
            REMOTE_PATHS + 1
        );
        let output = remote_files::command(workspace, &script, REMOTE_TIMEOUT).await?;
        if !output.success() {
            return Err(ToolError::Io(format!(
                "{folder}: {}",
                output.message().trim()
            )));
        }
        let listing = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut seen_dirs = HashSet::new();
        for (index, line) in listing.lines().enumerate() {
            if index >= REMOTE_PATHS {
                stopped = true;
                break;
            }
            let file = line.trim_end_matches('\r');
            if file.is_empty() {
                continue;
            }
            let mut paths = vec![(file.to_string(), false)];
            let mut parent = file;
            while let Some((dir, _)) = parent.rsplit_once('/') {
                if !seen_dirs.insert(dir.to_string()) {
                    break;
                }
                paths.push((dir.to_string(), true));
                parent = dir;
            }
            for (relative, directory) in paths {
                let Some(points) = score(query, &relative) else {
                    continue;
                };
                let path = format!("{}/{relative}", folder.trim_end_matches('/'));
                if rule_withholds(workspace, &path, directory) {
                    continue;
                }
                entries.push((
                    points,
                    FileMatch {
                        path,
                        folder: folder.clone(),
                        relative,
                        directory,
                    },
                ));
            }
        }
    }
    Ok(Candidates { entries, stopped })
}

fn rule_withholds(workspace: &Workspace, path: &str, directory: bool) -> bool {
    let permissions = workspace.permissions();
    if !permissions.has_ask_or_deny_rules() {
        return false;
    }
    let target = workspace.rule_target(path);
    let tools: &[&str] = if directory {
        &["list_dir", "read_file"]
    } else {
        &["read_file"]
    };
    tools.iter().any(|name| {
        matches!(
            permissions.evaluate(name, &target),
            Some(RuleAction::Deny | RuleAction::Ask)
        )
    })
}

/// Whether a permission rule asks about or denies reading `path`, which the user may name in a
/// message (`@path`); such a file is not attached, since an attachment is read without asking.
pub fn mention_withheld(workspace: &Workspace, path: &str) -> bool {
    rule_withholds(workspace, path, false)
}

/// A file in a remote runtime, at most `max_bytes` + 1 of it: a longer read is for the caller
/// to refuse as too large.
pub async fn read_remote_file(
    workspace: &Workspace,
    path: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, ToolError> {
    let full = workspace.resolve(path)?;
    let script = format!(
        "head -c {} -- {}",
        max_bytes + 1,
        sh_quote(&full.to_string_lossy())
    );
    let output = remote_files::command(workspace, &script, REMOTE_TIMEOUT).await?;
    if !output.success() {
        return Err(ToolError::Io(format!(
            "{path}: {}",
            output.message().trim()
        )));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_api::PermissionRule;
    use imperium_policy::permissions::{Permissions, SharedRules};

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn a() {}\n").unwrap();
        std::fs::write(root.join("src/nested/parser.rs"), "fn b() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "# readme\n").unwrap();
        std::fs::write(root.join("target/debug/out.rs"), "fn c() {}\n").unwrap();
        std::fs::write(root.join("build.log"), "log\n").unwrap();
        std::fs::write(root.join(".env"), "KEY=1\n").unwrap();
        dir
    }

    fn relatives(search: &FileSearch) -> Vec<String> {
        search.matches.iter().map(|m| m.relative.clone()).collect()
    }

    #[test]
    fn fuzzy_scores_order_subsequences_and_reject_the_rest() {
        assert!(fuzzy_score("xyz", "src/lib.rs").is_none());
        let name = fuzzy_score("lib", "src/lib.rs").unwrap();
        let scattered = fuzzy_score("lib", "src/lots/inner/b.rs").unwrap();
        assert!(name > scattered, "{name} vs {scattered}");
        assert!(fuzzy_score("LIB", "src/lib.rs").is_some());
        assert!(fuzzy_score("s l", "src/lib.rs").is_some());
        assert!(fuzzy_score("", "anything").is_some());
    }

    #[tokio::test]
    async fn the_listing_honours_gitignore_and_ranks_the_best_match_first() {
        let dir = tree();
        let workspace = Workspace::new(dir.path()).unwrap();
        let all = find_files(&workspace, "", 100).await.unwrap();
        let names = relatives(&all);
        assert!(names.contains(&"src/lib.rs".to_string()), "{names:?}");
        assert!(names.contains(&"src".to_string()), "{names:?}");
        assert!(names.contains(&".env".to_string()), "{names:?}");
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("target/") || n == "build.log"),
            "ignored files stay out: {names:?}"
        );
        assert!(!names.iter().any(|n| n.starts_with(".git/")), "{names:?}");
        let src = all.matches.iter().find(|m| m.relative == "src").unwrap();
        assert!(src.directory);
        assert!(!all.truncated);

        let parser = find_files(&workspace, "prsr", 100).await.unwrap();
        assert_eq!(parser.matches[0].relative, "src/nested/parser.rs");
        assert!(!parser.matches[0].directory);
        assert!(parser.matches[0].path.ends_with("src/nested/parser.rs"));

        let few = find_files(&workspace, "", 2).await.unwrap();
        assert_eq!(few.matches.len(), 2);
        assert!(few.truncated);
    }

    #[tokio::test]
    async fn rules_that_deny_or_ask_leave_a_file_out_of_the_listing() {
        let dir = tree();
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
        let found = find_files(&denied, "env", 100).await.unwrap();
        assert!(!relatives(&found).contains(&".env".to_string()));
        assert!(mention_withheld(&denied, ".env"));
        assert!(!mention_withheld(&denied, "README.md"));

        let asked = Workspace::new(dir.path())
            .unwrap()
            .with_permissions(Permissions::new(
                SharedRules::default(),
                SharedRules::new(vec![PermissionRule {
                    tool: "read_file".into(),
                    pattern: "src/**".into(),
                    action: RuleAction::Ask,
                }]),
            ));
        let found = find_files(&asked, "", 100).await.unwrap();
        let names = relatives(&found);
        assert!(!names.contains(&"src/lib.rs".to_string()), "{names:?}");
        assert!(names.contains(&"README.md".to_string()), "{names:?}");
    }

    #[tokio::test]
    async fn every_folder_of_the_workspace_is_listed_and_nothing_outside_it() {
        let first = tree();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(second.path().join("extra.txt"), "x").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
        let workspace = Workspace::new(first.path())
            .unwrap()
            .with_folders([second.path()])
            .unwrap();
        let found = find_files(&workspace, "", 100).await.unwrap();
        let names = relatives(&found);
        assert!(names.contains(&"extra.txt".to_string()), "{names:?}");
        assert!(!names.contains(&"secret.txt".to_string()), "{names:?}");
        assert!(found.matches.iter().all(|m| !m.path.contains("secret")));
    }
}
