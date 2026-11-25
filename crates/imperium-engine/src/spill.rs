//! Large tool results saved to a file instead of the conversation.
//!
//! A result past a threshold of the agent's context window is written under the session's
//! results folder, `<journal>.results/` beside its journal, which every agent of the session
//! may read (a read-only path grant). The conversation keeps a bounded preview (the first and
//! last lines) and the path, so the agent reads ranges with `read_file` or searches with
//! `grep`. The preview is what the conversation holds, so a resumed session still has the path;
//! the folder goes when the session is deleted, and stays when it is archived.
//!
//! The folder is created, empty, when the session spawns, whether or not a result is ever
//! saved. It must be a real folder: one that is a link (a symbolic link, or a Windows junction)
//! is refused, and is never written to, read or followed ([`is_link`]). A session's saved
//! results are also capped in total size ([`MAX_SESSION_BYTES`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::artefacts::CHARS_PER_TOKEN;

pub use imperium_tools::is_link;

/// The window assumed when the agent's model has none in the table and no local runtime plans
/// one.
const DEFAULT_WINDOW_TOKENS: u64 = 32_768;
/// A result may take this share of the window, as a fraction (1/16): above it the result is
/// saved to a file.
const WINDOW_DIVISOR: u64 = 16;
/// The threshold never exceeds this many characters, however large the window.
const MAX_THRESHOLD_CHARS: usize = 64_000;
/// The share of the threshold the preview may take, as a fraction (1/2), and its ceiling.
const PREVIEW_DIVISOR: usize = 2;
const MAX_PREVIEW_CHARS: usize = 6_000;
/// The head holds this many parts of every three of the preview; the tail the rest.
const HEAD_PARTS: usize = 2;
/// The first line of the notice that stands in for a saved result: a stable marker, so code
/// that rewrites or drops old tool results (stale clearing) can recognise one and keep it.
pub const SPILL_MARKER: &str = "[spilled tool result]";
/// The words in every notice that lead to the path (`<SAVED_TO> <path>; read ranges ...`): what
/// stale clearing matches to leave a notice alone.
pub const SAVED_TO: &str = "Full output saved to";
/// The line the command tool puts between a command's stdout and its stderr.
const STDERR_MARKER: &str = "--- stderr ---";
/// The share of the preview (a fraction, 1/4) given to the start of the stderr section when it
/// would otherwise fall in the omitted middle.
const STDERR_DIVISOR: usize = 4;
/// All the results a session may save, in bytes. Past it a result stays in the conversation
/// (as when a write fails), so a runaway tool cannot fill the disk through the results folder.
pub const MAX_SESSION_BYTES: u64 = 512 * 1024 * 1024;
/// Most characters of the output's own first line kept ahead of the marker when the output
/// began with an error.
const ERROR_LINE_CHARS: usize = 200;
/// The notice must be at most this many percent of the result, or saving it gains too little
/// and the result stays whole.
const MAX_NOTICE_PERCENT: usize = 70;
/// Tools whose results are never saved: reads that are already bounded to the window (and
/// would send the agent in a circle on the saved file), workflow context that compaction keeps
/// verbatim, the tool lookup the next call needs whole, and the agent-protocol tools, whose
/// results are other agents' reports, the user's answers, or the agent's own notes.
const EXEMPT_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "context_load",
    "load_skill",
    "tool_search",
    "activate_plugin",
    "update_plan",
    "update_state",
    "spawn_agent",
    "await_tasks",
    "steer_agent",
    "queue_agent",
    "list_agents",
    "resume_agent",
    "keep_as_knowledge",
    "ask_agent",
    "list_knowledge_agents",
    "delegate_chunks",
    "ask_user",
    "request_clarification",
    "memory_search",
    "memory_read",
    "memory_remember",
    "memory_edit",
];

/// The folder a session's saved results are kept in, beside its journal.
pub fn results_dir(journal: &Path) -> PathBuf {
    journal.with_extension("results")
}

/// How many characters of a result the conversation keeps whole: a sixteenth of the window in
/// tokens (about four characters each), never below `floor` and never above a fixed ceiling.
pub(crate) fn threshold_chars(window: Option<u64>, floor: usize) -> usize {
    let tokens = window.unwrap_or(DEFAULT_WINDOW_TOKENS) / WINDOW_DIVISOR;
    let chars = usize::try_from(tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(CHARS_PER_TOKEN);
    chars.min(MAX_THRESHOLD_CHARS).max(floor)
}

/// Whether the result of `tool` is a candidate: not one of the exempt tools.
pub(crate) fn applies_to(tool: &str) -> bool {
    !EXEMPT_TOOLS.contains(&tool)
}

/// A name for the saved file that is safe on every platform and unique across restarts.
fn file_name(tool: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tool: String = tool
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(40)
        .collect();
    format!(
        "{:x}-{}-{tool}.txt",
        imperium_api::time::unix_ms(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Whether `text` is the notice that stands in for a saved result, by its structure: it opens
/// with [`SPILL_MARKER`], or with a line that began with `error` and then the marker (the error
/// line a failed result keeps ahead of it). Text that only mentions the marker or the words of a
/// notice is not one.
pub(crate) fn is_notice(text: &str) -> bool {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    if first.starts_with(SPILL_MARKER) {
        return true;
    }
    first
        .get(..5)
        .is_some_and(|start| start.eq_ignore_ascii_case("error"))
        && lines
            .next()
            .is_some_and(|line| line.starts_with(SPILL_MARKER))
}

/// Saves `text` in `dir`, which is made when missing. Refuses a `dir` that is a link and a
/// session whose saved results would pass [`MAX_SESSION_BYTES`].
pub(crate) fn save(dir: &Path, tool: &str, text: &str) -> std::io::Result<PathBuf> {
    save_within(dir, tool, text, MAX_SESSION_BYTES)
}

/// [`save`] with the session's total size limited to `cap` bytes.
pub(crate) fn save_within(
    dir: &Path,
    tool: &str,
    text: &str,
    cap: u64,
) -> std::io::Result<PathBuf> {
    refuse_link(dir)?;
    std::fs::create_dir_all(dir)?;
    // Looked at again after the folder exists: it may have been made as a link meanwhile.
    refuse_link(dir)?;
    if folder_bytes(dir).saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX)) > cap {
        return Err(std::io::Error::other(format!(
            "the session's saved tool results would pass {cap} bytes"
        )));
    }
    let path = dir.join(file_name(tool));
    // A new file only: a link planted under the name is never written through.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    Ok(path)
}

/// An error when `dir` is a link (never followed).
pub(crate) fn refuse_link(dir: &Path) -> std::io::Result<()> {
    if is_link(dir) {
        return Err(std::io::Error::other(format!(
            "the results folder {} is a link, which is never followed",
            dir.display()
        )));
    }
    Ok(())
}

/// The bytes of the regular files directly in `dir`.
fn folder_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|metadata| std::fs::Metadata::len(&metadata))
        .sum()
}

/// The text the conversation keeps in place of a saved result: [`SPILL_MARKER`], what happened,
/// where the whole result is and how to read it, then the first and last lines (and the start of
/// a command's stderr when it would fall in between), bounded in characters. A result that began with an error keeps that line ahead of the marker.
pub(crate) fn notice(path: &Path, text: &str, threshold: usize) -> String {
    let budget = (threshold / PREVIEW_DIVISOR).min(MAX_PREVIEW_CHARS);
    let head_budget = budget * HEAD_PARTS / 3;
    let lines: Vec<&str> = text.lines().collect();
    let (mut head, mut tail) = split(&lines, head_budget, budget - head_budget);
    // A command's stderr follows its stdout, so a long stdout pushes the start of the stderr
    // (where the error usually is) into the omitted middle. Keep its head too, from a share of
    // the budget.
    let mut stderr_head: Vec<String> = Vec::new();
    let mut stderr_at = 0;
    if let Some(marker) = lines.iter().position(|line| *line == STDERR_MARKER)
        && marker >= head.len()
        && marker < lines.len() - tail.len()
    {
        let share = budget / STDERR_DIVISOR;
        let head_budget = (budget - share) * HEAD_PARTS / 3;
        (head, tail) = split(&lines, head_budget, budget - share - head_budget);
        let end = lines.len() - tail.len();
        if marker >= head.len() && marker < end {
            (stderr_head, _) = split(&lines[marker..end], share, 0);
            stderr_at = marker;
        }
    }
    let omitted = lines.len() - head.len() - tail.len() - stderr_head.len();
    let first = text.trim_start().lines().next().unwrap_or_default();
    let began_with_error = first
        .get(..5)
        .is_some_and(|start| start.eq_ignore_ascii_case("error"));
    let mut out = String::new();
    if began_with_error {
        out.push_str(&cut(first, ERROR_LINE_CHARS));
        out.push('\n');
    }
    out.push_str(&format!(
        "{SPILL_MARKER} This result is large ({} lines, {} characters). {SAVED_TO} {}; read \
         ranges with read_file (offset and limit are in lines) or search it with grep.\n\
         --- first lines ---\n{}",
        lines.len(),
        text.chars().count(),
        path.display(),
        head.join("\n"),
    ));
    if stderr_head.is_empty() {
        if omitted > 0 {
            out.push_str(&format!("\n--- {omitted} lines omitted ---"));
        }
    } else {
        let before = stderr_at - head.len();
        let after = omitted - before;
        if before > 0 {
            out.push_str(&format!("\n--- {before} lines omitted ---"));
        }
        out.push('\n');
        out.push_str(&stderr_head.join("\n"));
        if after > 0 {
            out.push_str(&format!("\n--- {after} lines omitted ---"));
        }
    }
    if !tail.is_empty() {
        out.push_str("\n--- last lines ---\n");
        out.push_str(&tail.join("\n"));
    }
    out
}

/// Whether the notice for `text` is small enough, against the text, to be worth saving it.
pub(crate) fn worth_saving(dir: &Path, text: &str, threshold: usize) -> bool {
    let notice = notice(&dir.join(file_name("estimate")), text, threshold);
    notice.chars().count() * 100 <= text.chars().count() * MAX_NOTICE_PERCENT
}

/// The leading and trailing lines that fit the budgets (a line too long for what is left is
/// cut), without overlapping.
fn split(lines: &[&str], head_chars: usize, tail_chars: usize) -> (Vec<String>, Vec<String>) {
    let mut head = Vec::new();
    let mut used = 0;
    for line in lines {
        let room = head_chars.saturating_sub(used);
        if room == 0 {
            break;
        }
        let kept = cut(line, room);
        used += kept.chars().count() + 1;
        let whole = kept.len() == line.len();
        head.push(kept);
        if !whole {
            break;
        }
    }
    let mut tail = Vec::new();
    let mut used = 0;
    for line in lines[head.len()..].iter().rev() {
        let room = tail_chars.saturating_sub(used);
        if room == 0 {
            break;
        }
        // A line that does not fit whole is cut at its end, keeping what the line finished with.
        let count = line.chars().count();
        let kept = if count <= room {
            (*line).to_string()
        } else {
            line.chars().skip(count - room).collect()
        };
        used += kept.chars().count() + 1;
        let whole = count <= room;
        tail.push(kept);
        if !whole {
            break;
        }
    }
    tail.reverse();
    (head, tail)
}

/// `line` cut to `max` characters.
fn cut(line: &str, max: usize) -> String {
    line.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_threshold_follows_the_window_between_a_floor_and_a_ceiling() {
        assert_eq!(threshold_chars(Some(8_192), 4_000), 4_000);
        assert_eq!(threshold_chars(Some(131_072), 4_000), 32_768);
        assert_eq!(
            threshold_chars(Some(10_000_000), 4_000),
            MAX_THRESHOLD_CHARS
        );
        assert_eq!(threshold_chars(None, 4_000), 8_192);
        assert_eq!(threshold_chars(Some(1), 100), 100);
    }

    #[test]
    fn agent_protocol_tools_are_exempt() {
        for tool in [
            "await_tasks",
            "ask_agent",
            "delegate_chunks",
            "list_agents",
            "ask_user",
        ] {
            assert!(!applies_to(tool), "{tool}");
        }
    }

    #[test]
    fn an_error_stays_visible_and_the_marker_is_stable() {
        let text = format!(
            "error: exit 2
{}",
            "line
"
            .repeat(4_000)
        );
        let shown = notice(Path::new("f.txt"), &text, 4_000);
        assert!(
            shown.starts_with(
                "error: exit 2
[spilled tool result] "
            ),
            "{shown}"
        );
        let plain = notice(
            Path::new("f.txt"),
            &"line
"
            .repeat(4_000),
            4_000,
        );
        assert!(plain.starts_with(SPILL_MARKER));
    }

    #[test]
    fn a_result_the_notice_would_barely_shorten_is_not_worth_saving() {
        let dir = Path::new("/data/sessions/x.results");
        // Just over the preview's size: the notice would be nearly as long as the text.
        let barely = "line
"
        .repeat(480);
        assert!(!worth_saving(dir, &barely, 4_000));
        assert!(worth_saving(
            dir,
            &"line
"
            .repeat(20_000),
            4_000
        ));
    }

    #[test]
    fn reads_and_pinned_context_are_exempt() {
        for tool in EXEMPT_TOOLS {
            assert!(!applies_to(tool), "{tool}");
        }
        assert!(applies_to("run_command") && applies_to("grep"));
    }

    #[test]
    fn the_notice_names_the_file_and_keeps_both_ends_within_a_bound() {
        let text: String = (1..=2_000).map(|n| format!("line {n}\n")).collect();
        let path = Path::new("/data/results/x.txt");
        let shown = notice(path, &text, 4_000);
        assert!(
            shown.contains("saved to /data/results/x.txt")
                || shown.contains(&path.display().to_string())
        );
        assert!(shown.contains("read_file") && shown.contains("grep"));
        assert!(shown.contains("line 1\n") && shown.contains("line 2000"));
        assert!(shown.contains("lines omitted") && !shown.contains("line 1000\n"));
        assert!(shown.chars().count() < 2_600, "{}", shown.chars().count());
    }

    #[test]
    fn a_single_enormous_line_is_cut_to_the_preview_budget() {
        let text = format!("{}{}", "a".repeat(10_000), "z".repeat(10_000));
        let shown = notice(Path::new("f.txt"), &text, 4_000);
        assert!(shown.contains("aaaa") && !shown.contains("zzzz"), "{shown}");
        assert!(shown.chars().count() < 2_600);
    }

    #[test]
    fn save_writes_the_whole_text_under_a_unique_name() {
        let dir = tempfile::tempdir().unwrap();
        let results = dir.path().join("s.results");
        let first = save(&results, "run_command", "one").unwrap();
        let second = save(&results, "mcp/odd name", "two").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(first).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two");
        assert!(second.starts_with(&results));
        assert_eq!(
            results_dir(Path::new("a/id.jsonl")),
            Path::new("a/id.results")
        );
    }

    #[test]
    fn a_notice_is_recognised_by_its_structure() {
        let plain = notice(Path::new("f.txt"), &"line\n".repeat(4_000), 4_000);
        assert!(is_notice(&plain));
        let failed = notice(
            Path::new("f.txt"),
            &format!("error: exit 2\n{}", "line\n".repeat(4_000)),
            4_000,
        );
        assert!(is_notice(&failed), "{failed}");
        // Words of a notice elsewhere in a result do not make it one.
        assert!(!is_notice(&format!("see {SPILL_MARKER} in the log")));
        assert!(!is_notice(&format!(
            "output\n{SPILL_MARKER} on the second line"
        )));
        assert!(!is_notice(&format!("a line\nanother\n{SAVED_TO} x")));
        assert!(!is_notice(""));
    }

    #[test]
    fn the_head_of_stderr_stays_in_the_preview_when_stdout_is_long() {
        let text = format!(
            "exit code: 1\n--- stdout ---\n{}--- stderr ---\nerror[E0432]: unresolved import\n{}",
            "out line\n".repeat(3_000),
            "warning: later\n".repeat(3_000)
        );
        let shown = notice(Path::new("f.txt"), &text, 4_000);
        assert!(
            shown.contains("--- stderr ---\nerror[E0432]: unresolved import"),
            "{shown}"
        );
        assert!(shown.contains("exit code: 1") && shown.contains("warning: later"));
        assert!(shown.contains("lines omitted"));
        assert!(shown.chars().count() < 2_700, "{}", shown.chars().count());
    }

    #[test]
    fn stderr_already_in_view_is_not_repeated() {
        let text = format!(
            "exit code: 1\n--- stdout ---\nshort\n--- stderr ---\nboom\n{}",
            "x\n".repeat(5_000)
        );
        let shown = notice(Path::new("f.txt"), &text, 4_000);
        assert_eq!(shown.matches("--- stderr ---").count(), 1, "{shown}");
        assert!(shown.contains("boom"));
    }

    #[cfg(unix)]
    fn make_dir_link(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    /// A junction on Windows: made with `mklink /J`, which needs no privilege.
    #[cfg(windows)]
    fn make_dir_link(target: &Path, link: &Path) {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn a_linked_results_folder_is_refused_and_nothing_is_written_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let linked = dir.path().join("s.results");
        make_dir_link(&real, &linked);
        assert!(is_link(&linked));
        let error = save(&linked, "run_command", "secret").unwrap_err();
        assert!(error.to_string().contains("link"), "{error}");
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
        // A real folder is fine.
        assert!(!is_link(&real));
        assert!(save(&real, "run_command", "ok").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn a_symbolic_link_is_refused_too_where_the_account_may_make_one() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let linked = dir.path().join("s.results");
        // Needs the symlink privilege; the junction test covers the platform without it.
        if std::os::windows::fs::symlink_dir(&real, &linked).is_ok() {
            assert!(is_link(&linked));
            assert!(save(&linked, "grep", "x").is_err());
        }
    }

    #[test]
    fn a_name_that_is_taken_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("taken.txt");
        std::fs::write(&path, "old").unwrap();
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        assert!(options.open(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    }

    #[test]
    fn a_session_past_its_cap_keeps_the_result_in_the_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let results = dir.path().join("s.results");
        assert!(save_within(&results, "grep", "12345", 8).is_ok());
        let error = save_within(&results, "grep", "6789", 8).unwrap_err();
        assert!(error.to_string().contains("would pass"), "{error}");
        // Exactly the cap is allowed.
        assert!(save_within(&results, "grep", "678", 8).is_ok());
        assert_eq!(std::fs::read_dir(&results).unwrap().count(), 2);
    }
}
