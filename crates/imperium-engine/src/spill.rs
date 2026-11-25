//! Large tool results saved to a file instead of the conversation.
//!
//! A result past a threshold of the agent's context window is written under the session's
//! results folder, `<journal>.results/` beside its journal, which every agent of the session
//! may read (a read-only path grant). The conversation keeps a bounded preview (the first and
//! last lines) and the path, so the agent reads ranges with `read_file` or searches with
//! `grep`. The preview is what the conversation holds, so a resumed session still has the path;
//! the folder goes when the session is deleted, and stays when it is archived.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::artefacts::CHARS_PER_TOKEN;

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

/// Saves `text` in `dir`, which is made when missing.
pub(crate) fn save(dir: &Path, tool: &str, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file_name(tool));
    std::fs::write(&path, text)?;
    Ok(path)
}

/// The text the conversation keeps in place of a saved result: [`SPILL_MARKER`], what happened,
/// where the whole result is and how to read it, then the first and last lines, bounded in
/// characters. A result that began with an error keeps that line ahead of the marker.
pub(crate) fn notice(path: &Path, text: &str, threshold: usize) -> String {
    let budget = (threshold / PREVIEW_DIVISOR).min(MAX_PREVIEW_CHARS);
    let head_budget = budget * HEAD_PARTS / 3;
    let lines: Vec<&str> = text.lines().collect();
    let (head, tail) = split(&lines, head_budget, budget - head_budget);
    let omitted = lines.len() - head.len() - tail.len();
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
    if omitted > 0 {
        out.push_str(&format!("\n--- {omitted} lines omitted ---"));
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
}
