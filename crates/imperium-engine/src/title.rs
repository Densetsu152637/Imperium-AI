//! Short task titles for subagents, so lists can say "Fix login redirect test" instead of `t3`.
//!
//! The delegating model may name a task with `spawn_agent`'s `title`; otherwise one is derived
//! from the brief: its first sentence that says what to do, without role preambles ("You are a
//! subagent...") or lead-ins ("Your task is to", "Please"), cut to a few words.

/// Most words a derived title keeps.
const MAX_WORDS: usize = 6;
/// Most words a given title keeps; the model was asked for 3 to 6.
const MAX_GIVEN_WORDS: usize = 8;
/// Most characters a title keeps.
const MAX_CHARS: usize = 60;
/// Shown when nothing usable is left.
const FALLBACK: &str = "Subagent task";

/// Lead-ins dropped from the start of a sentence, lower case, longest first where they overlap.
const LEAD_INS: &[&str] = &[
    "your task is to ",
    "your job is to ",
    "your goal is to ",
    "the task is to ",
    "i need you to ",
    "i want you to ",
    "we need to ",
    "you need to ",
    "you should ",
    "you must ",
    "please ",
    "could you ",
    "can you ",
    "task:",
    "goal:",
    "brief:",
    "objective:",
    "summary:",
    "to ",
];

/// Words a title does not end on.
const TRAILING: &[&str] = &[
    "a", "an", "the", "to", "of", "for", "and", "or", "in", "on", "with", "at", "by", "from",
    "into", "that", "which", "so", "as", "is", "are", "be",
];

/// The title for a task: `given` when the delegator named it (tidied and capped), else one
/// derived from `brief`.
pub fn task_title(given: Option<&str>, brief: &str) -> String {
    given
        .map(|title| tidy(title, MAX_GIVEN_WORDS))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| derive(brief))
}

/// The first line of `text`, without markdown or quotes, at most `max_words` words.
fn tidy(text: &str, max_words: usize) -> String {
    let line = text
        .lines()
        .map(strip_markers)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let line = line.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '_'));
    finish(line.split_whitespace().take(max_words).collect())
}

fn derive(brief: &str) -> String {
    for line in brief.lines() {
        let line = strip_markers(line);
        // A label such as "Context:" heads what follows; it says nothing itself.
        if line.is_empty() || (line.ends_with(':') && line.split_whitespace().count() <= 3) {
            continue;
        }
        for sentence in sentences(line) {
            let lower = sentence.to_lowercase();
            if lower.starts_with("you are ") || lower.starts_with("you're ") {
                continue;
            }
            let words: Vec<&str> = without_lead_ins(sentence)
                .split_whitespace()
                .take(MAX_WORDS)
                .collect();
            let title = finish(words);
            if !title.is_empty() {
                return title;
            }
        }
    }
    FALLBACK.to_string()
}

/// A line without list, heading, or quote markers.
fn strip_markers(line: &str) -> &str {
    let mut line = line.trim();
    loop {
        let stripped = line
            .trim_start_matches(['#', '>', '-', '*', '+'])
            .trim_start();
        // A numbered item: `1.` or `2)`.
        let digits = stripped.len()
            - stripped
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        let stripped = if digits > 0 && stripped[digits..].starts_with(['.', ')']) {
            stripped[digits + 1..].trim_start()
        } else {
            stripped
        };
        if stripped.len() == line.len() {
            return line;
        }
        line = stripped;
    }
}

/// A line's sentences: split after `.`, `?`, `!` or `;` followed by a space, and at a colon
/// or dash that follows a few words (a label such as "Goal: ..." is dropped as a lead-in).
fn sentences(line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = line.as_bytes();
    for (index, c) in line.char_indices() {
        let next_is_space = bytes.get(index + c.len_utf8()).is_some_and(|b| *b == b' ');
        let ends = match c {
            '.' | '?' | '!' | ';' => next_is_space,
            ':' | '—' => next_is_space && line[start..index].split_whitespace().count() > 1,
            _ => false,
        };
        if ends {
            parts.push(line[start..index].trim());
            start = index + c.len_utf8();
        }
    }
    parts.push(line[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

fn without_lead_ins(mut sentence: &str) -> &str {
    loop {
        let lower = sentence.to_lowercase();
        let Some(lead) = LEAD_INS.iter().find(|lead| lower.starts_with(**lead)) else {
            return sentence;
        };
        // Lead-ins are ASCII, so their length in `sentence` is the same.
        sentence = sentence[lead.len()..].trim_start();
    }
}

/// Joins `words` into a title: no trailing filler or punctuation, capitalised, capped.
fn finish(mut words: Vec<&str>) -> String {
    let bare = |word: &str| -> String {
        word.trim_end_matches(|c: char| matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | '—' | '-'))
            .to_string()
    };
    while let Some(last) = words.last() {
        let word = bare(last);
        if word.is_empty() || TRAILING.contains(&word.to_lowercase().as_str()) {
            words.pop();
        } else {
            break;
        }
    }
    let Some(last) = words.pop() else {
        return String::new();
    };
    let mut title = words.join(" ");
    if !title.is_empty() {
        title.push(' ');
    }
    title.push_str(&bare(last));
    let mut chars = title.chars();
    let title: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    if title.chars().count() > MAX_CHARS {
        let cut: String = title.chars().take(MAX_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::task_title;

    fn derived(brief: &str) -> String {
        task_title(None, brief)
    }

    #[test]
    fn a_given_title_is_used_tidied() {
        assert_eq!(
            task_title(Some("  \"fix the login redirect test\"  "), "anything"),
            "Fix the login redirect test"
        );
        assert_eq!(task_title(Some("## Audit\nsecond line"), "brief"), "Audit");
        // Too long: capped at eight words.
        assert_eq!(
            task_title(Some("one two three four five six seven eight nine ten"), ""),
            "One two three four five six seven eight"
        );
        // Blank: derived from the brief instead.
        assert_eq!(task_title(Some("   "), "Write the docs."), "Write the docs");
    }

    #[test]
    fn titles_come_from_the_first_sentence_that_says_what_to_do() {
        assert_eq!(
            derived("Find where the session queue is persisted and report the file and function."),
            "Find where the session queue"
        );
        assert_eq!(
            derived(
                "You are a subagent working on the desktop app. Fix the failing test in queue.test.ts."
            ),
            "Fix the failing test in queue.test.ts"
        );
        assert_eq!(
            derived("Your task is to review crates/imperium-core for unwraps; report each."),
            "Review crates/imperium-core for unwraps"
        );
        assert_eq!(
            derived("Context:\n\n## Goal: please audit the auth flow — focus on tokens"),
            "Audit the auth flow"
        );
        assert_eq!(derived("- 1. summarise README.md"), "Summarise README.md");
    }

    #[test]
    fn titles_do_not_end_on_filler_and_never_come_out_empty() {
        assert_eq!(
            derived("Compare the two parsers and the old one for speed"),
            "Compare the two parsers"
        );
        assert_eq!(derived(""), "Subagent task");
        assert_eq!(derived("   \n# \n"), "Subagent task");
        assert_eq!(derived("You are a helper."), "Subagent task");
        let long = derived(&"Supercalifragilisticexpialidocious ".repeat(6));
        assert!(long.chars().count() <= 60 && long.ends_with('…'), "{long}");
    }
}
