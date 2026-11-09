//! The retrieval primitives shared by memory search and session search: one tokenizer, the
//! embedding interface (a backend is optional; without one, search is keyword-only), vector
//! similarity, rank fusion for hybrid ranking, and snippets with highlighted matches.

use std::collections::HashSet;

/// Words too common to tell documents apart.
pub const STOPWORDS: [&str; 24] = [
    "the", "and", "for", "with", "that", "this", "are", "was", "but", "not", "you", "your", "use",
    "how", "what", "which", "when", "where", "who", "why", "can", "should", "from", "into",
];

/// The lowercase words of `text` at least `min_len` characters long, stopwords left out, in
/// order and with repeats. Words are runs of letters and digits, so paths and identifiers split
/// into their parts (`scripts/ci/build-desktop.sh` gives `scripts`, `ci`, `build`, `desktop`,
/// `sh`).
pub fn words(text: &str, min_len: usize) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= min_len)
        .map(str::to_lowercase)
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect()
}

/// The distinct words of a query, in the order they first appear.
pub fn query_terms(text: &str, min_len: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    words(text, min_len)
        .into_iter()
        .filter(|word| seen.insert(word.clone()))
        .collect()
}

/// Turns text into vectors whose cosine similarity reflects meaning. Implemented by the daemon
/// over an OpenAI-compatible `/embeddings` endpoint when `memory.embedding_model` is set, and by
/// a deterministic fake in tests.
pub trait Embedder: Send + Sync {
    /// Identifies the model, so vectors from another model are never compared with its own.
    fn model(&self) -> &str;

    /// One vector per text, in order.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
}

/// Cosine similarity of two vectors; 0 when either is empty, zero, or they differ in length.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut aa, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0.0 || bb == 0.0 {
        0.0
    } else {
        dot / (aa.sqrt() * bb.sqrt())
    }
}

/// The reciprocal-rank-fusion constant: larger values flatten the advantage of the top ranks.
pub const RRF_K: f64 = 60.0;

/// Reciprocal rank fusion of several rankings (each a list of keys, best first): every key
/// scores the sum of `1 / (RRF_K + rank)` over the lists it appears in.
pub fn fuse<K: Clone + Eq + std::hash::Hash>(rankings: &[Vec<K>]) -> Vec<(K, f64)> {
    let mut scores: std::collections::HashMap<K, f64> = std::collections::HashMap::new();
    let mut order: Vec<K> = Vec::new();
    for ranking in rankings {
        for (rank, key) in ranking.iter().enumerate() {
            let score = scores.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                0.0
            });
            *score += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
    }
    let mut fused: Vec<(K, f64)> = order
        .into_iter()
        .map(|key| {
            let score = scores[&key];
            (key, score)
        })
        .collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1));
    fused
}

/// A short excerpt of a text around its first match, with the matches marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    /// `[start, end)` ranges of matched words in `text`, in UTF-16 code units (as JavaScript
    /// strings index), in order.
    pub highlights: Vec<(u32, u32)>,
}

/// An excerpt of about `width` characters of `text` (whitespace collapsed) centred on the first
/// word that is one of `terms` (lowercase), with every such word highlighted. Without a match it
/// is the start of the text, with no highlights. Cut ends are marked with `…`.
pub fn snippet(text: &str, terms: &[String], width: usize) -> Snippet {
    let flat: Vec<char> = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let runs = word_runs(&flat);
    let is_term = |(start, end): (usize, usize)| {
        let word: String = flat[start..end].iter().collect::<String>().to_lowercase();
        terms.iter().any(|term| *term == word)
    };
    let first = runs.iter().copied().find(|run| is_term(*run));
    let (mut start, mut end) = match first {
        Some((at, _)) => {
            let start = at.saturating_sub(width / 3);
            (start, (start + width).min(flat.len()))
        }
        None => (0, width.min(flat.len())),
    };
    // Start and end on word boundaries where the text allows.
    if start > 0
        && let Some(space) = flat[start..end].iter().position(|c| *c == ' ')
        && start + space + 1 < end
    {
        start += space + 1;
    }
    if end < flat.len()
        && let Some(space) = flat[start..end].iter().rposition(|c| *c == ' ')
        && space > 0
    {
        end = start + space;
    }
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < flat.len() { "…" } else { "" };
    let mut out = String::from(prefix);
    let mut units = prefix.encode_utf16().count() as u32;
    let mut highlights = Vec::new();
    let mut cursor = start;
    for run in runs
        .iter()
        .copied()
        .filter(|(a, b)| *a >= start && *b <= end)
    {
        if !is_term(run) {
            continue;
        }
        for c in &flat[cursor..run.0] {
            out.push(*c);
            units += c.len_utf16() as u32;
        }
        let begin = units;
        for c in &flat[run.0..run.1] {
            out.push(*c);
            units += c.len_utf16() as u32;
        }
        highlights.push((begin, units));
        cursor = run.1;
    }
    out.extend(&flat[cursor..end]);
    out.push_str(suffix);
    Snippet {
        text: out,
        highlights,
    }
}

/// `[start, end)` character ranges of the runs of letters and digits.
fn word_runs(chars: &[char]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, c) in chars.iter().enumerate() {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, chars.len()));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_paths_and_drop_stopwords() {
        assert_eq!(
            words("Fix the scripts/ci/build-desktop.sh script", 2),
            ["fix", "scripts", "ci", "build", "desktop", "sh", "script"]
        );
    }

    #[test]
    fn snippets_centre_on_the_first_match_and_mark_every_match() {
        let text = format!(
            "{} the flaky websocket test fails on websocket close",
            "word ".repeat(40)
        );
        let terms = vec!["websocket".to_string()];
        let snip = snippet(&text, &terms, 60);
        assert!(snip.text.starts_with('…'));
        assert_eq!(snip.highlights.len(), 2);
        let units: Vec<u16> = snip.text.encode_utf16().collect();
        for (start, end) in &snip.highlights {
            let word = String::from_utf16(&units[*start as usize..*end as usize]).unwrap();
            assert_eq!(word, "websocket");
        }
    }

    #[test]
    fn snippets_without_a_match_show_the_start() {
        let snip = snippet("short text", &["absent".to_string()], 60);
        assert_eq!(snip.text, "short text");
        assert!(snip.highlights.is_empty());
    }

    #[test]
    fn fusion_rewards_keys_ranked_well_in_both_lists() {
        let fused = fuse(&[vec!["a", "b", "c"], vec!["b", "c", "a"]]);
        assert_eq!(fused[0].0, "b");
    }

    #[test]
    fn cosine_of_parallel_vectors_is_one() {
        assert!((cosine(&[1.0, 2.0], &[2.0, 4.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
    }
}
