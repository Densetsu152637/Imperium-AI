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

/// Contents as stored, for a diff: empty for a missing file, `None` for a blob over 2 MiB (its
/// size is asked first, so a large one is never read).
fn contents(top: &Path, entry: Option<&Entry>) -> Result<Option<Vec<u8>>, String> {
    let Some(entry) = entry else {
        return Ok(Some(Vec::new()));
    };
    let size: u64 = text(&run(command(top).args(["cat-file", "-s", &entry.sha]))?)
        .parse()
        .map_err(|_| format!("git cat-file -s gave no size for {}", entry.sha))?;
    if size > MAX_DIFF_BYTES as u64 {
        return Ok(None);
    }
    blob(top, &entry.sha).map(Some)
}

/// Both sides as text, when neither is binary or too large.
fn texts(old: Option<Vec<u8>>, new: Option<Vec<u8>>) -> Option<(String, String)> {
    let (old, new) = (old?, new?);
    if is_binary(&old) || is_binary(&new) {
        return None;
    }
    Some((String::from_utf8(old).ok()?, String::from_utf8(new).ok()?))
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.len() > MAX_DIFF_BYTES
        || bytes[..bytes.len().min(8000)].contains(&0)
        || std::str::from_utf8(bytes).is_err()
}

/// A file's id: a digest of its path and both sides, so a whole-file action on a file that
/// changed since it was shown misses.
fn file_id(path: &str, base: Option<&Entry>, working: Option<&Entry>) -> String {
    let side = |entry: Option<&Entry>| {
        entry.map_or_else(|| "-".to_string(), |e| format!("{} {}", e.mode, e.sha))
    };
    let mut digest = Sha256::new();
    digest.update(format!("{path}\n{}\n{}", side(base), side(working)).as_bytes());
    digest
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The mode a file gets: accepting keeps the working tree's (a mode change is part of what is
/// accepted), rejecting puts the base's back.
fn result_mode(action: ReviewAction, base: Option<&Entry>, working: Option<&Entry>) -> String {
    let (first, second) = match action {
        ReviewAction::Accept => (working, base),
        ReviewAction::Reject => (base, working),
    };
    first
        .or(second)
        .map_or_else(|| "100644".to_string(), |entry| entry.mode.clone())
}

/// A line's text without its ending (`\r\n`, `\n`, or a lone `\r`, as the diff splits them).
fn line_text(value: &str) -> &str {
    value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .or_else(|| value.strip_suffix('\r'))
        .unwrap_or(value)
}

/// A hunk as computed: its ranges in the old and new lines (0-based, end exclusive) and its
/// lines for display.
struct Hunk {
    old: std::ops::Range<usize>,
    new: std::ops::Range<usize>,
    lines: Vec<String>,
}

fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    hunks_of(&TextDiff::configure().diff_lines(old, new))
}

fn hunks_of(diff: &TextDiff<'_, '_, '_, str>) -> Vec<Hunk> {
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
                    lines.push(format!("{sign}{}", line_text(change.value())));
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
    let both = texts(contents(&repo.top, base)?, contents(&repo.top, working)?);
    let binary = both.is_none();
    let hunks = match &both {
        None => Vec::new(),
        Some((old, new)) => hunks(old, new)
            .iter()
            .map(|hunk| to_wire(path, hunk))
            .collect(),
    };
    Ok(ReviewFile {
        id: file_id(path, base, working),
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
        gh: find_gh(state).is_some(),
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

/// Applies (accept: the base takes the hunk) or reverts (reject: the working text loses it) the
/// hunk named `id`, using the diff's own lines, so the ranges match whatever line endings the
/// text has. Returns the side's new text.
fn apply_hunk(
    path: &str,
    old: &str,
    new: &str,
    id: &str,
    action: ReviewAction,
) -> Result<String, String> {
    let diff = TextDiff::configure().diff_lines(old, new);
    let found = hunks_of(&diff)
        .into_iter()
        .find(|hunk| hunk_id(path, hunk) == id)
        .ok_or("that hunk has changed since it was shown; review the file again")?;
    let (old_lines, new_lines) = (diff.old_slices(), diff.new_slices());
    let parts: [&[&str]; 3] = match action {
        ReviewAction::Accept => [
            &old_lines[..found.old.start],
            &new_lines[found.new.clone()],
            &old_lines[found.old.end..],
        ],
        ReviewAction::Reject => [
            &new_lines[..found.new.start],
            &old_lines[found.old.clone()],
            &new_lines[found.new.end..],
        ],
    };
    Ok(parts.concat().concat())
}

/// Hashes `bytes` into the repository as they are.
fn store(top: &Path, bytes: Vec<u8>) -> Result<String, String> {
    run_with_input(
        command(top).args(["hash-object", "-w", "--no-filters", "--stdin"]),
        bytes,
    )
    .map(|out| text(&out))
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
    if !repo.touched.contains(path) {
        return Err(format!("{path} was not changed by this session's turns"));
    }
    let (base, working) = (repo.base.get(path), repo.working.get(path));
    if base == working {
        return Err(format!("{path} has no changes to review"));
    }
    // What the changing side's entry becomes: `None` removes the file.
    let result: Option<Entry> = if let Some(id) = params.hunk.strip_prefix("file:") {
        // The whole file, as it was when shown: accepting takes the working entry (contents
        // and mode) into the base, rejecting puts the base's back.
        if id != file_id(path, base, working) {
            return Err(format!(
                "{path} has changed since it was shown; review it again"
            ));
        }
        match params.action {
            ReviewAction::Accept => working.cloned(),
            ReviewAction::Reject => base.cloned(),
        }
    } else {
        let Some((old, new)) = texts(contents(&repo.top, base)?, contents(&repo.top, working)?)
        else {
            return Err(format!(
                "{path} is binary or large: accept or reject the whole file"
            ));
        };
        let joined = apply_hunk(path, &old, &new, &params.hunk, params.action)?;
        // A file only one side has, emptied, is removed rather than left empty.
        let one_sided = match params.action {
            ReviewAction::Accept => working.is_none(),
            ReviewAction::Reject => base.is_none(),
        };
        if one_sided && joined.is_empty() {
            None
        } else {
            Some(Entry {
                mode: result_mode(params.action, base, working),
                sha: store(&repo.top, joined.into_bytes())?,
            })
        }
    };
    match params.action {
        ReviewAction::Accept => {
            let mut files = repo.base.clone();
            match result {
                Some(entry) => files.insert(path.to_string(), entry),
                None => files.remove(path),
            };
            set_base(state, &params.session_id, &context, index, repo, &files)?;
        }
        ReviewAction::Reject => match result {
            Some(entry) => {
                write_file(&repo.top, path, &entry)?;
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

/// Keys in a repository's own config that make `git add`, `git commit`, or a checkout run a
/// program (lower case, as git compares them). Hooks and fsmonitor are not among them: every
/// daemon git call turns those off itself.
pub(crate) const COMMIT_REFUSED: &str =
    r"^(filter\..+\.(clean|smudge|process)|gpg\..*program|gpg\.ssh\.defaultkeycommand)$";

/// Keys that also make `git push` or `gh` run a program, reach a transport, or answer for
/// credentials: the commit keys plus these.
pub(crate) const PUSH_REFUSED: &str = r"^(filter\..+\.(clean|smudge|process)|gpg\..*program|gpg\.ssh\.defaultkeycommand|core\.(hookspath|sshcommand|gitproxy|askpass|fsmonitor|alternaterefscommand)|credential\..*|remote\..+\.(receivepack|uploadpack|vcs)|url\..+\.(insteadof|pushinsteadof)|protocol\..*)$";

/// Which list a step checks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// A checkout (`worktree add`): the commit list, without the signing check.
    Checkout,
    Commit,
    Push,
}

/// Refuses a repository whose own config (`.git/config` with its includes, and the worktree's
/// config) has any key the step's list names ([`COMMIT_REFUSED`], [`PUSH_REFUSED`]), or, for a
/// commit, turns on signing that the user's global config does not: those would run programs
/// (or reach places) a repository chose, and Imperium runs nothing a repository's config names.
/// The user's global config applies as usual. `what` says what to do instead.
pub(crate) fn check_repository_config(top: &Path, step: Step, what: &str) -> Result<(), String> {
    let pattern = match step {
        Step::Commit | Step::Checkout => COMMIT_REFUSED,
        Step::Push => PUSH_REFUSED,
    };
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
    // Signing the repository turned on, which the user did not turn on for themselves.
    let signs = |scope: &str| {
        run(command(top).args([
            "config",
            scope,
            "--includes",
            "--type=bool",
            "--get",
            "commit.gpgsign",
        ]))
        .is_ok_and(|out| text(&out) == "true")
    };
    if step == Step::Commit
        && (signs("--local") || signs("--worktree"))
        && !signs("--global")
        && !signs("--system")
    {
        found.push("commit.gpgsign".into());
    }
    found.dedup();
    if found.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "this repository's own config sets {}, which could make git run programs or reach \
             places the repository chose; Imperium does not run them, so {what} from a terminal",
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
    check_repository_config(&repo.top, Step::Commit, "commit")?;
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
    // `add` stages the session's files; if anything after it fails, the index goes back to
    // exactly what it was, so changes the user had staged in those files are not lost.
    let snapshot = IndexSnapshot::take(&repo.top);
    let committed = (|| -> Result<(), String> {
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
        run(&mut commit).map(|_| ())
    })();
    if let Err(error) = committed {
        snapshot.restore();
        return Err(error);
    }
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

/// A copy of a repository's index file, to put back when a step that stages files fails.
struct IndexSnapshot {
    path: Option<PathBuf>,
    /// The index file's bytes; `None` when there was no index file.
    bytes: Option<Vec<u8>>,
}

impl IndexSnapshot {
    fn take(top: &Path) -> Self {
        let mut locate = command(top);
        locate.args(["rev-parse", "--git-path", "index"]);
        let path = run(&mut locate)
            .ok()
            .and_then(|out| String::from_utf8(out).ok())
            .map(|path| top.join(path.trim()));
        let bytes = path.as_ref().and_then(|path| std::fs::read(path).ok());
        Self { path, bytes }
    }

    fn restore(self) {
        let Some(path) = self.path else { return };
        match self.bytes {
            Some(bytes) => {
                let _ = std::fs::write(&path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// `gh`: the one set for tests, else the one on the PATH, if there is one.
fn find_gh(state: &DaemonState) -> Option<PathBuf> {
    if let Some(gh) = state.gh_override() {
        return Some(gh);
    }
    let names: &[&str] = if cfg!(windows) {
        &["gh.exe", "gh.cmd", "gh"]
    } else {
        &["gh"]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// The shell the shown commands are written for: PowerShell on Windows, a POSIX shell
/// elsewhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shell {
    Posix,
    PowerShell,
}

impl Shell {
    fn here() -> Self {
        if cfg!(windows) {
            Shell::PowerShell
        } else {
            Shell::Posix
        }
    }
}

/// Quotes a word for `shell`: plain when it is only safe characters, else in single quotes,
/// inside which nothing is expanded. A POSIX `'` becomes `'\''`; PowerShell doubles every
/// single quote it accepts, typographic ones included, and quotes a word that starts with `@`
/// (splatting there).
fn quoted(word: &str, shell: Shell) -> String {
    let splat = shell == Shell::PowerShell && word.starts_with('@');
    if !word.is_empty()
        && !splat
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@".contains(c))
    {
        return word.to_string();
    }
    match shell {
        Shell::Posix => format!("'{}'", word.replace('\'', r"'\''")),
        Shell::PowerShell => {
            let mut out = String::from("'");
            for c in word.chars() {
                out.push(c);
                if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
                    out.push(c);
                }
            }
            out.push('\'');
            out
        }
    }
}

/// The push's arguments: the branch only, never submodules (whose own config would run).
fn push_args(refspec: &str) -> Vec<&str> {
    vec![
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "protocol.file.allow=never",
        "push",
        "--quiet",
        "--recurse-submodules=no",
        "-u",
        "--",
        "origin",
        refspec,
    ]
}

/// The commands that push the branch and open the pull request, to run by hand: the body is
/// read from `body_file`, and every other value is quoted for `shell`.
fn shown_commands(
    shell: Shell,
    branch: &str,
    title: &str,
    body_file: &Path,
    base: Option<&str>,
) -> String {
    let refspec = format!("refs/heads/{branch}");
    let mut create = format!(
        "gh pr create --head {} --title {} --body-file {}",
        quoted(branch, shell),
        quoted(title, shell),
        quoted(&body_file.display().to_string(), shell)
    );
    if let Some(base) = base {
        create.push_str(&format!(" --base {}", quoted(base, shell)));
    }
    format!(
        "git push --recurse-submodules=no -u origin -- {}\n{create}",
        quoted(&refspec, shell)
    )
}

/// `gh pr create` for the branch, with nothing from the repository's config able to run in
/// the git it starts: hooks (pointed at `no_hooks`, an empty folder) and fsmonitor off through
/// `GIT_CONFIG_*`, and git's own variables from the daemon's surroundings dropped.
fn gh_command(
    gh: &Path,
    top: &Path,
    branch: &str,
    title: &str,
    base: Option<&str>,
    no_hooks: &Path,
) -> std::process::Command {
    let mut command = std::process::Command::new(gh);
    command
        .current_dir(top)
        .args(["pr", "create", "--head", branch, "--title", title])
        .args(["--body-file", "-"]);
    if let Some(base) = base {
        command.args(["--base", base]);
    }
    for name in crate::safe_git::DROPPED_ENV {
        command.env_remove(name);
    }
    command
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "5")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_1", no_hooks)
        // Submodules are never entered (their own config would run).
        .env("GIT_CONFIG_KEY_2", "submodule.recurse")
        .env("GIT_CONFIG_VALUE_2", "false")
        .env("GIT_CONFIG_KEY_3", "diff.ignoreSubmodules")
        .env("GIT_CONFIG_VALUE_3", "all")
        .env("GIT_CONFIG_KEY_4", "push.recurseSubmodules")
        .env("GIT_CONFIG_VALUE_4", "no")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

/// `session.review_pull_request`: pushes the repository's current branch and opens a pull
/// request with `gh`. Without `gh`, it returns the commands to run instead.
pub(crate) fn pull_request(
    state: &DaemonState,
    params: &SessionReviewPullRequestParams,
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
    // In full, then without `refs/heads/`: `--short` can answer `heads/x` when a tag `x` exists.
    let branch = run(command(&top).args(["symbolic-ref", "--quiet", "HEAD"]))
        .ok()
        .and_then(|out| text(&out).strip_prefix("refs/heads/").map(str::to_string))
        .ok_or("HEAD is not on a branch; commit on a branch first")?;
    let base = params.base.as_deref().filter(|base| !base.is_empty());
    // Read as options by git or gh otherwise.
    if branch.starts_with('-') || base.is_some_and(|base| base.starts_with('-')) {
        return Err("a branch name starting with '-' cannot be pushed from here".into());
    }
    if params.title.trim().is_empty() {
        return Err("a pull request title is needed".into());
    }
    // The body goes in a file, so the shown commands carry only short, quoted values.
    let records = crate::checkpoints::records_dir(state.data_dir(), &params.session_id);
    std::fs::create_dir_all(&records).map_err(|error| error.to_string())?;
    let body_file = records.join("pull-request-body.md");
    std::fs::write(&body_file, &params.body).map_err(|error| error.to_string())?;
    let shown = shown_commands(
        Shell::here(),
        &branch,
        params.title.trim(),
        &body_file,
        base,
    );
    let Some(gh) = find_gh(state) else {
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
    check_repository_config(&top, Step::Push, "push and open the pull request")?;
    let refspec = format!("refs/heads/{branch}");
    run(command(&top)
        // Network transports only: never `file` or `ext` (a local program).
        .env("GIT_ALLOW_PROTOCOL", "https:http:ssh:git")
        .args(push_args(&refspec)))?;
    let no_hooks = records.join("no-hooks");
    std::fs::create_dir_all(&no_hooks).map_err(|error| error.to_string())?;
    let mut gh = gh_command(&gh, &top, &branch, params.title.trim(), base, &no_hooks);
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
        // Rejecting the second hunk reverts only it; accepting the first moves only it.
        let reverted = apply_hunk("x", old, new, &second.id, ReviewAction::Reject).unwrap();
        assert_eq!(reverted, "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\n");
        let accepted = apply_hunk("x", old, new, &first.id, ReviewAction::Accept).unwrap();
        assert_eq!(accepted, "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\n");
        assert!(apply_hunk("x", old, new, "stale", ReviewAction::Accept).is_err());
    }

    #[test]
    fn hunks_follow_the_diffs_own_lines_with_lone_carriage_returns() {
        // The diff splits at a lone `\r` too; the applied ranges must use the same lines.
        let old = "a\rb\nc\nd\ne\nf\ng\nh\ni\nj\nk\n";
        let new = "a\rB\nc\nd\ne\nf\ng\nh\ni\nj\nK\n";
        let found: Vec<ReviewHunk> = hunks(old, new).iter().map(|h| to_wire("x", h)).collect();
        assert_eq!(found.len(), 2);
        assert!(found[0].lines.contains(&"-b".to_string()));
        assert!(
            found[0].lines.contains(&" a".to_string()),
            "no stray \\r in the text"
        );
        let reverted = apply_hunk("x", old, new, &found[1].id, ReviewAction::Reject).unwrap();
        assert_eq!(reverted, "a\rB\nc\nd\ne\nf\ng\nh\ni\nj\nk\n");
        let accepted = apply_hunk("x", old, new, &found[0].id, ReviewAction::Accept).unwrap();
        assert_eq!(accepted, "a\rB\nc\nd\ne\nf\ng\nh\ni\nj\nk\n");
    }

    #[test]
    fn accepting_keeps_the_working_mode_and_rejecting_restores_the_base() {
        let base = Entry {
            mode: "100644".into(),
            sha: "a".into(),
        };
        let working = Entry {
            mode: "100755".into(),
            sha: "b".into(),
        };
        let accept = result_mode(ReviewAction::Accept, Some(&base), Some(&working));
        let reject = result_mode(ReviewAction::Reject, Some(&base), Some(&working));
        assert_eq!((accept.as_str(), reject.as_str()), ("100755", "100644"));
        assert_eq!(
            result_mode(ReviewAction::Reject, None, Some(&working)),
            "100755"
        );
        // A mode change alone changes the file's id, so a stale whole-file action misses.
        assert_ne!(
            file_id("x", Some(&base), Some(&working)),
            file_id("x", Some(&base), Some(&base))
        );
    }

    #[test]
    fn gh_runs_without_the_repositorys_hooks_or_fsmonitor() {
        let gh = gh_command(
            Path::new("gh"),
            Path::new("."),
            "feat/x",
            "Title",
            Some("main"),
            Path::new("/empty"),
        );
        let envs: std::collections::HashMap<String, Option<String>> = gh
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let get = |key: &str| envs.get(key).cloned().flatten();
        assert_eq!(get("GIT_CONFIG_COUNT").as_deref(), Some("5"));
        assert_eq!(
            get("GIT_CONFIG_KEY_2").as_deref(),
            Some("submodule.recurse")
        );
        assert_eq!(get("GIT_CONFIG_VALUE_2").as_deref(), Some("false"));
        assert_eq!(
            get("GIT_CONFIG_KEY_3").as_deref(),
            Some("diff.ignoreSubmodules")
        );
        assert_eq!(get("GIT_CONFIG_VALUE_3").as_deref(), Some("all"));
        assert_eq!(
            get("GIT_CONFIG_KEY_4").as_deref(),
            Some("push.recurseSubmodules")
        );
        assert_eq!(get("GIT_CONFIG_VALUE_4").as_deref(), Some("no"));
        assert_eq!(get("GIT_CONFIG_KEY_0").as_deref(), Some("core.fsmonitor"));
        assert_eq!(get("GIT_CONFIG_VALUE_0").as_deref(), Some("false"));
        assert_eq!(get("GIT_CONFIG_KEY_1").as_deref(), Some("core.hooksPath"));
        assert_eq!(get("GIT_CONFIG_VALUE_1").as_deref(), Some("/empty"));
        assert_eq!(
            envs.get("GIT_DIR"),
            Some(&None),
            "git's own variables are dropped"
        );
        let args: Vec<String> = gh
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "pr",
                "create",
                "--head",
                "feat/x",
                "--title",
                "Title",
                "--body-file",
                "-",
                "--base",
                "main"
            ]
        );
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
        use Shell::{Posix, PowerShell};
        assert_eq!(quoted("feat/x", Posix), "feat/x");
        assert_eq!(quoted("feat/x", PowerShell), "feat/x");
        assert_eq!(
            quoted("Add \"x\" $(rm -rf ~)", Posix),
            "'Add \"x\" $(rm -rf ~)'"
        );
        assert_eq!(quoted("it's", Posix), r"'it'\''s'");
        // A leading `@` is splatting in PowerShell, so it is quoted there only.
        assert_eq!(quoted("@args", PowerShell), "'@args'");
        assert_eq!(quoted("@args", Posix), "@args");
        assert_eq!(quoted("a@b", PowerShell), "a@b");
        // A title that would end the quotes and run a command, in each shell.
        let title = "x'; calc; 'y";
        assert_eq!(quoted(title, Posix), r"'x'\''; calc; '\''y'");
        assert_eq!(quoted(title, PowerShell), "'x''; calc; ''y'");
        assert_eq!(
            quoted("a\u{2019}b $env:X", PowerShell),
            "'a\u{2019}\u{2019}b $env:X'"
        );
        // The body is never on the command line, only the file it is in.
        for shell in [Posix, PowerShell] {
            let shown = shown_commands(shell, "main", title, Path::new("/b/body.md"), None);
            assert!(!shown.contains("--body "), "{shown}");
            assert!(shown.contains("--body-file /b/body.md"), "{shown}");
            assert!(shown.contains(&quoted(title, shell)), "{shown}");
            assert!(
                shown
                    .starts_with("git push --recurse-submodules=no -u origin -- refs/heads/main\n")
            );
        }
    }

    #[test]
    fn pushes_never_enter_submodules() {
        let args = push_args("refs/heads/main");
        assert!(args.contains(&"--recurse-submodules=no"));
        let at = args.iter().position(|arg| *arg == "--").unwrap();
        assert_eq!(&args[at..], ["--", "origin", "refs/heads/main"]);
    }
}
