//! Markdown helpers: links, heading sections and their anchors, and the terms used to match a
//! request against triggers.

use std::collections::BTreeSet;

/// A Markdown link on a line, with the text before it since the previous link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The words leading up to the link (`Incorrect behavior or incidents:`).
    pub lead: String,
    pub text: String,
    pub target: String,
}

/// Inline `[text](target)` links in `line`. Images and links without a target are skipped.
pub fn links(line: &str) -> Vec<Link> {
    let mut found = Vec::new();
    let mut rest = line;
    let mut lead_start = 0;
    let mut consumed = 0;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find("](") else { break };
        let target_part = &after[close + 2..];
        let Some(end) = target_part.find(')') else {
            break;
        };
        let is_image = open > 0 && rest.as_bytes()[open - 1] == b'!';
        let absolute_open = consumed + open;
        if !is_image {
            found.push(Link {
                lead: line[lead_start..absolute_open].to_string(),
                text: after[..close].to_string(),
                target: target_part[..end].trim().to_string(),
            });
        }
        let advance = open + 1 + close + 2 + end + 1;
        consumed += advance;
        lead_start = consumed;
        rest = &rest[advance..];
    }
    found
}

/// GitHub's heading anchor: lowercase, spaces to hyphens, punctuation dropped.
pub fn anchor(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// A heading and its body, up to the next heading of the same or a higher level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub anchor: String,
    pub level: usize,
    pub text: String,
}

/// Every heading section of a document (fenced code is not mistaken for headings).
pub fn sections(document: &str) -> Vec<Section> {
    let lines: Vec<&str> = document.lines().collect();
    let mut headings = Vec::new();
    let mut in_fence = false;
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let level = line.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&level) && line.as_bytes().get(level) == Some(&b' ') {
            headings.push((index, level, line[level + 1..].trim().to_string()));
        }
    }
    headings
        .iter()
        .enumerate()
        .map(|(i, (start, level, heading))| {
            let end = headings[i + 1..]
                .iter()
                .find(|(_, other, _)| other <= level)
                .map_or(lines.len(), |(index, _, _)| *index);
            Section {
                heading: heading.clone(),
                anchor: anchor(heading),
                level: *level,
                text: lines[*start..end].join("\n"),
            }
        })
        .collect()
}

const STOPWORDS: [&str; 30] = [
    "the", "and", "for", "with", "that", "this", "are", "was", "but", "not", "you", "your", "use",
    "how", "what", "which", "when", "where", "who", "why", "can", "should", "from", "into", "its",
    "any", "all", "has", "have", "need",
];

/// Matching terms: lowercase words of three letters or more, minus common filler words.
pub fn terms(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() >= 3 && !STOPWORDS.contains(&word.as_str()))
        .collect()
}

/// Whether two terms are the same word: equal, or one of four letters or more begins the
/// other (`debug` and `debugging`, `push` and `pushes`).
pub fn same_word(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short == long || (short.len() >= 4 && long.starts_with(short))
}

/// How many of `wanted` appear in `have`.
pub fn overlap(wanted: &BTreeSet<String>, have: &BTreeSet<String>) -> usize {
    wanted
        .iter()
        .filter(|term| have.iter().any(|other| same_word(term, other)))
        .count()
}

/// Rough tokens in a text: four characters a token.
pub fn tokens(text: &str) -> u32 {
    u32::try_from(text.len().div_ceil(4)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_keep_the_words_before_them() {
        let found = links(
            "- Local edits/Git mutations: [Git usage](guides/git-usage.md); parallel writes: [Worktrees](guides/git-worktrees.md#parallel-work).",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].lead, "- Local edits/Git mutations: ");
        assert_eq!(found[1].lead, "; parallel writes: ");
        assert_eq!(found[1].target, "guides/git-worktrees.md#parallel-work");
        assert!(links("![logo](a.png) plain").is_empty());
    }

    #[test]
    fn sections_split_by_level_and_ignore_fences() {
        let doc = "# Title\nintro\n## Shared validation rules\nrule\n```\n# not a heading\n```\n### Detail\nx\n## Next\ny\n";
        let found = sections(doc);
        let anchors: Vec<_> = found.iter().map(|s| s.anchor.as_str()).collect();
        assert_eq!(
            anchors,
            ["title", "shared-validation-rules", "detail", "next"]
        );
        assert!(found[1].text.contains("### Detail"));
        assert!(!found[1].text.contains("## Next"));
        assert_eq!(
            anchor("Success criteria for 1.0"),
            "success-criteria-for-10"
        );
    }

    #[test]
    fn related_word_forms_overlap() {
        let label = terms("Incorrect behavior or incidents: Debugging");
        assert_eq!(overlap(&terms("debug the crash"), &label), 1);
        assert_eq!(
            overlap(&terms("push the branch"), &terms("pushes/PRs/merges")),
            1
        );
        assert_eq!(overlap(&terms("the git log"), &terms("digit")), 0);
    }
}
