//! Term-based relevance: each query term found in an entry adds its inverse document
//! frequency, weighted by where it appears (name, description, body). Small and fast for the
//! hundreds of entries a memory holds. Words come from the tokenizer session search shares
//! ([`crate::retrieval`]).

use std::collections::HashSet;

use crate::Entry;

fn terms(text: &str) -> HashSet<String> {
    crate::retrieval::words(text, 3).into_iter().collect()
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
