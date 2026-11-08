//! Slash commands: Markdown prompt templates (`commands/<name>.md`). A message starting with
//! `/<name>` is replaced by the template, with `$ARGUMENTS` (everything after the name) and
//! `$1`, `$2`, ... (whitespace-separated arguments) filled in.

use std::path::{Path, PathBuf};

use imperium_api::markdown::front_matter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub origin: String,
}

/// Finds `*.md` files in each `(folder, origin)`; names from subfolders are joined with `:`
/// (`commands/git/commit.md` is `/git:commit`). Plugin commands are also reachable as
/// `/<plugin>:<name>`. Earlier folders win.
pub fn discover(folders: &[(PathBuf, String, Option<String>)]) -> Vec<SlashCommand> {
    let mut commands: Vec<SlashCommand> = Vec::new();
    for (folder, origin, namespace) in folders {
        let mut found = Vec::new();
        collect(folder, "", &mut found);
        found.sort();
        for (name, path) in found {
            let description = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| front_matter(&text).0.get("description").cloned())
                .unwrap_or_default();
            let mut names = vec![name.clone()];
            if let Some(namespace) = namespace {
                names.push(format!("{namespace}:{name}"));
            }
            for name in names {
                if !commands.iter().any(|command| command.name == name) {
                    commands.push(SlashCommand {
                        name,
                        description: description.clone(),
                        path: path.clone(),
                        origin: origin.clone(),
                    });
                }
            }
        }
    }
    commands
}

fn collect(dir: &Path, prefix: &str, found: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            collect(&path, &format!("{prefix}{stem}:"), found);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            found.push((format!("{prefix}{stem}"), path));
        }
    }
}

/// Expands `message` when it invokes a known command; `None` otherwise (the message is sent
/// as written).
pub fn expand(commands: &[SlashCommand], message: &str) -> Option<Result<String, String>> {
    let rest = message.trim_start().strip_prefix('/')?;
    let (name, arguments) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(name, arguments)| (name, arguments.trim()));
    let command = commands.iter().find(|command| command.name == name)?;
    Some(
        std::fs::read_to_string(&command.path)
            .map_err(|error| format!("/{name}: {error}"))
            .map(|text| fill(front_matter(&text).1, arguments)),
    )
}

fn fill(template: &str, arguments: &str) -> String {
    let words: Vec<&str> = arguments.split_whitespace().collect();
    let mut out = template.replace("$ARGUMENTS", arguments);
    // Highest first, so $1 does not eat the start of $10.
    for index in (1..=words.len().max(9)).rev() {
        out = out.replace(
            &format!("${index}"),
            words.get(index - 1).copied().unwrap_or(""),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_expand_with_arguments_and_namespaces() {
        let user = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(user.path().join("git")).unwrap();
        std::fs::write(
            user.path().join("git/commit.md"),
            "---\ndescription: Commit staged work\n---\nCommit with message: $ARGUMENTS",
        )
        .unwrap();
        std::fs::write(
            plugin.path().join("review.md"),
            "Review PR $1 on branch $2.",
        )
        .unwrap();
        let commands = discover(&[
            (user.path().to_path_buf(), "user".into(), None),
            (
                plugin.path().to_path_buf(),
                "plugin:pr".into(),
                Some("pr".into()),
            ),
        ]);
        let names: Vec<_> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["git:commit", "review", "pr:review"]);
        assert_eq!(commands[0].description, "Commit staged work");
        assert_eq!(
            expand(&commands, "/git:commit fix the bug")
                .unwrap()
                .unwrap(),
            "Commit with message: fix the bug"
        );
        assert_eq!(
            expand(&commands, "/pr:review 42 main").unwrap().unwrap(),
            "Review PR 42 on branch main."
        );
        assert_eq!(expand(&commands, "/unknown x"), None);
        assert_eq!(expand(&commands, "no slash"), None);
    }
}
