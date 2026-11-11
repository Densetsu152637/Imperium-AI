//! The review pane (roadmap item 6): what a session changed since one of its checkpoints, file by
//! file as unified diff hunks. Accepting a hunk keeps it and stops showing it (it joins the
//! review's base, a commit on `refs/imperium/review/<session>`); rejecting one reverts it in the
//! working tree. Then the changed files are committed, or the branch is pushed and a pull
//! request opened with `gh`; both are permission-gated (`git_commit`, `open_pull_request`) and
//! ask by default.
//!
//! Like checkpoints, everything runs through [`crate::safe_git`]: contents are read as stored,
//! never through a repository's filters or diff drivers, and diffs are computed here. The
//! commit is the one step that uses git's porcelain (`add`, `commit`), with hooks and fsmonitor
//! off; a repository whose own config names filter drivers or a signing program is refused, so
//! nothing a repository's config names runs in the daemon.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use imperium_protocol::methods::{
    ReviewAction, ReviewFile, ReviewHunk, SessionReviewCommitParams, SessionReviewCommitResult,
    SessionReviewHunkParams, SessionReviewHunkResult, SessionReviewParams,
    SessionReviewPullRequestParams, SessionReviewPullRequestResult, SessionReviewResult,
};
use imperium_tools::permissions::{RuleAction, Target};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};

use crate::checkpoints::{
    Entry, Files, blob, commit_files, commit_tree, differing, remove_file, safe_relative,
    write_file, write_tree,
};
use crate::safe_git::{self, command, run, run_with_input, text};
use crate::state::DaemonState;

/// Where review bases live in a repository.
pub(crate) const REF_PREFIX: &str = "refs/imperium/review";

/// Lines of context around each change.
const CONTEXT: usize = 3;

/// A file bigger than this is shown as changed without its lines.
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

/// The permission rule names for the two gated steps.
pub(crate) const COMMIT_TOOL: &str = "git_commit";
pub(crate) const PULL_REQUEST_TOOL: &str = "open_pull_request";

/// Which checkpoint a session's review compares against, and the base each repository's review
/// has reached by accepting hunks, kept beside the checkpoint records.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    since: u32,
    /// Repository root to its review base commit.
    bases: std::collections::BTreeMap<String, String>,
}

fn saved_path(state: &DaemonState, session_id: &str) -> PathBuf {
    crate::checkpoints::records_dir(state.data_dir(), session_id).join("review.json")
}

fn load(state: &DaemonState, session_id: &str, since: u32) -> Saved {
    std::fs::read_to_string(saved_path(state, session_id))
        .ok()
        .and_then(|text| serde_json::from_str::<Saved>(&text).ok())
        .filter(|saved| saved.since == since)
        .unwrap_or(Saved {
            since,
            bases: Default::default(),
        })
}

fn save(state: &DaemonState, session_id: &str, saved: &Saved) -> Result<(), String> {
    let path = saved_path(state, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(saved).map_err(|error| error.to_string())?;
    std::fs::write(path, text).map_err(|error| error.to_string())
}

/// The ref keeping the review base of the session's `index`th repository.
fn base_ref(session_id: &str, index: usize) -> String {
    if index == 0 {
        format!("{REF_PREFIX}/{session_id}")
    } else {
        format!("{REF_PREFIX}/{session_id}-{index}")
    }
}

/// One repository's review: the base's files and the working tree's.
struct RepoReview {
    top: PathBuf,
    base_commit: String,
    /// The files at the review's checkpoint.
    checkpoint: Files,
    /// The checkpoint with the accepted hunks applied.
    base: Files,
    /// The files the session's turns changed from the checkpoint on.
    touched: BTreeSet<String>,
    working: Files,
}

/// What a session's review starts from: its checkpoint, and each repository's base.
struct Context {
    since: u32,
    repos: Vec<RepoReview>,
    suggested_message: String,
    checkpoints: Vec<imperium_protocol::methods::SessionCheckpoint>,
}

fn context(
    state: &DaemonState,
    session_id: &str,
    since: Option<u32>,
) -> Result<Option<Context>, String> {
    let session = state
        .session(session_id)
        .ok_or_else(|| format!("no running session {session_id}"))?;
    let Some(checkpoints) = state.checkpoints_of(session_id) else {
        return Ok(None);
    };
    let list = checkpoints.list();
    // The session's start: its first checkpoint.
    let Some(chosen) = (match since {
        Some(number) => Some(
            list.iter()
                .find(|checkpoint| checkpoint.number == number)
                .ok_or_else(|| format!("no checkpoint {number}"))?,
        ),
        None => list.first(),
    }) else {
        return Ok(Some(Context {
            since: 0,
            repos: Vec::new(),
            suggested_message: String::new(),
            checkpoints: list,
        }));
    };
    let saved = load(state, session_id, chosen.number);
    let mut touched = checkpoints.touched_since(chosen.number);
    let mut repos = Vec::new();
    for repo in &chosen.repos {
        let top = PathBuf::from(&repo.root);
        let base_commit = saved
            .bases
            .get(&repo.root)
            .cloned()
            .unwrap_or_else(|| repo.commit.clone());
        let checkpoint = commit_files(&top, &repo.commit)?;
        let base = if base_commit == repo.commit {
            checkpoint.clone()
        } else {
            commit_files(&top, &base_commit)?
        };
        let working = checkpoints.working_files(&top)?;
        repos.push(RepoReview {
            top,
            base_commit,
            checkpoint,
            base,
            working,
            touched: touched.remove(&repo.root).unwrap_or_default(),
        });
    }
    let title = state.summary(&session).title;
    let mut context = Context {
        since: chosen.number,
        repos,
        suggested_message: String::new(),
        checkpoints: list.clone(),
    };
    context.suggested_message = suggested_message(&title, &changed_paths_all(&context));
    Ok(Some(context))
}

/// The paths that differ between a repository's base and working tree (submodules aside).
fn changed_paths(repo: &RepoReview) -> Vec<String> {
    session_paths(repo, &repo.base)
}

/// The files the session's turns changed that differ between `from` and the working tree.
fn session_paths(repo: &RepoReview, from: &Files) -> Vec<String> {
    differing(from, &repo.working)
        .into_iter()
        .filter(|path| repo.touched.contains(path))
        .collect()
}

/// Everything the session changed since the checkpoint, reviewed or not.
fn changed_paths_all(context: &Context) -> Vec<String> {
    context
        .repos
        .iter()
        .flat_map(|repo| session_paths(repo, &repo.checkpoint))
        .collect()
}

/// A commit message for the changes: the session's title (written by its model) as the
/// subject, else one made from the files, and the files in the body.
pub(crate) fn suggested_message(title: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = paths
        .iter()
        .map(|path| path.rsplit('/').next().unwrap_or(path))
        .collect();
    let subject = match title.trim() {
        "" if names.len() <= 3 => format!("Update {}", names.join(", ")),
        "" => format!(
            "Update {} and {} more files",
            names[..2].join(", "),
            names.len() - 2
        ),
        title => title.to_string(),
    };
    let mut message = subject;
    message.push_str("\n\n");
    for path in paths {
        message.push_str(&format!("- {path}\n"));
    }
    message
}

/// Contents as stored; empty for a missing file.
fn contents(top: &Path, entry: Option<&Entry>) -> Result<Vec<u8>, String> {
    match entry {
        Some(entry) => blob(top, &entry.sha),
        None => Ok(Vec::new()),
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.len() > MAX_DIFF_BYTES
        || bytes[..bytes.len().min(8000)].contains(&0)
        || std::str::from_utf8(bytes).is_err()
}

/// A hunk as computed: its ranges in the old and new lines (0-based, end exclusive) and its
/// lines for display.
struct Hunk {
    old: std::ops::Range<usize>,
    new: std::ops::Range<usize>,
    lines: Vec<String>,
}

fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let diff = TextDiff::configure().diff_lines(old, new);
    diff.grouped_ops(CONTEXT)
        .into_iter()
        .filter_map(|group| {
            let first = group.first()?;
            let last = group.last()?;
            let old = first.old_range().start..last.old_range().end;
            let new = first.new_range().start..last.new_range().end;
            let mut lines = Vec::new();
            for op in &group {
                for change in diff.iter_changes(op) {
                    let sign = match change.tag() {
                        ChangeTag::Equal => ' ',
                        ChangeTag::Delete => '-',
                        ChangeTag::Insert => '+',
                    };
                    let value = change.value();
                    let value = value
                        .strip_suffix('\n')
                        .map(|line| line.strip_suffix('\r').unwrap_or(line))
                        .unwrap_or(value);
                    lines.push(format!("{sign}{value}"));
                }
            }
            Some(Hunk { old, new, lines })
        })
        .collect()
}

/// A hunk's id: a digest of where it is and what it changes, so a stale request misses.
fn hunk_id(path: &str, hunk: &Hunk) -> String {
    let mut digest = Sha256::new();
    digest.update(path.as_bytes());
    digest.update(format!("{:?}{:?}", hunk.old, hunk.new).as_bytes());
    for line in &hunk.lines {
        digest.update(line.as_bytes());
        digest.update(b"\n");
    }
    digest
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn to_wire(path: &str, hunk: &Hunk) -> ReviewHunk {
    let count = |range: &std::ops::Range<usize>| u32::try_from(range.len()).unwrap_or(u32::MAX);
    // Unified diff numbering: 1-based, and the line before an empty range.
    let start = |range: &std::ops::Range<usize>| {
        let start = u32::try_from(range.start).unwrap_or(u32::MAX);
        if range.is_empty() { start } else { start + 1 }
    };
    let (old_start, old_lines) = (start(&hunk.old), count(&hunk.old));
    let (new_start, new_lines) = (start(&hunk.new), count(&hunk.new));
    ReviewHunk {
        id: hunk_id(path, hunk),
        header: format!("@@ -{old_start},{old_lines} +{new_start},{new_lines} @@"),
        old_start,
        old_lines,
        new_start,
        new_lines,
        lines: hunk.lines.clone(),
    }
}

/// One changed file as the review shows it.
fn review_file(repo: &RepoReview, path: &str) -> Result<ReviewFile, String> {
    let (base, working) = (repo.base.get(path), repo.working.get(path));
    let status = match (base, working) {
        (None, _) => "added",
        (_, None) => "deleted",
        _ => "modified",
    };
    let old = contents(&repo.top, base)?;
    let new = contents(&repo.top, working)?;
    let binary = is_binary(&old) || is_binary(&new);
    let hunks = if binary {
        Vec::new()
    } else {
        let (old, new) = (String::from_utf8_lossy(&old), String::from_utf8_lossy(&new));
        hunks(&old, &new)
            .iter()
            .map(|hunk| to_wire(path, hunk))
            .collect()
    };
    Ok(ReviewFile {
        repo: repo.top.display().to_string(),
        path: path.to_string(),
        status: status.to_string(),
        binary,
        hunks,
    })
}

/// `session.review`.
pub(crate) fn review(
    state: &DaemonState,
    params: &SessionReviewParams,
) -> Result<SessionReviewResult, String> {
    let Some(context) = context(state, &params.session_id, params.since)? else {
        return Ok(SessionReviewResult {
            available: false,
            reason: Some(
                crate::checkpoints::list(state, &params.session_id)?
                    .reason
                    .unwrap_or_else(|| "this session has no checkpoints".into()),
            ),
            ..SessionReviewResult::default()
        });
    };
    let mut files = Vec::new();
    for repo in &context.repos {
        for path in changed_paths(repo) {
            files.push(review_file(repo, &path)?);
        }
    }
    let repos = state
        .checkpoints_of(&params.session_id)
        .map(|checkpoints| {
            checkpoints
                .repositories()
                .iter()
                .map(|top| top.display().to_string())
                .collect()
        })
        .unwrap_or_default();
    Ok(SessionReviewResult {
        available: true,
        reason: None,
        since: (context.since > 0).then_some(context.since),
        checkpoints: context.checkpoints,
        files,
        suggested_message: context.suggested_message,
        repos,
        gh: find_gh().is_some(),
    })
}

fn find_repo<'a>(context: &'a Context, repo: &str) -> Result<(usize, &'a RepoReview), String> {
    let wanted = PathBuf::from(repo);
    context
        .repos
        .iter()
        .enumerate()
        .find(|(_, candidate)| candidate.top == wanted)
        .ok_or_else(|| format!("{repo} is not one of the session's repositories"))
}

/// Splits text into lines with their endings, as the diff numbers them.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// `session.review_hunk`: accepts a hunk (it joins the review base) or rejects it (it is
/// reverted in the working tree). `hunk: "file"` acts on the whole file (the only choice for a
/// binary file).
pub(crate) fn act_on_hunk(
    state: &DaemonState,
    params: &SessionReviewHunkParams,
) -> Result<SessionReviewHunkResult, String> {
    idle(state, &params.session_id)?;
    let context = context(state, &params.session_id, params.since)?
        .ok_or("this session has no checkpoints")?;
    if context.since == 0 {
        return Err("the session has changed nothing yet".into());
    }
    let (index, repo) = find_repo(&context, &params.repo)?;
    let path = params.path.as_str();
    if !safe_relative(path) {
        return Err(format!("{path}: not a path inside the repository"));
    }
    let (base, working) = (repo.base.get(path), repo.working.get(path));
    if base == working {
        return Err(format!("{path} has no changes to review"));
    }
    let old = contents(&repo.top, base)?;
    let new = contents(&repo.top, working)?;
    // What the file becomes on the side that changes: `None` removes it.
    let whole = params.hunk == "file";
    let result: Option<Vec<u8>> = if whole {
        match params.action {
            ReviewAction::Accept => working.map(|_| new.clone()),
            ReviewAction::Reject => base.map(|_| old.clone()),
        }
    } else {
        if is_binary(&old) || is_binary(&new) {
            return Err(format!("{path} is binary: accept or reject the whole file"));
        }
        let (old_text, new_text) = (String::from_utf8_lossy(&old), String::from_utf8_lossy(&new));
        let found = hunks(&old_text, &new_text)
            .into_iter()
            .find(|hunk| hunk_id(path, hunk) == params.hunk)
            .ok_or("that hunk has changed since it was shown; review the file again")?;
        let (old_lines, new_lines) = (lines(&old_text), lines(&new_text));
        let joined = match params.action {
            ReviewAction::Accept => [
                &old_lines[..found.old.start],
                &new_lines[found.new.clone()],
                &old_lines[found.old.end..],
            ]
            .concat()
            .concat(),
            ReviewAction::Reject => [
                &new_lines[..found.new.start],
                &old_lines[found.old.clone()],
                &new_lines[found.new.end..],
            ]
            .concat()
            .concat(),
        };
        // A file only one side has, emptied, is removed rather than left empty.
        let one_sided = match params.action {
            ReviewAction::Accept => working.is_none(),
            ReviewAction::Reject => base.is_none(),
        };
        (!(one_sided && joined.is_empty())).then(|| joined.into_bytes())
    };
    let mode = working
        .or(base)
        .map_or_else(|| "100644".to_string(), |entry| entry.mode.clone());
    match params.action {
        ReviewAction::Accept => {
            let mut files = repo.base.clone();
            match result {
                Some(bytes) => {
                    let sha = text(&run_with_input(
                        command(&repo.top).args(["hash-object", "-w", "--no-filters", "--stdin"]),
                        bytes,
                    )?);
                    files.insert(path.to_string(), Entry { mode, sha });
                }
                None => {
                    files.remove(path);
                }
            }
            set_base(state, &params.session_id, &context, index, repo, &files)?;
        }
        ReviewAction::Reject => match result {
            Some(bytes) => {
                let sha = text(&run_with_input(
                    command(&repo.top).args(["hash-object", "-w", "--no-filters", "--stdin"]),
                    bytes,
                )?);
                write_file(&repo.top, path, &Entry { mode, sha })?;
            }
            None => {
                remove_file(&repo.top, path)?;
            }
        },
    }
    // The file as it now is.
    let after = context_after(state, &params.session_id, context.since, &repo.top)?;
    let file = if changed_paths(&after).iter().any(|changed| changed == path) {
        Some(review_file(&after, path)?)
    } else {
        None
    };
    Ok(SessionReviewHunkResult { file })
}

/// One repository's review read again.
fn context_after(
    state: &DaemonState,
    session_id: &str,
    since: u32,
    top: &Path,
) -> Result<RepoReview, String> {
    let context = context(state, session_id, Some(since))?.ok_or("no checkpoints")?;
    context
        .repos
        .into_iter()
        .find(|repo| repo.top == top)
        .ok_or_else(|| "the repository left the review".to_string())
}

/// Makes `files` the review base of the `index`th repository.
fn set_base(
    state: &DaemonState,
    session_id: &str,
    context: &Context,
    index: usize,
    repo: &RepoReview,
    files: &Files,
) -> Result<(), String> {
    let dir = crate::checkpoints::records_dir(state.data_dir(), session_id);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let tree = write_tree(&repo.top, files, &dir.join("review-index.tmp"))?;
    let commit = commit_tree(
        &repo.top,
        &tree,
        Some(&repo.base_commit),
        &format!("Imperium review of session {session_id}"),
    )?;
    run(command(&repo.top).args(["update-ref", &base_ref(session_id, index), &commit]))?;
    let mut saved = load(state, session_id, context.since);
    saved.bases.insert(repo.top.display().to_string(), commit);
    save(state, session_id, &saved)
}

/// Refuses changes while a turn runs: the agent may be writing the same files.
fn idle(state: &DaemonState, session_id: &str) -> Result<(), String> {
    if state
        .checkpoints_of(session_id)
        .is_some_and(|checkpoints| checkpoints.turn_running())
    {
        return Err("a turn is running; wait for it to finish or stop it first".into());
    }
    Ok(())
}

/// What the permission rules say about `tool` for the session: `None` when they say ask.
fn permitted(state: &DaemonState, session_id: &str, tool: &str) -> Result<bool, String> {
    let session = state
        .session(session_id)
        .ok_or_else(|| format!("no running session {session_id}"))?;
    match session
        .workspace()
        .permissions()
        .evaluate(tool, &Target::Other)
    {
        Some(RuleAction::Deny) => Err(format!("a permission rule denies {tool}")),
        Some(RuleAction::Allow) => Ok(true),
        Some(RuleAction::Ask) | None => Ok(false),
    }
}

/// Refuses a repository whose own config (not the user's global one) names programs `git add`
/// or `git commit` would run: filter drivers and signing programs.
fn check_repository_config(top: &Path) -> Result<(), String> {
    let pattern = r"^(filter\..+\.(clean|smudge|process)|gpg\.(.+\.)?program|core\.hookspath)$";
    let mut found = Vec::new();
    for scope in ["--local", "--worktree"] {
        if let Ok(out) = run(command(top).args([
            "config",
            scope,
            "--includes",
            "--name-only",
            "--get-regexp",
            pattern,
        ])) {
            found.extend(String::from_utf8_lossy(&out).lines().map(str::to_string));
        }
    }
    if found.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this repository's own config names programs git would run ({}), which Imperium \
             does not run; commit from a terminal",
            found.join(", ")
        ))
    }
}

/// `session.review_commit`: commits the files the session changed since the review's
/// checkpoint, in one repository, as they now are (rejected hunks are already reverted).
pub(crate) fn commit(
    state: &DaemonState,
    params: &SessionReviewCommitParams,
) -> Result<SessionReviewCommitResult, String> {
    let allowed = permitted(state, &params.session_id, COMMIT_TOOL)?;
    idle(state, &params.session_id)?;
    let context = context(state, &params.session_id, params.since)?
        .ok_or("this session has no checkpoints")?;
    let (index, repo) = find_repo(&context, &params.repo)?;
    // Everything the session changed since the checkpoint, as it now is: accepted hunks kept,
    // rejected ones already reverted, unreviewed ones included.
    let paths = session_paths(repo, &repo.checkpoint);
    if paths.is_empty() {
        return Err("the session changed nothing to commit in this repository".into());
    }
    if !allowed && !params.confirmed {
        return Ok(SessionReviewCommitResult {
            needs_confirmation: true,
            files: paths,
            ..SessionReviewCommitResult::default()
        });
    }
    if params.message.trim().is_empty() {
        return Err("a commit message is needed".into());
    }
    check_repository_config(&repo.top)?;
    // A file git never knew that is gone again has nothing to commit (and git would refuse the
    // path).
    let known = run(command(&repo.top).args(["ls-files", "-z"]))?;
    let known: BTreeSet<&[u8]> = known.split(|byte| *byte == 0).collect();
    let paths: Vec<String> = paths
        .into_iter()
        .filter(|path| repo.working.contains_key(path) || known.contains(path.as_bytes()))
        .collect();
    if paths.is_empty() {
        return Err("the session changed nothing to commit in this repository".into());
    }
    let mut add = command(&repo.top);
    add.args(["add", "-A", "--"]).args(&paths);
    run(&mut add)?;
    let mut staged = command(&repo.top);
    staged
        .args([
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            "--",
        ])
        .args(&paths);
    if run(&mut staged).is_ok() {
        return Err("nothing to commit: these changes are already committed".into());
    }
    let mut commit = command(&repo.top);
    commit
        .args([
            "commit",
            "--no-verify",
            "--quiet",
            "-m",
            &params.message,
            "--",
        ])
        .args(&paths);
    run(&mut commit)?;
    let head = safe_git::head(&repo.top).unwrap_or_default();
    // What was committed is reviewed: the base becomes the working tree.
    let mut files = repo.base.clone();
    for path in &paths {
        match repo.working.get(path) {
            Some(entry) => files.insert(path.clone(), entry.clone()),
            None => files.remove(path),
        };
    }
    set_base(state, &params.session_id, &context, index, repo, &files)?;
    Ok(SessionReviewCommitResult {
        committed: true,
        needs_confirmation: false,
        commit: Some(head),
        files: paths,
    })
}

/// `gh` on the PATH, if there is one.
fn find_gh() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["gh.exe", "gh.cmd", "gh"]
    } else {
        &["gh"]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Quotes a word for the shown command.
fn quoted(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@".contains(c))
    {
        word.to_string()
    } else {
        format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// `session.review_pull_request`: pushes the repository's current branch and opens a pull
/// request with `gh`. Without `gh`, it returns the commands to run instead.
pub(crate) fn pull_request(
    state: &DaemonState,
    params: &SessionReviewPullRequestParams,
) -> Result<SessionReviewPullRequestResult, String> {
    pull_request_with(state, params, find_gh())
}

pub(crate) fn pull_request_with(
    state: &DaemonState,
    params: &SessionReviewPullRequestParams,
    gh: Option<PathBuf>,
) -> Result<SessionReviewPullRequestResult, String> {
    let allowed = permitted(state, &params.session_id, PULL_REQUEST_TOOL)?;
    idle(state, &params.session_id)?;
    let checkpoints = state
        .checkpoints_of(&params.session_id)
        .ok_or("this session has no checkpoints")?;
    let top = PathBuf::from(&params.repo);
    if !checkpoints.repositories().contains(&top) {
        return Err(format!(
            "{} is not one of the session's repositories",
            params.repo
        ));
    }
    let branch = run(command(&top).args(["symbolic-ref", "--quiet", "--short", "HEAD"]))
        .map(|out| text(&out))
        .map_err(|_| "HEAD is not on a branch; commit on a branch first".to_string())?;
    if params.title.trim().is_empty() {
        return Err("a pull request title is needed".into());
    }
    let mut create = format!(
        "gh pr create --head {} --title {} --body {}",
        quoted(&branch),
        quoted(params.title.trim()),
        quoted(&params.body)
    );
    if let Some(base) = params.base.as_deref().filter(|base| !base.is_empty()) {
        create.push_str(&format!(" --base {}", quoted(base)));
    }
    let shown = format!("git push -u origin {}\n{create}", quoted(&branch));
    let Some(gh) = gh else {
        return Ok(SessionReviewPullRequestResult {
            branch,
            command: shown,
            ..SessionReviewPullRequestResult::default()
        });
    };
    if !allowed && !params.confirmed {
        return Ok(SessionReviewPullRequestResult {
            needs_confirmation: true,
            branch,
            command: shown,
            ..SessionReviewPullRequestResult::default()
        });
    }
    run(command(&top).args(["push", "--quiet", "-u", "origin", &branch]))?;
    let mut gh = std::process::Command::new(gh);
    gh.current_dir(&top)
        .args([
            "pr",
            "create",
            "--head",
            &branch,
            "--title",
            params.title.trim(),
        ])
        .args(["--body-file", "-"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(base) = params.base.as_deref().filter(|base| !base.is_empty()) {
        gh.args(["--base", base]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        gh.creation_flags(0x0800_0000);
    }
    let out = run_with_input(&mut gh, params.body.clone().into_bytes())
        .map_err(|error| error.replacen("git ", "gh ", 1))?;
    let url = String::from_utf8_lossy(&out)
        .lines()
        .rev()
        .find(|line| line.starts_with("http"))
        .map(str::to_string);
    Ok(SessionReviewPullRequestResult {
        opened: true,
        needs_confirmation: false,
        url,
        branch,
        command: shown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks_number_like_unified_diffs_and_apply_both_ways() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\n";
        let found = hunks(old, new);
        assert_eq!(found.len(), 2, "far-apart changes are separate hunks");
        let first = to_wire("x", &found[0]);
        assert_eq!(first.header, "@@ -1,5 +1,5 @@");
        assert!(first.lines.contains(&"-b".to_string()));
        assert!(first.lines.contains(&"+B".to_string()));
        let second = to_wire("x", &found[1]);
        assert_eq!(second.header, "@@ -8,3 +8,4 @@");
        // Rejecting the second hunk reverts only it.
        let (o, n) = (lines(old), lines(new));
        let reverted = [
            &n[..found[1].new.start],
            &o[found[1].old.clone()],
            &n[found[1].new.end..],
        ]
        .concat()
        .concat();
        assert_eq!(reverted, "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\n");
    }

    #[test]
    fn messages_use_the_title_or_the_files() {
        let paths = vec!["src/a.rs".to_string(), "b.md".to_string()];
        assert!(
            suggested_message("Fix the parser", &paths).starts_with("Fix the parser\n\n- src/a.rs")
        );
        assert!(suggested_message("", &paths).starts_with("Update a.rs, b.md\n"));
        assert_eq!(suggested_message("x", &[]), "");
    }

    #[test]
    fn shown_commands_quote_what_needs_it() {
        assert_eq!(quoted("feat/x"), "feat/x");
        assert_eq!(quoted("Add \"x\""), "\"Add \\\"x\\\"\"");
    }
}
