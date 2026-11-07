//! Term-based relevance: each query term found in an entry adds its inverse document
//! frequency, weighted by where it appears (name, description, body). Small and fast for the
//! hundreds of entries a memory holds; embeddings can be layered on later.

use std::collections::HashSet;

use crate::Entry;

const STOPWORDS: [&str; 24] = [
    "the", "and", "for", "with", "that", "this", "are", "was", "but", "not", "you", "your", "use",
    "how", "what", "which", "when", "where", "who", "why", "can", "should", "from", "into",
];

fn terms(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.len() >= 3 && !STOPWORDS.contains(&word.as_str()))
        .collect()
}

/// A relevance score per entry (0 when nothing matches), in the same order as `entries`.
pub fn score(entries: &[Entry], query: &str) -> Vec<f64> {
    let query = terms(query);
    let fields: Vec<[HashSet<String>; 3]> = entries
        .iter()
        .map(|entry| {
            [
                terms(&entry.name.replace('-', " ")),
                terms(&entry.description),
                terms(&entry.body),
            ]
        })
        .collect();
    let count = entries.len() as f64;
    query
        .iter()
        .fold(vec![0.0; entries.len()], |mut scores, term| {
            let containing = fields
                .iter()
                .filter(|f| f.iter().any(|field| field.contains(term)))
                .count();
            if containing == 0 {
                return scores;
            }
            let idf = (1.0 + count / containing as f64).ln();
            for (score, [name, description, body]) in scores.iter_mut().zip(&fields) {
                let weight = if name.contains(term) {
                    3.0
                } else if description.contains(term) {
                    2.0
                } else if body.contains(term) {
                    1.0
                } else {
                    0.0
                };
                *score += weight * idf;
            }
            scores
        })
}
