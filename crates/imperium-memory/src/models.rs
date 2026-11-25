//! Model cards in the retrieval layer: finds the models whose cards (the user's notes, what
//! the model's metadata says, and Imperium's measurements) best match a task description, for
//! agents choosing a model to delegate to (`list_models` with a `query`).
//!
//! The cards are few (one per model the session may use), so they are ranked in memory on each
//! search: by keyword, on the tokenizer the other searches share ([`crate::retrieval`]), with
//! words reduced to a rough stem and task words mapped to the strengths cards name (a "refactor"
//! is coding, an "essay" is writing); and, when an embedding model is configured, by meaning
//! too, the two rankings fused ([`crate::retrieval::fuse`]). Card vectors are kept by text, so
//! a card is embedded again only when it changes.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use crate::retrieval::{cosine, fuse, words};

/// Task words and the strength each one points to, as a stem ([`stem`]) the cards share.
const CONCEPTS: [(&str, &[&str]); 9] = [
    (
        "cod",
        &[
            "program",
            "programming",
            "software",
            "debug",
            "bug",
            "bugs",
            "refactor",
            "refactoring",
            "implement",
            "compile",
            "compiler",
            "function",
            "functions",
            "rust",
            "python",
            "typescript",
            "javascript",
            "java",
            "golang",
            "sql",
            "test",
            "tests",
            "repository",
            "repo",
            "script",
            "scripts",
            "api",
            "developer",
        ],
    ),
    (
        "writ",
        &[
            "essay",
            "essays",
            "prose",
            "story",
            "stories",
            "fiction",
            "blog",
            "article",
            "copy",
            "copywriting",
            "email",
            "letter",
            "draft",
            "poem",
            "poetry",
            "creative",
            "narrative",
            "edit",
            "editing",
            "proofread",
        ],
    ),
    (
        "research",
        &[
            "investigate",
            "literature",
            "sources",
            "source",
            "survey",
            "citation",
            "citations",
            "cite",
            "papers",
            "paper",
            "study",
            "studies",
            "compare",
            "facts",
            "evidence",
        ],
    ),
    (
        "reason",
        &[
            "logic",
            "proof",
            "proofs",
            "puzzle",
            "plan",
            "planning",
            "think",
            "thinking",
            "step",
            "hard",
            "tricky",
            "architecture",
            "design",
            "algorithm",
        ],
    ),
    (
        "math",
        &[
            "maths",
            "mathematics",
            "equation",
            "calculus",
            "algebra",
            "arithmetic",
        ],
    ),
    (
        "imag",
        &[
            "vision",
            "screenshot",
            "screenshots",
            "picture",
            "pictures",
            "photo",
            "photos",
            "diagram",
            "diagrams",
            "chart",
            "charts",
            "ocr",
            "visual",
        ],
    ),
    ("tool", &["agentic", "agent", "calling", "call"]),
    (
        "summar",
        &["summary", "summaries", "tldr", "digest", "condense"],
    ),
    (
        "translat",
        &[
            "translate",
            "translation",
            "multilingual",
            "language",
            "languages",
        ],
    ),
];

/// Suffixes stripped by [`stem`], longest first.
const SUFFIXES: [&str; 9] = [
    "ations", "ation", "ings", "ing", "ers", "er", "ed", "es", "s",
];

/// A rough stem: `coding`, `coder` and `code` all give `cod`; `writing`, `writer` and `write`
/// give `writ`. Short words are left alone.
pub fn stem(word: &str) -> String {
    let word = word.to_lowercase();
    for suffix in SUFFIXES {
        if let Some(base) = word.strip_suffix(suffix)
            && base.chars().count() >= 3
        {
            return base.trim_end_matches('e').to_string();
        }
    }
    if word.chars().count() > 3 {
        word.trim_end_matches('e').to_string()
    } else {
        word
    }
}

/// The concepts of `text`: the stems of its words, plus the strength any task word points to.
fn concepts(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for word in words(text, 2) {
        for (strength, cues) in CONCEPTS {
            if cues.contains(&word.as_str()) {
                out.insert(strength.to_string());
            }
        }
        out.insert(stem(&word));
    }
    out
}

/// One model's card as the search reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCard {
    /// What `spawn_agent` names the model by.
    pub id: String,
    pub text: String,
}

/// A card that matched, best first.
#[derive(Debug, Clone, PartialEq)]
pub struct CardHit {
    pub id: String,
    pub score: f64,
    /// The concepts of the query the card shares, for the reader.
    pub matched: Vec<String>,
}

/// Card vectors kept between searches, by embedding model and card text.
#[derive(Debug, Default)]
pub struct CardIndex {
    vectors: Mutex<HashMap<(String, String), Vec<f32>>>,
}

impl CardIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// The card texts that `model` has no vector for yet, each once.
    pub fn unembedded(&self, model: &str, cards: &[ModelCard]) -> Vec<String> {
        let vectors = self.vectors.lock().unwrap_or_else(PoisonError::into_inner);
        let mut seen = HashSet::new();
        cards
            .iter()
            .filter(|card| !vectors.contains_key(&(model.to_string(), card.text.clone())))
            .filter(|card| seen.insert(card.text.clone()))
            .map(|card| card.text.clone())
            .collect()
    }

    /// Keeps `vectors` for `texts` (in order) under `model`. Vectors of texts no card has any
    /// more are dropped when `keep` (the current cards) is given.
    pub fn store(&self, model: &str, texts: &[String], vectors: Vec<Vec<f32>>, keep: &[ModelCard]) {
        let mut stored = self.vectors.lock().unwrap_or_else(PoisonError::into_inner);
        let current: HashSet<&str> = keep.iter().map(|card| card.text.as_str()).collect();
        stored.retain(|(_, text), _| current.contains(text.as_str()));
        for (text, vector) in texts.iter().zip(vectors) {
            stored.insert((model.to_string(), text.clone()), vector);
        }
    }

    /// The cards that match `query`, best first, at most `limit`. By keyword alone, only cards
    /// sharing a concept with the query are returned; with `query_vector` (from `model`), the
    /// meaning ranking is fused in and every card with a vector takes part.
    pub fn search(
        &self,
        query: &str,
        cards: &[ModelCard],
        query_vector: Option<(&str, &[f32])>,
        limit: usize,
    ) -> Vec<CardHit> {
        let wanted = concepts(query);
        let card_concepts: Vec<HashSet<String>> =
            cards.iter().map(|card| concepts(&card.text)).collect();
        let count = cards.len() as f64;
        // Each shared concept adds its inverse document frequency, so a word every card has
        // (such as "model") counts for little.
        let mut keyword: Vec<(usize, f64, Vec<String>)> = cards
            .iter()
            .enumerate()
            .map(|(n, _)| {
                let mut score = 0.0;
                let mut matched = Vec::new();
                for concept in &wanted {
                    if !card_concepts[n].contains(concept) {
                        continue;
                    }
                    let containing = card_concepts.iter().filter(|c| c.contains(concept)).count();
                    score += (1.0 + count / containing as f64).ln();
                    matched.push(concept.clone());
                }
                matched.sort();
                (n, score, matched)
            })
            .filter(|(_, score, _)| *score > 0.0)
            .collect();
        keyword.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let matched_of: HashMap<usize, Vec<String>> = keyword
            .iter()
            .map(|(n, _, matched)| (*n, matched.clone()))
            .collect();
        let hit = |n: usize, score: f64| CardHit {
            id: cards[n].id.clone(),
            score,
            matched: matched_of.get(&n).cloned().unwrap_or_default(),
        };
        let Some((model, vector)) = query_vector else {
            return keyword
                .into_iter()
                .take(limit)
                .map(|(n, score, _)| hit(n, score))
                .collect();
        };
        let stored = self.vectors.lock().unwrap_or_else(PoisonError::into_inner);
        let mut by_meaning: Vec<(usize, f32)> = cards
            .iter()
            .enumerate()
            .filter_map(|(n, card)| {
                let own = stored.get(&(model.to_string(), card.text.clone()))?;
                Some((n, cosine(vector, own)))
            })
            .collect();
        drop(stored);
        by_meaning.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let fused = fuse(&[
            keyword.iter().map(|(n, _, _)| *n).collect(),
            by_meaning.iter().map(|(n, _)| *n).collect(),
        ]);
        fused
            .into_iter()
            .take(limit)
            .map(|(n, score)| hit(n, score))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, text: &str) -> ModelCard {
        ModelCard {
            id: id.into(),
            text: text.into(),
        }
    }

    fn cards() -> Vec<ModelCard> {
        vec![
            card(
                "writer",
                "Story Writer 7B. Good at: creative writing, essays and long-form prose. Measured 40 tok/s.",
            ),
            card(
                "coder",
                "Qwen3 Coder 30B. Likely good at: coding, tool calling. Fits now. Measured 55 tok/s.",
            ),
            card(
                "scholar",
                "Hosted, strong tier, 1M context. Good at: research with citations, summarising long documents.",
            ),
            card(
                "eyes",
                "Gemma 3 4B. Reads images: a vision projector is beside it.",
            ),
        ]
    }

    fn ids(hits: &[CardHit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.id.as_str()).collect()
    }

    #[test]
    fn stems_join_word_forms() {
        for word in ["code", "coding", "coder", "codes"] {
            assert_eq!(stem(word), "cod", "{word}");
        }
        for word in ["write", "writing", "writer"] {
            assert_eq!(stem(word), "writ", "{word}");
        }
        assert_eq!(stem("images"), "imag");
        assert_eq!(stem("go"), "go");
    }

    #[test]
    fn task_descriptions_find_the_model_whose_card_fits() {
        let index = CardIndex::new();
        let cards = cards();
        let search = |query: &str| index.search(query, &cards, None, 5);
        let coding = search("Fix the failing Rust test and refactor the parser");
        assert_eq!(ids(&coding)[0], "coder");
        assert!(coding[0].matched.contains(&"cod".to_string()));
        assert_eq!(
            ids(&search("Draft a blog post about our launch"))[0],
            "writer"
        );
        assert_eq!(
            ids(&search("Survey the literature and cite your sources"))[0],
            "scholar"
        );
        assert_eq!(ids(&search("What does this screenshot show?"))[0], "eyes");
        // Nothing in common: no card matches by keyword.
        assert!(search("zzz qqq").is_empty());
        assert_eq!(index.search("coding", &cards, None, 0), Vec::new());
    }

    #[test]
    fn meaning_is_fused_in_when_vectors_exist() {
        let index = CardIndex::new();
        let cards = cards();
        let missing = index.unembedded("emb", &cards);
        assert_eq!(missing.len(), 4);
        // A fake embedding: the writer's card points one way, every other card the other.
        let vectors = missing
            .iter()
            .map(|text| {
                if text.contains("Story") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect();
        index.store("emb", &missing, vectors, &cards);
        assert!(index.unembedded("emb", &cards).is_empty());
        assert_eq!(index.unembedded("other", &cards).len(), 4);
        // No keyword in common, but the meaning points at the writer.
        let hits = index.search(
            "a sonnet for my mother",
            &cards,
            Some(("emb", &[1.0, 0.0])),
            5,
        );
        assert_eq!(ids(&hits)[0], "writer");
        assert_eq!(hits.len(), 4);
        // A changed card is embedded again; the old text's vector is dropped.
        let mut changed = cards.clone();
        changed[0].text.push_str(" Also poems.");
        assert_eq!(index.unembedded("emb", &changed).len(), 1);
        index.store("emb", &[], Vec::new(), &changed);
        assert_eq!(index.unembedded("emb", &cards).len(), 1);
    }
}
