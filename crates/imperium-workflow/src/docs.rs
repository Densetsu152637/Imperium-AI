//! The project's documentation index: README and `docs/` split into heading sections, so
//! agents get pointers to what already documents their task.

use std::collections::BTreeSet;
use std::path::Path;

use crate::Pointer;
use crate::text::{overlap, sections, terms};

/// Folders never indexed.
const SKIP: [&str; 5] = ["node_modules", "target", ".git", "dist", "build"];
/// Files indexed at most, so huge trees stay cheap.
const MAX_FILES: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocChunk {
    /// `project:<path>#<anchor>`.
    pub reference: String,
    pub summary: String,
    pub terms: BTreeSet<String>,
    /// Paths, commands, and identifiers the section quotes in backticks.
    pub mentions: BTreeSet<String>,
}

/// Indexes `README*.md` at the root and every Markdown file under `docs/`.
pub fn index(project_dir: &Path) -> Vec<DocChunk> {
    let mut files: Vec<String> = std::fs::read_dir(project_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (name.to_lowercase().starts_with("readme") && name.ends_with(".md")).then_some(name)
        })
        .collect();
    collect(&project_dir.join("docs"), "docs", &mut files);
    files.sort();
    files.truncate(MAX_FILES);
    let mut chunks = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(project_dir.join(&file)) else {
            continue;
        };
        for section in sections(&text) {
            // The section's own text, up to its first subsection, which is indexed separately.
            let body: Vec<&str> = section
                .text
                .lines()
                .skip(1)
                .take_while(|line| !line.starts_with('#'))
                .collect();
            let summary = body
                .iter()
                .map(|line| line.trim())
                .find(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("```"))
                .unwrap_or_default()
                .chars()
                .take(160)
                .collect();
            chunks.push(DocChunk {
                reference: format!("project:{file}#{}", section.anchor),
                summary,
                terms: terms(&format!("{} {}", section.heading, section.heading)),
                mentions: mentions(&body.join("\n")),
            });
            // Body words count too, once.
            if let Some(chunk) = chunks.last_mut() {
                chunk.terms.extend(terms(&body.join(" ")));
            }
        }
    }
    chunks
}

fn collect(dir: &Path, prefix: &str, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() {
            if !SKIP.contains(&name.as_str()) {
                collect(&path, &format!("{prefix}/{name}"), files);
            }
        } else if name.ends_with(".md") {
            files.push(format!("{prefix}/{name}"));
        }
    }
}

/// Backticked spans that look like paths, files, or config keys.
fn mentions(text: &str) -> BTreeSet<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|span| !span.contains(' ') && (span.contains('/') || span.contains('.')))
        .map(str::to_string)
        .collect()
}

/// The chunks most relevant to a request: those quoting a path or file the request names
/// first, then those sharing at least three words with it.
pub fn pointers(
    chunks: &[DocChunk],
    request: &str,
    wanted: &BTreeSet<String>,
    limit: usize,
) -> Vec<Pointer> {
    let named: Vec<&str> = request
        .split(|c: char| c.is_whitespace() || "`'\"(),".contains(c))
        .filter(|word| word.contains('/') || word.contains('.'))
        .map(|word| word.trim_end_matches(['.', ':', ';']))
        .filter(|word| !word.is_empty())
        .collect();
    let mut scored: Vec<(usize, &DocChunk, String)> = chunks
        .iter()
        .filter_map(|chunk| {
            if let Some(path) = named.iter().find(|path| {
                chunk
                    .mentions
                    .iter()
                    .any(|mention| mention.ends_with(*path) || path.ends_with(mention.as_str()))
            }) {
                return Some((usize::MAX, chunk, format!("mentions {path}")));
            }
            let shared = overlap(wanted, &chunk.terms);
            (shared >= 3).then(|| (shared, chunk, format!("{shared} words in common")))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.reference.cmp(&b.1.reference))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, chunk, reason)| Pointer {
            reference: chunk.reference.clone(),
            reason,
            summary: chunk.summary.clone(),
        })
        .collect()
}
