//! Workflow packs: a folder of Markdown instructions with an entry index (`AGENTS.md`) and an
//! optional `imperium-workflow.toml` manifest.
//!
//! Without a manifest the index is read by convention: each link in it is a candidate whose
//! label (the words before the link plus its text) is its trigger, and links to
//! `AGENTS.<role>.md` are that role's files.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::text::{links, terms};

/// A file or section an agent may be asked to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `<path>` or `<path>#<anchor>`, relative to the pack.
    pub reference: String,
    /// What the index says it is for.
    pub label: String,
    pub terms: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct Pack {
    pub name: String,
    pub root: PathBuf,
    /// The entry index, always loaded.
    pub entry: String,
    /// Files every agent in a role must load.
    pub roles: BTreeMap<String, Vec<String>>,
    pub candidates: Vec<Candidate>,
}

#[derive(Deserialize, Default)]
struct Manifest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    roles: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("{0} has no {1}")]
    NoEntry(String, String),
    #[error("{0}: {1}")]
    Manifest(String, String),
}

impl Pack {
    /// Reads the pack in `root`.
    pub fn load(root: &Path) -> Result<Self, PackError> {
        let manifest_path = root.join("imperium-workflow.toml");
        let manifest: Manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => toml::from_str(&text).map_err(|error| {
                PackError::Manifest(manifest_path.display().to_string(), error.to_string())
            })?,
            Err(_) => Manifest::default(),
        };
        let entry = manifest.entry.unwrap_or_else(|| "AGENTS.md".into());
        let index = std::fs::read_to_string(root.join(&entry))
            .map_err(|_| PackError::NoEntry(root.display().to_string(), entry.clone()))?;
        let name = manifest.name.unwrap_or_else(|| {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "workflow".into())
        });
        let mut roles = manifest.roles;
        let mut candidates: Vec<Candidate> = Vec::new();
        // Index entries are list items; links in prose paragraphs are references, not triggers.
        let items = index.lines().filter(|line| {
            let line = line.trim_start();
            line.starts_with("- ") || line.starts_with("* ")
        });
        for line in items {
            for link in links(line) {
                if link.target.contains("://") || link.target.starts_with('#') {
                    continue;
                }
                let (path, _) = link.target.split_once('#').unwrap_or((&link.target, ""));
                if !root.join(path).is_file() {
                    continue;
                }
                if let Some(role) = role_of(path) {
                    let files = roles.entry(role).or_default();
                    if !files.contains(&path.to_string()) {
                        files.push(path.to_string());
                    }
                }
                let lead = link
                    .lead
                    .trim()
                    .trim_start_matches(['-', '*', ';', ',', '.'])
                    .trim();
                let label = format!("{} {}", lead.trim_end_matches(':'), link.text)
                    .trim()
                    .to_string();
                if candidates.iter().any(|c| c.reference == link.target) {
                    continue;
                }
                candidates.push(Candidate {
                    reference: link.target.clone(),
                    terms: terms(&label),
                    label,
                });
            }
        }
        Ok(Pack {
            name,
            root: root.to_path_buf(),
            entry,
            roles,
            candidates,
        })
    }
}

/// Roles the convention recognises: `workflows/AGENTS.leaf.md` is the `leaf` role's file.
/// Other `AGENTS.<name>.md` files (such as debugging) are ordinary triggered workflows.
pub const ROLES: [&str; 3] = ["root", "manager", "leaf"];

fn role_of(path: &str) -> Option<String> {
    let file = Path::new(path).file_name()?.to_str()?;
    let role = file.strip_prefix("AGENTS.")?.strip_suffix(".md")?;
    ROLES.contains(&role).then(|| role.to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample_pack() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("workflows")).unwrap();
        std::fs::create_dir_all(root.join("guides")).unwrap();
        std::fs::write(
            root.join("AGENTS.md"),
            "# Agent instructions\n\n## Workflows\n\n\
             - Root: [Responsibilities and reporting](workflows/AGENTS.root.md).\n\
             - Bounded delegated execution: [Leaf](workflows/AGENTS.leaf.md).\n\
             - Incorrect behavior or incidents: [Debugging](workflows/AGENTS.debugging.md).\n\
             - Acceptance evidence: [Validation](guides/validation.md#shared-validation-rules).\n\
             - Local edits/Git mutations: [Git usage](guides/git-usage.md); pushes/PRs/merges: [Publication](guides/git-publication.md).\n\
             - Missing: [Gone](guides/missing.md). External: [Site](https://example.com).\n",
        )
        .unwrap();
        for file in ["AGENTS.root.md", "AGENTS.leaf.md", "AGENTS.debugging.md"] {
            std::fs::write(
                root.join("workflows").join(file),
                format!("# {file}\n\nbody\n"),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("guides/validation.md"),
            "# Validation\n\nintro\n\n## Shared validation rules\n\nRun the checks.\n\n## Other\n\nmore\n",
        )
        .unwrap();
        std::fs::write(root.join("guides/git-usage.md"), "# Git usage\n").unwrap();
        std::fs::write(root.join("guides/git-publication.md"), "# Publication\n").unwrap();
        dir
    }

    #[test]
    fn convention_packs_index_links_and_roles() {
        let dir = sample_pack();
        let pack = Pack::load(dir.path()).unwrap();
        assert_eq!(pack.entry, "AGENTS.md");
        assert_eq!(pack.roles["root"], ["workflows/AGENTS.root.md"]);
        assert_eq!(pack.roles["leaf"], ["workflows/AGENTS.leaf.md"]);
        let labels: Vec<_> = pack.candidates.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Root Responsibilities and reporting",
                "Bounded delegated execution Leaf",
                "Incorrect behavior or incidents Debugging",
                "Acceptance evidence Validation",
                "Local edits/Git mutations Git usage",
                "pushes/PRs/merges Publication",
            ]
        );
        assert_eq!(
            pack.candidates[3].reference,
            "guides/validation.md#shared-validation-rules"
        );
    }

    #[test]
    fn manifests_name_the_pack_and_add_roles() {
        let dir = sample_pack();
        std::fs::write(
            dir.path().join("imperium-workflow.toml"),
            "iwc = \"1.0\"\nname = \"acw\"\n[roles]\ntester = [\"guides/validation.md\"]\n",
        )
        .unwrap();
        let pack = Pack::load(dir.path()).unwrap();
        assert_eq!(pack.name, "acw");
        assert_eq!(pack.roles["tester"], ["guides/validation.md"]);
        assert!(Pack::load(&dir.path().join("workflows")).is_err());
    }
}
