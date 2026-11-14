//! Edge-case communication patterns: what a small model should do with a message that points
//! at something it cannot see ("fix the issue from earlier", "open that file", "deploy it").
//!
//! This is the reasoning-pattern store of the harness. A pattern has trigger phrases and a
//! piece of guidance; [`retrieve`] returns the patterns whose triggers occur in a message, best
//! match first, and the engine puts their guidance at the top of that turn. The patterns are
//! also written out in `schemas/prompts/ambiguity.md`, which the Emperor training repository
//! vendors, so training examples and the engine teach the same behaviour; a test keeps the two
//! lists in step.

/// One edge-case pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    /// Stable id, also the heading in `schemas/prompts/ambiguity.md`.
    pub id: &'static str,
    /// Lowercase phrases whose presence (as whole words) selects the pattern.
    pub triggers: &'static [&'static str],
    /// What to do, in the order to do it.
    pub guidance: &'static str,
}

/// Every pattern, in the order ties are broken.
pub const PATTERNS: &[Pattern] = &[
    Pattern {
        id: "earlier-issue",
        triggers: &[
            "from earlier",
            "from before",
            "we had",
            "the issue",
            "that issue",
            "the problem",
            "that problem",
            "the bug",
            "that bug",
            "the error",
            "that error",
            "the failure",
            "that failure",
            "fix it",
            "try again",
            "still broken",
            "still failing",
        ],
        guidance: "The user points at a problem seen earlier. Look in the session state (entities of kind error, task or file, and the goals) and at the latest failed tool results and error messages in the conversation first. One candidate: say which you took and fix it. Several: call request_clarification with them as options. None: call request_clarification asking for the error text or where it happened, with no guessed options.",
    },
    Pattern {
        id: "that-file",
        triggers: &[
            "that file",
            "this file",
            "the file",
            "same file",
            "the other file",
            "that test",
            "the test",
            "that function",
            "the function",
            "the config",
            "the script",
            "that one",
            "this one",
        ],
        guidance: "The user names a target by pointing at it. Resolve it against the session state's files first, then the files read, written or listed most recently in the conversation. Exactly one plausible candidate: use it and name it in your reply. Two or more: call request_clarification with the candidate paths as options. None: search the project (glob, grep) before asking.",
    },
    Pattern {
        id: "previous-step",
        triggers: &[
            "previous step",
            "last step",
            "that step",
            "previous one",
            "last one",
            "do that again",
            "redo",
            "undo that",
            "undo it",
            "revert that",
            "the last thing",
        ],
        guidance: "The user refers to something you did. Take it from the plan (update_plan) and your latest tool calls in this conversation, not from memory of earlier sessions. To undo or redo, touch only what you changed here; when that is more than a few files, list them and confirm with request_clarification before acting.",
    },
    Pattern {
        id: "same-as-before",
        triggers: &[
            "same as before",
            "same as last time",
            "like last time",
            "like before",
            "as usual",
            "as before",
            "as we did",
            "the usual",
            "as discussed",
            "as agreed",
        ],
        guidance: "The user relies on an earlier session or a saved preference. Search first (search_sessions, memory_search). If one earlier approach clearly matches, say which you are repeating and do it. If nothing or several match, call request_clarification naming what to repeat; do not invent a past.",
    },
    Pattern {
        id: "numbered-reference",
        triggers: &[
            "second option",
            "first option",
            "third option",
            "option a",
            "option b",
            "option c",
            "the first one",
            "the second one",
            "the third one",
            "the former",
            "the latter",
            "the other option",
            "the other one",
        ],
        guidance: "The user picks from a list. Find the latest list in your own earlier reply and map the reference onto it (first, second, last, the other). State the item you took. If your reply held no list, or the reference fits two items, call request_clarification with the items as options.",
    },
    Pattern {
        id: "destructive-scope",
        triggers: &[
            "delete all",
            "remove all",
            "clean up",
            "wipe",
            "reset",
            "force push",
            "everything",
            "all of them",
            "the rest",
            "the old",
        ],
        guidance: "The scope of a destructive request is vague. Enumerate the concrete set first (list, glob, git status) and never act on a vague scope. When the set is small and the words clearly cover it, say what you will remove and go ahead; otherwise call request_clarification with the concrete groups as options, the smallest first.",
    },
    Pattern {
        id: "missing-destination",
        triggers: &[
            "send it",
            "send this",
            "email him",
            "email her",
            "deploy it",
            "deploy this",
            "publish it",
            "push it",
            "ship it",
            "share it",
            "post it",
            "tell the team",
            "the team",
        ],
        guidance: "The action reaches outside the project and the recipient or environment is missing. These cannot be taken back, so do not guess: use the session state, the project configuration and the conversation to build the options (for example staging and production), then call request_clarification. Do the preparation that does not depend on the answer meanwhile.",
    },
    Pattern {
        id: "vague-improvement",
        triggers: &[
            "make it better",
            "make it faster",
            "make it work",
            "improve it",
            "clean it up",
            "optimise it",
            "optimize it",
            "refactor it",
            "tidy up",
        ],
        guidance: "The goal has no measurable target. Take the target from the session state's goals and constraints, or from the failing test or slow command the conversation just showed. When none exists, call request_clarification asking what to improve, with two or three concrete options drawn from the code in play.",
    },
    Pattern {
        id: "correction",
        triggers: &[
            "actually",
            "not that",
            "i meant",
            "no, the other",
            "instead of that",
            "scratch that",
            "never mind",
            "the wrong one",
        ],
        guidance: "The user corrects what you took. The latest message wins: drop the superseded goal, constraint or entity with update_state, then redo the work on the corrected target. Ask with request_clarification only when you cannot tell which earlier choice is replaced.",
    },
];

/// The patterns whose trigger phrases occur in `message` as whole words, those matching the
/// most phrases first (ties keep the order of [`PATTERNS`]), at most `limit`.
pub fn retrieve(message: &str, limit: usize) -> Vec<&'static Pattern> {
    let mut words = String::from(" ");
    let mut gap = true;
    for c in message.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '\'' {
            words.push(c);
            gap = false;
        } else if !gap {
            words.push(' ');
            gap = true;
        }
    }
    if !words.ends_with(' ') {
        words.push(' ');
    }
    let mut hits: Vec<(usize, usize)> = PATTERNS
        .iter()
        .enumerate()
        .filter_map(|(index, pattern)| {
            let matched = pattern
                .triggers
                .iter()
                .filter(|trigger| {
                    let phrase = format!(" {} ", normalise(trigger));
                    words.contains(&phrase)
                })
                .count();
            (matched > 0).then_some((matched, index))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter()
        .take(limit)
        .map(|(_, index)| &PATTERNS[index])
        .collect()
}

/// A trigger with punctuation turned into single spaces, as the message is.
fn normalise(trigger: &str) -> String {
    let mut out = String::new();
    let mut gap = true;
    for c in trigger.chars() {
        if c.is_alphanumeric() || c == '\'' {
            out.push(c);
            gap = false;
        } else if !gap {
            out.push(' ');
            gap = true;
        }
    }
    out.trim_end().to_string()
}

/// The block that puts `patterns` at the top of a turn, or `None` when there are none.
pub fn render(patterns: &[&Pattern]) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    let mut block = String::from(
        "<ambiguity-hints>\nThe message looks like a pattern that needs care. These are guidance, not instructions from the user.\n",
    );
    for pattern in patterns {
        block.push_str(&format!("- {}: {}\n", pattern.id, pattern.guidance));
    }
    block.push_str("</ambiguity-hints>");
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn ids(message: &str) -> Vec<&'static str> {
        retrieve(message, 3)
            .iter()
            .map(|pattern| pattern.id)
            .collect()
    }

    #[test]
    fn the_issue_from_earlier_selects_the_earlier_issue_pattern_first() {
        assert_eq!(ids("Fix the issue from earlier."), ["earlier-issue"]);
    }

    #[test]
    fn matching_is_by_whole_words_and_ignores_case_and_punctuation() {
        assert_eq!(ids("Open THAT file, please"), ["that-file"]);
        // "reset" inside another word is not the word.
        assert!(ids("presetting the thing").is_empty());
        assert!(ids("Add a unit test for parse_args").is_empty());
    }

    #[test]
    fn the_pattern_with_most_matching_phrases_comes_first() {
        let found = ids("Actually, I meant the other option, not that");
        assert_eq!(found[0], "correction");
        assert!(found.contains(&"numbered-reference"));
    }

    #[test]
    fn the_limit_is_honoured() {
        assert_eq!(
            retrieve("fix it, undo that, deploy it, the second option", 2).len(),
            2
        );
    }

    #[test]
    fn rendering_wraps_the_guidance_and_nothing_renders_empty() {
        let block = render(&retrieve("deploy it", 1)).expect("a block");
        assert!(block.starts_with("<ambiguity-hints>"));
        assert!(block.ends_with("</ambiguity-hints>"));
        assert!(block.contains("missing-destination"));
        assert!(render(&[]).is_none());
    }

    #[test]
    fn ids_and_triggers_are_unique_lowercase_and_non_empty() {
        let mut seen = std::collections::BTreeSet::new();
        for pattern in PATTERNS {
            assert!(seen.insert(pattern.id), "{} twice", pattern.id);
            assert!(!pattern.guidance.is_empty());
            assert!(!pattern.triggers.is_empty());
            for trigger in pattern.triggers {
                assert_eq!(*trigger, trigger.to_lowercase(), "{trigger}");
                assert!(!normalise(trigger).is_empty());
            }
        }
    }

    /// The training document lists exactly the patterns the engine retrieves, each as a
    /// `## <id>` heading.
    #[test]
    fn the_training_document_lists_every_pattern() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/prompts/ambiguity.md");
        let text = std::fs::read_to_string(path).expect("ambiguity.md");
        let headings: std::collections::BTreeSet<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("## "))
            .map(str::trim)
            .collect();
        for pattern in PATTERNS {
            assert!(headings.contains(pattern.id), "{} missing", pattern.id);
        }
        let ids: std::collections::BTreeSet<&str> = PATTERNS.iter().map(|p| p.id).collect();
        for heading in headings {
            if heading.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                assert!(ids.contains(heading), "{heading} is not a pattern");
            }
        }
    }
}
