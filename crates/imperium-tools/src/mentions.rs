//! The file picker behind `@` mentions: a fuzzy search over the session's folders (and the
//! project worktrees open beside them) that returns files and directories as the user may name
//! them. It walks like `glob` does: `.git` is skipped, `.gitignore` and `.ignore` files are
//! honoured, links leave the workspace for nowhere, and entries a permission rule asks about or
//! denies are left out. In a remote runtime the listing comes from `git ls-files` there (or
//! `find` outside a repository), covering the session's folders only (not worktrees, grants or
//! `.ignore` files), and each path is checked against the rules afterwards.
//!
//! The picker searches on every keystroke, so the walk's result (the candidate paths, before
//! rules and ranking) is kept for a few seconds per set of folders; each query only ranks it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use imperium_api::RuleAction;

use crate::runtime::sh_quote;
use crate::search::{walker, withheld_by_rule};
use crate::{ToolError, Workspace, remote_files};

/// Most entries one search may visit, so a huge tree cannot stall the picker.
const MAX_VISITED: usize = 200_000;
/// The longest a local walk may take before it stops with what it has.
const WALK_DEADLINE: Duration = Duration::from_secs(4);
/// The most matches one search returns.
pub const MAX_MATCHES: usize = 200;
/// How long the listing may take in a remote runtime.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(30);
/// The most a remote listing returns, in bytes; a longer one is cut at the last whole path.
const REMOTE_BYTES: usize = 8 * 1024 * 1024;
/// How long a listing is reused, and how many are kept.
const CACHE_TTL: Duration = Duration::from_secs(5);
const CACHE_SLOTS: usize = 8;

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

/// One walked path, before rules and ranking.
struct Entry {
    /// The resolved path on this machine (empty for a remote runtime).
    full: PathBuf,
    shown: FileMatch,
}

/// Every path of a set of folders.
struct Listing {
    entries: Vec<Entry>,
    stopped: bool,
}

type Slot = (String, Instant, Arc<Listing>);

static CACHE: Mutex<Vec<Slot>> = Mutex::new(Vec::new());

/// What identifies a listing: the folders it covers and whether the session is held to local
/// files (which decides which folders resolve). Rules are not part of it: they are applied to
/// each query's results.
fn cache_key(workspace: &Workspace) -> String {
    let mut key = String::new();
    if workspace.runtime().is_some() {
        key.push_str("remote:");
        key.push_str(&workspace.folders_display().join("\u{0}"));
    } else {
        key.push_str("local:");
        for root in workspace.command_roots() {
            key.push_str(&root.to_string_lossy());
            key.push('\u{0}');
        }
    }
    key.push_str(if workspace.profile().local_files_only() {
        "|locked"
    } else {
        "|open"
    });
    key
}

fn cached(key: &str) -> Option<Arc<Listing>> {
    let cache = CACHE.lock().ok()?;
    cache
        .iter()
        .find(|(held, at, _)| held == key && at.elapsed() < CACHE_TTL)
        .map(|(_, _, listing)| Arc::clone(listing))
}

fn keep(key: String, listing: Arc<Listing>) {
    let Ok(mut cache) = CACHE.lock() else { return };
    cache.retain(|(held, at, _)| held != &key && at.elapsed() < CACHE_TTL);
    if cache.len() >= CACHE_SLOTS {
        cache.remove(0);
    }
    cache.push((key, Instant::now(), listing));
}

/// Forgets the kept listing of `workspace`.
#[cfg(test)]
fn forget_listing(workspace: &Workspace) {
    let key = cache_key(workspace);
    CACHE.lock().unwrap().retain(|(held, _, _)| held != &key);
}

/// Files and directories of the session's folders matching `query` (every one, shallowest
/// first, when it is empty), best first, at most `limit` (and [`MAX_MATCHES`]).
pub async fn find_files(
    workspace: &Workspace,
    query: &str,
    limit: usize,
) -> Result<FileSearch, ToolError> {
    let limit = limit.clamp(1, MAX_MATCHES);
    let key = cache_key(workspace);
    let listing = match cached(&key) {
        Some(listing) => listing,
        None => {
            let fresh = if workspace.runtime().is_some() {
                remote_listing(workspace).await?
            } else {
                let workspace = workspace.clone();
                tokio::task::spawn_blocking(move || local_listing(&workspace))
                    .await
                    .map_err(|error| ToolError::Io(error.to_string()))?
            };
            let fresh = Arc::new(fresh);
            keep(key, Arc::clone(&fresh));
            fresh
        }
    };
    Ok(rank(workspace, &listing, query, limit))
}

/// The score of a path for `query`; with no query, shallower paths come first.
fn score(query: &str, relative: &str) -> Option<i64> {
    if query.trim().is_empty() {
        Some(-(relative.matches('/').count() as i64) * 1000 - relative.len() as i64)
    } else {
        fuzzy_score(query, relative)
    }
}

/// The best `limit` entries for `query` that no permission rule asks about or denies.
fn rank(workspace: &Workspace, listing: &Listing, query: &str, limit: usize) -> FileSearch {
    let mut scored: Vec<(i64, &Entry)> = listing
        .entries
        .iter()
        .filter_map(|entry| score(query, &entry.shown.relative).map(|points| (points, entry)))
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.shown.relative.len().cmp(&b.1.shown.relative.len()))
            .then_with(|| a.1.shown.relative.cmp(&b.1.shown.relative))
    });
    let mut matches = Vec::new();
    let mut more = false;
    for (_, entry) in scored {
        let withheld = if workspace.runtime().is_some() {
            rule_withholds(workspace, &entry.shown.path, entry.shown.directory)
        } else {
            let tools: &[&str] = if entry.shown.directory {
                &["list_dir", "read_file"]
            } else {
                &["read_file"]
            };
            withheld_by_rule(workspace, tools, &entry.full)
        };
        if withheld {
            continue;
        }
        if matches.len() == limit {
            more = true;
            break;
        }
        matches.push(entry.shown.clone());
    }
    FileSearch {
        matches,
        truncated: more || listing.stopped,
    }
}

fn local_listing(workspace: &Workspace) -> Listing {
    let started = Instant::now();
    let mut entries = Vec::new();
    let mut visited = 0;
    let mut stopped = false;
    'folders: for folder in workspace.command_roots() {
        // A folder that does not resolve (a worktree deleted since, or one outside what a
        // local-files-only session may use) is left out; the others still list.
        let Ok(base) = workspace.resolve(&folder.to_string_lossy()) else {
            continue;
        };
        let shown = crate::plain_path(&base).replace('\\', "/");
        for entry in walker(&base, true).build() {
            visited += 1;
            if visited > MAX_VISITED || (visited % 512 == 0 && started.elapsed() > WALK_DEADLINE) {
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
            entries.push(Entry {
                shown: FileMatch {
                    path: crate::plain_path(&full).replace('\\', "/"),
                    folder: shown.clone(),
                    relative,
                    directory,
                },
                full,
            });
        }
    }
    Listing { entries, stopped }
}

/// The paths of a NUL-separated listing (`git ls-files -z`, `find -print0`), which keeps names
/// with non-ASCII characters or line breaks whole. `cut` says the listing hit its size limit,
/// so its last path may be incomplete and is dropped. A leading `./` is removed.
fn split_listing(bytes: &[u8], cut: bool) -> Vec<String> {
    let mut parts: Vec<&[u8]> = bytes.split(|&byte| byte == 0).collect();
    // What follows the last NUL is empty in a whole listing, else an unfinished path.
    let tail = parts.pop().unwrap_or_default();
    if !cut && !tail.is_empty() {
        parts.push(tail);
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(|part| {
            let text = String::from_utf8_lossy(part);
            text.strip_prefix("./").unwrap_or(&text).to_string()
        })
        .collect()
}

/// The files of each folder in the runtime (the repository's tracked and unignored files, else
/// everything outside `.git` and `node_modules`) and the directories they sit in.
async fn remote_listing(workspace: &Workspace) -> Result<Listing, ToolError> {
    let mut entries = Vec::new();
    let mut stopped = false;
    for folder in workspace.folders_display() {
        let script = format!(
            "cd -- {} || exit 1; if git rev-parse --is-inside-work-tree >/dev/null 2>&1; \
             then git -c core.quotePath=false ls-files -z -co --exclude-standard; \
             else find . \\( -name .git -o -name node_modules \\) -prune -o -type f -print0; \
             fi | head -c {}",
            sh_quote(&folder),
            REMOTE_BYTES
        );
        let output = remote_files::command(workspace, &script, REMOTE_TIMEOUT).await?;
        if !output.success() {
            return Err(ToolError::Io(format!(
                "{folder}: {}",
                output.message().trim()
            )));
        }
        let cut = output.stdout.len() >= REMOTE_BYTES;
        stopped |= cut;
        let mut seen_dirs = HashSet::new();
        for file in split_listing(&output.stdout, cut) {
            let mut paths = vec![(file.clone(), false)];
            let mut parent = file.as_str();
            while let Some((dir, _)) = parent.rsplit_once('/') {
                if !seen_dirs.insert(dir.to_string()) {
                    break;
                }
                paths.push((dir.to_string(), true));
                parent = dir;
            }
            for (relative, directory) in paths {
                entries.push(Entry {
                    full: PathBuf::new(),
                    shown: FileMatch {
                        path: format!("{}/{relative}", folder.trim_end_matches('/')),
                        folder: folder.clone(),
                        relative,
                        directory,
                    },
                });
            }
        }
    }
    Ok(Listing { entries, stopped })
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

    #[test]
    fn a_nul_separated_listing_keeps_odd_names_whole() {
        let listing = "src/lib.rs\0docs/café notes.md\0odd\nname.txt\0./found.rs\0".as_bytes();
        assert_eq!(
            split_listing(listing, false),
            [
                "src/lib.rs",
                "docs/café notes.md",
                "odd\nname.txt",
                "found.rs"
            ]
        );
        // No final NUL: the last path is whole unless the listing was cut.
        assert_eq!(split_listing(b"a.rs\0b.rs", false), ["a.rs", "b.rs"]);
        assert_eq!(split_listing(b"a.rs\0b.r", true), ["a.rs"]);
        assert!(split_listing(b"", false).is_empty());
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

        // The kept listing is shared, but each workspace's own rules apply to what it returns.
        let open = Workspace::new(dir.path()).unwrap();
        let found = find_files(&open, "env", 100).await.unwrap();
        assert!(relatives(&found).contains(&".env".to_string()));

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

    #[tokio::test]
    async fn a_folder_that_no_longer_resolves_does_not_stop_the_others() {
        let first = tree();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(second.path().join("extra.txt"), "x").unwrap();
        let workspace = Workspace::new(first.path())
            .unwrap()
            .with_folders([second.path()])
            .unwrap();
        // The second folder is deleted after the workspace was made.
        drop(second);
        forget_listing(&workspace);
        let found = find_files(&workspace, "", 100).await.unwrap();
        let names = relatives(&found);
        assert!(names.contains(&"README.md".to_string()), "{names:?}");
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .is_ok_and(|out| out.status.success())
    }

    #[tokio::test]
    async fn a_worktree_a_local_files_only_session_cannot_open_is_skipped() {
        let parent = tempfile::tempdir().unwrap();
        let main = parent.path().join("main");
        let other = parent.path().join("other");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("a.txt"), "a").unwrap();
        if !(git(&main, &["init", "-q"])
            && git(&main, &["add", "."])
            && git(&main, &["commit", "-q", "-m", "x"])
            && git(&main, &["worktree", "add", "-q", other.to_str().unwrap()]))
        {
            return; // no usable git here
        }
        let locked = Workspace::new(&main)
            .unwrap()
            .with_profile(imperium_policy::AgentProfile::new(
                imperium_policy::ProfileRules {
                    local_files_only: true,
                    ..Default::default()
                },
            ))
            .with_project_worktrees();
        let found = find_files(&locked, "", 100).await.unwrap();
        let names = relatives(&found);
        assert!(names.contains(&"a.txt".to_string()), "{names:?}");
        assert!(
            found.matches.iter().all(|m| !m.path.contains("/other/")),
            "{:?}",
            found.matches
        );
    }

    #[tokio::test]
    async fn queries_reuse_the_kept_listing_instead_of_walking_again() {
        let dir = tree();
        let workspace = Workspace::new(dir.path()).unwrap();
        forget_listing(&workspace);
        let first = find_files(&workspace, "readme", 10).await.unwrap();
        assert_eq!(first.matches[0].relative, "README.md");
        // A file made after the walk is not seen until the listing expires.
        std::fs::write(dir.path().join("README-new.md"), "x").unwrap();
        let again = find_files(&workspace, "readme", 10).await.unwrap();
        assert_eq!(again.matches.len(), first.matches.len());
        forget_listing(&workspace);
        let fresh = find_files(&workspace, "readme", 10).await.unwrap();
        assert_eq!(fresh.matches.len(), first.matches.len() + 1);
    }

    #[tokio::test]
    async fn a_large_tree_is_walked_once_and_each_keystroke_only_ranks() {
        let dir = tempfile::tempdir().unwrap();
        for folder in 0..50 {
            let sub = dir.path().join(format!("pkg{folder}/src"));
            std::fs::create_dir_all(&sub).unwrap();
            for file in 0..100 {
                std::fs::write(sub.join(format!("file{file}.rs")), "x").unwrap();
            }
        }
        let workspace = Workspace::new(dir.path()).unwrap();
        forget_listing(&workspace);
        let first = find_files(&workspace, "pkg7 file42", 10).await.unwrap();
        assert!(!first.matches.is_empty());
        let started = Instant::now();
        for query in ["f", "fi", "fil", "file", "file4", "file42"] {
            find_files(&workspace, query, 10).await.unwrap();
        }
        // Six keystrokes over 5,000 files rank in well under the time of one walk.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }
}
