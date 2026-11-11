//! Instruction files met while exploring. When an agent's file tools first touch a directory
//! inside an allowed folder, the `AGENTS.md` and `CLAUDE.md` files in that directory and its
//! parents (up to the folder's root) that the conversation does not hold yet are put into it,
//! outermost first so the nearest comes last and wins where they differ.
//!
//! They arrive as a user message wrapped in `<workflow-instructions>` after the tool results of
//! the round that touched the directory. The message is the record: which files an agent holds
//! is read back from the conversation, so a resumed or compacted conversation needs nothing
//! else. Compaction keeps these blocks verbatim (see [`preserved`]) instead of summarising them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use imperium_models::ChatMessage;
use imperium_tools::{Invocation, Workspace};
use imperium_workflow::ContextIndex;
use imperium_workflow::imports::{self, Policy};

/// Opens the message holding instruction files; also how a restored conversation is recognised.
pub(crate) const OPEN: &str = "<workflow-instructions>";
pub(crate) const CLOSE: &str = "</workflow-instructions>";
const FILE_OPEN: &str = "<instruction-file path=\"";
const FILE_CLOSE: &str = "</instruction-file>";
const IMPORT_OPEN: &str = "<imported-file path=\"";
/// A conversation holds this much instruction text at most; further files are listed, not read.
pub(crate) const MAX_TOTAL_BYTES: usize = 96 * 1024;
/// What the journal's `context_loaded` references for these files start with.
pub(crate) const REFERENCE_PREFIX: &str = "instructions:";

const INTRO: &str = "Instruction files for directories you opened, outermost first. The nearest \
                     file comes last and takes precedence where they differ.";

/// One instruction file, rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Loaded {
    pub path: String,
    pub block: String,
}

/// The instruction files an agent's conversation holds, by path as written in their blocks.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    loaded: HashSet<String>,
    bytes: usize,
}

impl Tracker {
    pub(crate) fn from_messages(messages: &[ChatMessage]) -> Self {
        let mut tracker = Tracker::default();
        for message in messages {
            let ChatMessage::User { content, .. } = message else {
                continue;
            };
            if !content.contains(OPEN) {
                continue;
            }
            for prefix in [FILE_OPEN, IMPORT_OPEN] {
                for path in attribute_values(content, prefix) {
                    tracker.loaded.insert(path);
                }
            }
            tracker.bytes += blocks_in(content)
                .iter()
                .map(|(_, block)| block.len())
                .sum::<usize>();
        }
        tracker
    }
}

/// The values of the `path` attribute after each `prefix` in `content`.
fn attribute_values(content: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(prefix) {
        let after = &rest[start + prefix.len()..];
        let Some(end) = after.find('"') else { break };
        found.push(after[..end].to_string());
        rest = &after[end..];
    }
    found
}

fn escape(text: &str) -> String {
    text.replace(FILE_CLOSE, "<\\/instruction-file>")
        .replace(CLOSE, "<\\/workflow-instructions>")
}

/// A file as the conversation holds it.
pub(crate) fn render_block(path: &str, text: &str, truncated: bool) -> String {
    format!(
        "{FILE_OPEN}{path}\"{}>\n{}\n{FILE_CLOSE}",
        if truncated { " truncated=\"true\"" } else { "" },
        escape(text)
    )
}

/// The message content for `blocks`.
pub(crate) fn wrap(blocks: &[String]) -> String {
    format!("{OPEN}\n{INTRO}\n\n{}\n{CLOSE}", blocks.join("\n\n"))
}

/// Whether `message` is one this module added.
pub(crate) fn is_message(message: &ChatMessage) -> bool {
    matches!(message, ChatMessage::User { content, .. } if content.starts_with(OPEN))
}

/// The instruction blocks (path and whole block) inside the wrappers in `content`.
pub(crate) fn blocks_in(content: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(FILE_OPEN) {
        let after = &rest[start..];
        let Some(end) = after.find(FILE_CLOSE) else {
            break;
        };
        let end = end + FILE_CLOSE.len();
        let block = &after[..end];
        let path = attribute_values(block, FILE_OPEN)
            .into_iter()
            .next()
            .unwrap_or_default();
        found.push((path, block.to_string()));
        rest = &after[end..];
    }
    found
}

/// `content` without its instruction wrappers.
pub(crate) fn strip(content: &str) -> String {
    let mut out = String::new();
    let mut rest = content;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        match after.find(CLOSE) {
            Some(end) => rest = after[end + CLOSE.len()..].trim_start_matches('\n'),
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// The blocks in `messages`, in order and once per path, for compaction to keep.
pub(crate) fn preserved(messages: &[ChatMessage]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut blocks = Vec::new();
    for message in messages {
        let ChatMessage::User { content, .. } = message else {
            continue;
        };
        if !content.contains(OPEN) {
            continue;
        }
        for (path, block) in blocks_in(content) {
            if seen.insert(path) {
                blocks.push(block);
            }
        }
    }
    blocks
}

/// The directory a file tool call works in, when it is one.
pub(crate) fn touched_dir(workspace: &Workspace, name: &str, arguments: &str) -> Option<PathBuf> {
    if workspace.runtime().is_some() {
        return None;
    }
    let (path, is_dir) = match Invocation::parse(name, arguments).ok()? {
        Invocation::ListDir { path } => (path, true),
        Invocation::ReadFile { path, .. }
        | Invocation::WriteFile { path, .. }
        | Invocation::EditFile { path, .. } => (path, false),
        Invocation::RunCommand { .. } => return None,
    };
    let resolved = workspace.resolve(&path).ok()?;
    let dir = if is_dir {
        resolved
    } else {
        resolved.parent()?.to_path_buf()
    };
    Some(nearest_existing(&dir))
}

/// `path`, or its nearest ancestor that exists, canonical.
fn nearest_existing(path: &Path) -> PathBuf {
    path.ancestors()
        .find_map(|candidate| std::fs::canonicalize(candidate).ok())
        .unwrap_or_else(|| path.to_path_buf())
}

/// The instruction files the conversation does not hold for the directories in `touched`.
/// The project folder's own files are left out: the workflow manifest serves those.
pub(crate) fn discover(
    workspace: &Workspace,
    context: &ContextIndex,
    touched: &[PathBuf],
    tracker: &mut Tracker,
) -> Vec<Loaded> {
    let mut roots: Vec<PathBuf> = workspace
        .roots()
        .into_iter()
        .map(Path::to_path_buf)
        .chain(workspace.project_worktrees())
        .chain(
            workspace
                .grants()
                .iter()
                .filter(|grant| grant.path.is_dir())
                .map(|grant| grant.path.clone()),
        )
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect();
    roots.dedup();
    // The manifest serves the project's files; a session in a worktree works in its root.
    let project = std::fs::canonicalize(context.project_dir()).ok();
    let served_root = std::fs::canonicalize(workspace.root()).ok();
    let mut out = Vec::new();
    for dir in touched {
        let Some(root) = roots
            .iter()
            .filter(|root| dir.starts_with(root))
            .max_by_key(|root| root.components().count())
        else {
            continue;
        };
        for file in imports::instruction_files(dir, root) {
            if file.parent() == project.as_deref() {
                continue;
            }
            // A worktree's root files are skipped only when they are the project's own, or
            // the same bytes: otherwise the manifest does not serve what they say.
            if file.parent() == served_root.as_deref()
                && let (Some(project), Some(name)) = (project.as_deref(), file.file_name())
            {
                let counterpart = project.join(name);
                if std::fs::canonicalize(&counterpart).ok() == std::fs::canonicalize(&file).ok()
                    || matches!(
                        (std::fs::read(&counterpart), std::fs::read(&file)),
                        (Ok(theirs), Ok(ours)) if theirs == ours
                    )
                {
                    continue;
                }
            }
            let path = imports::display_path(&file);
            if tracker.loaded.contains(&path) {
                continue;
            }
            tracker.loaded.insert(path.clone());
            let Ok(expanded) = imports::expand(
                &file,
                &Policy {
                    allowed: roots.clone(),
                    trusted: false,
                    skip: tracker.loaded.iter().map(PathBuf::from).collect(),
                    ..Policy::default()
                },
            ) else {
                continue;
            };
            if tracker.bytes + expanded.text.len() > MAX_TOTAL_BYTES {
                let note = format!(
                    "[not loaded: this conversation already holds {} KiB of instruction files]",
                    tracker.bytes / 1024
                );
                out.push(Loaded {
                    block: render_block(&path, &note, true),
                    path,
                });
                continue;
            }
            tracker.bytes += expanded.text.len();
            tracker.loaded.extend(
                expanded
                    .files
                    .iter()
                    .map(|file| imports::display_path(file)),
            );
            out.push(Loaded {
                block: render_block(&path, &expanded.text, expanded.truncated),
                path,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_round_trip_through_a_message() {
        let blocks = vec![
            render_block("/p/AGENTS.md", "one </instruction-file> tricky", false),
            render_block("/p/a/CLAUDE.md", "two", true),
        ];
        let content = wrap(&blocks);
        let found = blocks_in(&content);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "/p/AGENTS.md");
        assert_eq!(found[1].0, "/p/a/CLAUDE.md");
        assert!(found[1].1.contains("truncated=\"true\""));
        assert!(is_message(&ChatMessage::user(&content)));
        assert_eq!(strip(&format!("{content}\n\nhello")), "hello");
    }

    #[test]
    fn a_restored_conversation_counts_them_as_workflow_context() {
        use crate::breakdown::{Category, restored_spans};
        let content = format!(
            "{}\n\n{}\n\nquestion",
            wrap(&[render_block("/p/AGENTS.md", "a", false)]),
            wrap(&[render_block("/p/b/AGENTS.md", "b", false)]),
        );
        let spans = restored_spans(&content);
        let categories: Vec<Category> = spans.iter().map(|(category, _)| *category).collect();
        assert_eq!(
            categories,
            [Category::Manifest, Category::Manifest, Category::User]
        );
        assert_eq!(&content[spans[2].1.clone()], "question");
    }

    #[test]
    fn the_tracker_reads_paths_back_from_the_conversation() {
        let messages = vec![
            ChatMessage::user("hi"),
            ChatMessage::user(wrap(&[render_block("/p/a/AGENTS.md", "x", false)])),
        ];
        let tracker = Tracker::from_messages(&messages);
        assert!(tracker.loaded.contains("/p/a/AGENTS.md"));
        assert_eq!(preserved(&messages).len(), 1);
    }
}
