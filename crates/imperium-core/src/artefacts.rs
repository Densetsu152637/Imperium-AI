//! Artefacts: large inputs kept out of an agent's context, and how they are split into chunks.
//!
//! An input larger than the threshold (see `Agent::guard_input`) is stored here and the agent
//! is told only its handle and an outline. `delegate_chunks` then splits it on structure
//! boundaries and gives each chunk to its own subagent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::session::lock;

/// Roughly four characters a token, for sizes given in tokens.
pub(crate) const CHARS_PER_TOKEN: usize = 4;
/// Longest outline, in lines.
const OUTLINE_LINES: usize = 12;
/// Longest outline line, in characters.
const OUTLINE_LINE_CHARS: usize = 100;

/// The text of every artefact in one session, by handle (`a1`, `a2`, ...).
#[derive(Default)]
pub(crate) struct Artefacts {
    next: AtomicU32,
    items: Mutex<HashMap<String, Arc<str>>>,
}

impl Artefacts {
    pub fn store(&self, text: &str) -> String {
        let id = format!(
            "a{}-{}",
            run_prefix(),
            self.next.fetch_add(1, Ordering::Relaxed) + 1
        );
        lock(&self.items).insert(id.clone(), text.into());
        id
    }

    pub fn get(&self, id: &str) -> Option<Arc<str>> {
        lock(&self.items).get(id).cloned()
    }
}

/// Four random hex digits fixed for this process, so a handle from before a restart (still
/// named in a resumed conversation) cannot match a new artefact.
fn run_prefix() -> &'static str {
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| {
        let mut bytes = [0u8; 2];
        // Without randomness the prefix is fixed, which only weakens the restart guard.
        let _ = getrandom::fill(&mut bytes);
        format!("{:02x}{:02x}", bytes[0], bytes[1])
    })
}

/// Approximate tokens in `text`, counting characters rather than bytes.
pub(crate) fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.chars().count() / CHARS_PER_TOKEN).unwrap_or(u64::MAX)
}

/// What an agent is shown in place of an input it must not read inline.
pub(crate) fn handle_message(id: &str, text: &str) -> String {
    let mut message = format!(
        "[This input is about {} tokens and {} lines, so it is stored as artefact {id} and is \
         not in your context.]\nOutline:\n",
        estimate_tokens(text),
        text.lines().count()
    );
    for line in outline(text) {
        message.push_str("- ");
        message.push_str(&line);
        message.push('\n');
    }
    message.push_str(&format!(
        "To read it, call delegate_chunks with artefact_id \"{id}\", a chunk_tokens that suits \
         your window, a brief for each subagent, and optionally a reduce instruction. You get \
         back only the subagents' reports."
    ));
    message
}

/// The headings and file starts of `text`, or its first lines when it has none.
fn outline(text: &str) -> Vec<String> {
    let clip = |line: &str| {
        line.trim()
            .chars()
            .take(OUTLINE_LINE_CHARS)
            .collect::<String>()
    };
    let mut in_fence = false;
    let mut marks = Vec::new();
    for line in text.lines() {
        if is_fence(line) {
            in_fence = !in_fence;
        } else if !in_fence && (is_heading(line) || is_file_start(line)) {
            marks.push(clip(line));
            if marks.len() == OUTLINE_LINES {
                break;
            }
        }
    }
    if marks.is_empty() {
        marks = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(3)
            .map(clip)
            .collect();
    }
    marks
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn is_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && line[hashes..].starts_with(' ')
}

/// A line that starts a new file in a concatenation: a git diff header, a `=== path ===`
/// banner, or a `<file ...>` tag.
fn is_file_start(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with("diff --git ")
        || (line.starts_with("=== ") && line.ends_with(" ==="))
        || line.starts_with("<file ")
}

/// A run of lines that stays together when it fits: a heading with its first paragraph, a
/// paragraph, or a fenced code block.
struct Unit {
    text: String,
    /// Starts a file, so a chunk never spans it.
    file_start: bool,
}

#[derive(Default)]
struct Units {
    done: Vec<Unit>,
    buffer: String,
    file_start: bool,
}

impl Units {
    fn flush(&mut self) {
        if !self.buffer.is_empty() {
            self.done.push(Unit {
                text: std::mem::take(&mut self.buffer),
                file_start: self.file_start,
            });
            self.file_start = false;
        }
    }
}

fn units(text: &str) -> Vec<Unit> {
    let mut units = Units::default();
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        if in_fence {
            units.buffer.push_str(line);
            if is_fence(line) {
                in_fence = false;
                units.flush();
            }
        } else if is_fence(line) {
            units.flush();
            units.buffer.push_str(line);
            in_fence = true;
        } else if is_file_start(line) {
            units.flush();
            units.file_start = true;
            units.buffer.push_str(line);
        } else if is_heading(line) {
            units.flush();
            units.buffer.push_str(line);
        } else {
            units.buffer.push_str(line);
            if line.trim().is_empty() {
                units.flush();
            }
        }
    }
    units.flush();
    units.done
}

/// Pieces of at most `limit` bytes, cut at the last line break in the second half of each
/// window when there is one.
fn windows(text: &str, limit: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while rest.len() > limit {
        let mut end = limit;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(newline) = rest[..end].rfind('\n')
            && newline + 1 > end / 2
        {
            end = newline + 1;
        }
        pieces.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        pieces.push(rest.to_string());
    }
    pieces
}

/// Splits `text` into chunks of about `chunk_tokens` tokens, on structure boundaries:
/// a chunk never spans the start of a file, and otherwise breaks between headings, paragraphs
/// (blank lines), and code blocks. A single piece larger than a chunk falls back to token
/// windows, cut at line breaks where possible.
pub(crate) fn split(text: &str, chunk_tokens: u64) -> Vec<String> {
    let limit = usize::try_from(chunk_tokens)
        .unwrap_or(usize::MAX)
        .saturating_mul(CHARS_PER_TOKEN)
        .max(CHARS_PER_TOKEN);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for unit in units(text) {
        let pieces = if unit.text.len() > limit {
            windows(&unit.text, limit)
        } else {
            vec![unit.text]
        };
        let mut file_start = unit.file_start;
        for piece in pieces {
            let full = current.len() + piece.len() > limit;
            if !current.is_empty() && (file_start || full) {
                chunks.push(std::mem::take(&mut current));
            }
            file_start = false;
            current.push_str(&piece);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_break_between_paragraphs_and_keep_code_blocks_whole() {
        let text = "# One\nfirst paragraph\n\nsecond paragraph\n\n```\ncode\n\nmore code\n```\n";
        // 8 tokens = 32 bytes: the fenced block (blank line included) stays in one piece.
        let chunks = split(text, 8);
        assert_eq!(chunks.concat(), text);
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.contains("code\n\nmore code"))
        );
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn a_chunk_never_spans_the_start_of_a_file() {
        let text = "=== a.rs ===\nfn a() {}\n=== b.rs ===\nfn b() {}\n";
        let chunks = split(text, 1_000);
        assert_eq!(
            chunks,
            ["=== a.rs ===\nfn a() {}\n", "=== b.rs ===\nfn b() {}\n"]
        );
    }

    #[test]
    fn text_without_structure_falls_back_to_token_windows() {
        let text = "x".repeat(1_000);
        let chunks = split(&text, 100);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 400));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn handles_carry_a_run_prefix_and_never_repeat() {
        let artefacts = Artefacts::default();
        let (first, second) = (artefacts.store("x"), artefacts.store("y"));
        assert_ne!(first, second);
        assert!(first.starts_with('a') && first.contains('-'));
        assert_eq!(artefacts.get(&first).as_deref(), Some("x"));
        assert!(artefacts.get("a1").is_none());
    }

    #[test]
    fn tokens_are_estimated_from_characters() {
        assert_eq!(estimate_tokens(&"é".repeat(8)), 2);
    }

    #[test]
    fn the_outline_lists_headings_outside_code() {
        let text = "# Title\n```\n# not a heading\n```\n## Part\nbody\n";
        assert_eq!(outline(text), ["# Title", "## Part"]);
        assert_eq!(
            outline("just\nsome\ntext\nhere\n"),
            ["just", "some", "text"]
        );
    }
}
