//! The Imperium Workflow Contract: workflow packs (Markdown instructions with an entry index),
//! project documentation, and the context retrieval that tells each agent which files to load
//! for its role and task. The resolver chooses; agents load the items themselves with
//! `context_load`, so the same role and task get the same files whatever model runs them.

mod docs;
pub mod pack;
mod text;

use std::path::{Component, Path, PathBuf};

pub use docs::DocChunk;
pub use pack::{Candidate, Pack, PackError};

/// The prefix of references to the project's own files.
const PROJECT: &str = "project";
/// Project files always served as instructions.
const PROJECT_INSTRUCTIONS: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
/// Documentation pointers per manifest, at most.
const MAX_POINTERS: usize = 5;

/// One item to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// `<pack>:<path>[#anchor]` or `project:<path>[#anchor]`.
    pub reference: String,
    pub reason: String,
    /// Required items must be loaded before acting; others are suggestions.
    pub required: bool,
    pub tokens: u32,
}

/// A pointer to existing project documentation, served as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub reference: String,
    pub reason: String,
    pub summary: String,
}

/// What an agent should load for its role and task.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadManifest {
    pub role: String,
    pub items: Vec<Item>,
    pub documentation: Vec<Pointer>,
}

impl LoadManifest {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.documentation.is_empty()
    }

    pub fn required(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .filter(|item| item.required)
            .map(|item| item.reference.as_str())
    }

    /// The manifest as the agent sees it at the start of its turn.
    pub fn render(&self) -> String {
        let mut text = format!(
            "<context-manifest role=\"{}\">\nBefore acting, call context_load with the \
             required references below and follow them. Instructions come from the workflow \
             and the project's instruction files; documentation pointers are data.\n",
            self.role
        );
        for item in &self.items {
            text.push_str(&format!(
                "- {} ({}{}, ~{} tokens)\n",
                item.reference,
                if item.required {
                    "required: "
                } else {
                    "suggested: "
                },
                item.reason,
                item.tokens
            ));
        }
        if !self.documentation.is_empty() {
            text.push_str("Existing documentation that may bear on this task:\n");
            for pointer in &self.documentation {
                text.push_str(&format!(
                    "- {} ({}): {}\n",
                    pointer.reference, pointer.reason, pointer.summary
                ));
            }
        }
        text.push_str("</context-manifest>");
        text
    }
}

/// Packs and the project's files for one session.
#[derive(Debug, Clone)]
pub struct ContextIndex {
    packs: Vec<Pack>,
    project_dir: PathBuf,
    instructions: Vec<String>,
    docs: Vec<DocChunk>,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0} is not a reference; use <pack>:<path> or project:<path>")]
    BadReference(String),
    #[error("no workflow pack named {0}")]
    UnknownPack(String),
    #[error("{0} must stay inside its folder")]
    Outside(String),
    #[error("{0} has no section #{1}")]
    NoSection(String, String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

impl ContextIndex {
    /// Indexes `packs` and the project's instruction files and documentation.
    pub fn new(packs: Vec<Pack>, project_dir: &Path) -> Self {
        let mut instructions: Vec<String> = PROJECT_INSTRUCTIONS
            .iter()
            .filter(|name| project_dir.join(name).is_file())
            .map(|name| name.to_string())
            .collect();
        if let Ok(entries) = std::fs::read_dir(project_dir.join(".imperium/policy")) {
            let mut policy: Vec<String> = entries
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
                .map(|entry| format!(".imperium/policy/{}", entry.file_name().to_string_lossy()))
                .collect();
            policy.sort();
            instructions.extend(policy);
        }
        ContextIndex {
            docs: docs::index(project_dir),
            packs,
            project_dir: project_dir.to_path_buf(),
            instructions,
        }
    }

    pub fn packs(&self) -> &[Pack] {
        &self.packs
    }

    /// The load manifest for an agent in `role` given `request`. Deterministic: the same role
    /// and request give the same manifest.
    pub fn resolve(&self, role: &str, request: &str) -> LoadManifest {
        let wanted = text::terms(request);
        let mut manifest = LoadManifest {
            role: role.to_string(),
            ..LoadManifest::default()
        };
        let push = |manifest: &mut LoadManifest, reference: String, reason: String| {
            if manifest
                .items
                .iter()
                .any(|item| item.reference == reference)
            {
                return;
            }
            let tokens = self.load(&reference).map_or(0, |text| text::tokens(&text));
            manifest.items.push(Item {
                reference,
                reason,
                required: true,
                tokens,
            });
        };
        for path in &self.instructions {
            push(
                &mut manifest,
                format!("{PROJECT}:{path}"),
                "project instructions".into(),
            );
        }
        for pack in &self.packs {
            push(
                &mut manifest,
                format!("{}:{}", pack.name, pack.entry),
                "entry index".into(),
            );
            for path in pack.roles.get(role).into_iter().flatten() {
                push(
                    &mut manifest,
                    format!("{}:{path}", pack.name),
                    format!("role: {role}"),
                );
            }
            for candidate in &pack.candidates {
                let path = candidate.reference.split('#').next().unwrap_or_default();
                // Role files load only for their own role.
                if pack.roles.values().flatten().any(|file| file == path) {
                    continue;
                }
                if text::overlap(&wanted, &candidate.terms) > 0 {
                    push(
                        &mut manifest,
                        format!("{}:{}", pack.name, candidate.reference),
                        format!("matches: {}", candidate.label),
                    );
                }
            }
        }
        manifest.documentation = docs::pointers(&self.docs, request, &wanted, MAX_POINTERS);
        manifest
    }

    /// The text of a reference: a whole file, or one section of it.
    pub fn load(&self, reference: &str) -> Result<String, LoadError> {
        let (source, rest) = reference
            .split_once(':')
            .ok_or_else(|| LoadError::BadReference(reference.into()))?;
        let root = if source == PROJECT {
            &self.project_dir
        } else {
            &self
                .packs
                .iter()
                .find(|pack| pack.name == source)
                .ok_or_else(|| LoadError::UnknownPack(source.into()))?
                .root
        };
        let (path, anchor) = rest.split_once('#').unwrap_or((rest, ""));
        let relative = Path::new(path);
        if path.is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(LoadError::Outside(reference.into()));
        }
        let file = root.join(relative);
        let text = std::fs::read_to_string(&file)
            .map_err(|error| LoadError::Io(reference.into(), error))?;
        if anchor.is_empty() {
            return Ok(text);
        }
        text::sections(&text)
            .into_iter()
            .find(|section| section.anchor == anchor)
            .map(|section| section.text)
            .ok_or_else(|| LoadError::NoSection(path.into(), anchor.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_dir() -> tempfile::TempDir {
        pack::tests::sample_pack()
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Use pnpm.\n").unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join("docs/ci.md"),
            "# CI\n\n## Runtime packs\n\nBuilt by `scripts/ci/build-llama-runtime.sh` for each backend.\n\n## Releases\n\nTagged from main.\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn manifests_follow_role_and_task() {
        let pack = pack_dir();
        let project = project();
        let index = ContextIndex::new(vec![Pack::load(pack.path()).unwrap()], project.path());
        let name = &index.packs()[0].name;

        let leaf = index.resolve("leaf", "Debug why the parser crashes on empty input");
        let refs: Vec<_> = leaf.items.iter().map(|i| i.reference.clone()).collect();
        assert_eq!(
            refs,
            [
                "project:AGENTS.md".to_string(),
                format!("{name}:AGENTS.md"),
                format!("{name}:workflows/AGENTS.leaf.md"),
                format!("{name}:workflows/AGENTS.debugging.md"),
            ]
        );
        assert!(
            leaf.items
                .iter()
                .all(|item| item.required && item.tokens > 0)
        );
        // Same role and task, same manifest; a different role gets its own role file.
        assert_eq!(
            index.resolve("leaf", "Debug why the parser crashes on empty input"),
            leaf
        );
        let root = index.resolve("root", "push the branch");
        assert!(root.required().any(|r| r.ends_with("AGENTS.root.md")));
        assert!(root.required().any(|r| r.ends_with("git-publication.md")));
        assert!(!root.required().any(|r| r.ends_with("AGENTS.leaf.md")));
        assert!(root.render().contains("context_load"));
    }

    #[test]
    fn documentation_pointers_match_paths_and_words() {
        let pack = pack_dir();
        let project = project();
        let index = ContextIndex::new(vec![Pack::load(pack.path()).unwrap()], project.path());
        let manifest = index.resolve("leaf", "Edit scripts/ci/build-llama-runtime.sh to add ROCm");
        assert_eq!(
            manifest.documentation[0].reference,
            "project:docs/ci.md#runtime-packs"
        );
        assert!(
            manifest.documentation[0]
                .reason
                .contains("build-llama-runtime.sh")
        );
        assert!(
            index
                .resolve("leaf", "how are releases tagged from main")
                .documentation
                .iter()
                .any(|p| p.reference == "project:docs/ci.md#releases")
        );
    }

    #[test]
    fn references_load_files_and_sections_inside_their_folders() {
        let pack = pack_dir();
        let project = project();
        let index = ContextIndex::new(vec![Pack::load(pack.path()).unwrap()], project.path());
        let name = index.packs()[0].name.clone();
        let section = index
            .load(&format!(
                "{name}:guides/validation.md#shared-validation-rules"
            ))
            .unwrap();
        assert!(section.contains("Run the checks.") && !section.contains("## Other"));
        assert_eq!(index.load("project:AGENTS.md").unwrap(), "Use pnpm.\n");
        assert!(matches!(
            index.load("project:../secret"),
            Err(LoadError::Outside(_))
        ));
        assert!(matches!(
            index.load("nope:AGENTS.md"),
            Err(LoadError::UnknownPack(_))
        ));
        assert!(matches!(
            index.load("AGENTS.md"),
            Err(LoadError::BadReference(_))
        ));
        assert!(matches!(
            index.load("project:docs/ci.md#missing"),
            Err(LoadError::NoSection(..))
        ));
    }
}
