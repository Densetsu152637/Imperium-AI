use std::path::Path;

use crate::{ToolError, Workspace};

/// Largest text returned to the model from one read, in characters.
const MAX_READ_CHARS: usize = 100_000;

pub async fn read_file(
    workspace: &Workspace,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, ToolError> {
    let full = workspace.resolve(path)?;
    let text = read_text(&full, path).await?;
    let start = offset.unwrap_or(1).max(1) - 1;
    let lines: Vec<&str> = text
        .lines()
        .skip(start)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        out.push_str(&format!("{:>6}\t{line}\n", start + index + 1));
        if out.len() > MAX_READ_CHARS {
            out.push_str("[truncated: read a smaller range with offset and limit]\n");
            break;
        }
    }
    if out.is_empty() {
        out.push_str("[no lines in that range]");
    }
    Ok(out)
}

pub async fn write_file(
    workspace: &Workspace,
    path: &str,
    content: &str,
) -> Result<String, ToolError> {
    let full = workspace.resolve(path)?;
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| ToolError::Io(format!("{path}: {error}")))?;
    }
    tokio::fs::write(&full, content)
        .await
        .map_err(|error| ToolError::Io(format!("{path}: {error}")))?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        workspace.display(&full)
    ))
}

pub async fn edit_file(
    workspace: &Workspace,
    path: &str,
    old: &str,
    new: &str,
) -> Result<String, ToolError> {
    if old.is_empty() {
        return Err(ToolError::Invalid("old_string must not be empty".into()));
    }
    let full = workspace.resolve(path)?;
    let text = read_text(&full, path).await?;
    match text.matches(old).count() {
        0 => Err(ToolError::Invalid(format!(
            "old_string not found in {path}"
        ))),
        1 => {
            tokio::fs::write(&full, text.replacen(old, new, 1))
                .await
                .map_err(|error| ToolError::Io(format!("{path}: {error}")))?;
            Ok(format!("edited {}", workspace.display(&full)))
        }
        n => Err(ToolError::Invalid(format!(
            "old_string occurs {n} times in {path}; include more context so it is unique"
        ))),
    }
}

pub async fn list_dir(workspace: &Workspace, path: &str) -> Result<String, ToolError> {
    let full = workspace.resolve(path)?;
    let mut reader = tokio::fs::read_dir(&full)
        .await
        .map_err(|error| ToolError::Io(format!("{path}: {error}")))?;
    let mut entries = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|error| ToolError::Io(format!("{path}: {error}")))?
    {
        let is_dir = entry.file_type().await.is_ok_and(|kind| kind.is_dir());
        let name = entry.file_name().to_string_lossy().to_string();
        entries.push(if is_dir { format!("{name}/") } else { name });
    }
    entries.sort();
    if entries.is_empty() {
        return Ok(format!("{} is empty", workspace.display(&full)));
    }
    Ok(entries.join("\n"))
}

async fn read_text(full: &Path, shown: &str) -> Result<String, ToolError> {
    let bytes = tokio::fs::read(full)
        .await
        .map_err(|error| ToolError::Io(format!("{shown}: {error}")))?;
    String::from_utf8(bytes).map_err(|_| ToolError::Invalid(format!("{shown} is not UTF-8 text")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_read_edit_and_list_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();

        write_file(&workspace, "src/lib.rs", "fn a() {}\nfn b() {}\n")
            .await
            .unwrap();
        let read = read_file(&workspace, "src/lib.rs", Some(2), Some(1))
            .await
            .unwrap();
        assert_eq!(read, "     2\tfn b() {}\n");

        edit_file(&workspace, "src/lib.rs", "fn b", "fn c")
            .await
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert_eq!(text, "fn a() {}\nfn c() {}\n");

        assert_eq!(list_dir(&workspace, ".").await.unwrap(), "src/");
        assert_eq!(list_dir(&workspace, "src").await.unwrap(), "lib.rs");
    }

    #[tokio::test]
    async fn edits_must_match_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        write_file(&workspace, "a.txt", "x x").await.unwrap();
        assert!(matches!(
            edit_file(&workspace, "a.txt", "x", "y").await,
            Err(ToolError::Invalid(message)) if message.contains("2 times")
        ));
        assert!(matches!(
            edit_file(&workspace, "a.txt", "z", "y").await,
            Err(ToolError::Invalid(_))
        ));
    }
}
