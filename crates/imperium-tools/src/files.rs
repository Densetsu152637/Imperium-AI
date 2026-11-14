use std::path::Path;

use crate::{ToolError, Workspace, remote_files};

/// Largest text returned to the model from one read, in characters.
const MAX_READ_CHARS: usize = 100_000;

pub async fn read_file(
    workspace: &Workspace,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, ToolError> {
    let bytes = if workspace.runtime().is_some() {
        remote_files::read_bytes(workspace, path).await?
    } else {
        let full = workspace.resolve(path)?;
        tokio::fs::read(&full)
            .await
            .map_err(|error| ToolError::Io(format!("{path}: {error}")))?
    };
    let text = readable_text(bytes, path)?;
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

/// The text of a file's bytes for reading: UTF-8, or UTF-16 when it starts with a byte order
/// mark (what PowerShell's `>` and many Windows tools write). Editing keeps to UTF-8, so a
/// UTF-16 file is never rewritten in another encoding.
fn readable_text(bytes: Vec<u8>, shown: &str) -> Result<String, ToolError> {
    let utf16 = |rest: &[u8], big_endian: bool| {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| ToolError::Invalid(format!("{shown} is not text")))
    };
    match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        _ => String::from_utf8(bytes)
            .map_err(|_| ToolError::Invalid(format!("{shown} is not UTF-8 text"))),
    }
}

pub async fn write_file(
    workspace: &Workspace,
    path: &str,
    content: &str,
) -> Result<String, ToolError> {
    if workspace.runtime().is_some() {
        let shown = remote_files::write(workspace, path, content.as_bytes()).await?;
        return Ok(format!("wrote {} bytes to {shown}", content.len()));
    }
    let full = workspace.resolve_writable(path)?;
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
    if workspace.runtime().is_some() {
        let text = remote_files::read_text(workspace, path).await?;
        let replaced = replace_once(&text, old, new, path)?;
        let shown = remote_files::write(workspace, path, replaced.as_bytes()).await?;
        return Ok(format!("edited {shown}"));
    }
    let full = workspace.resolve_writable(path)?;
    let text = read_text(&full, path).await?;
    let replaced = replace_once(&text, old, new, path)?;
    tokio::fs::write(&full, replaced)
        .await
        .map_err(|error| ToolError::Io(format!("{path}: {error}")))?;
    Ok(format!("edited {}", workspace.display(&full)))
}

/// `text` with its one occurrence of `old` replaced; none or several is an error.
fn replace_once(text: &str, old: &str, new: &str, path: &str) -> Result<String, ToolError> {
    match text.matches(old).count() {
        0 => Err(ToolError::Invalid(format!(
            "old_string not found in {path}"
        ))),
        1 => Ok(text.replacen(old, new, 1)),
        n => Err(ToolError::Invalid(format!(
            "old_string occurs {n} times in {path}; include more context so it is unique"
        ))),
    }
}

pub async fn list_dir(workspace: &Workspace, path: &str) -> Result<String, ToolError> {
    if workspace.runtime().is_some() {
        return remote_files::list(workspace, path).await;
    }
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
    async fn utf16_files_with_a_byte_order_mark_are_read_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(
            "one\r\ntwo \u{e9}\r\n"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        std::fs::write(dir.path().join("log.txt"), bytes).unwrap();
        let read = read_file(&workspace, "log.txt", None, None).await.unwrap();
        assert_eq!(read, "     1\tone\n     2\ttwo \u{e9}\n");
        std::fs::write(dir.path().join("bin.dat"), [0xFFu8, 0x00, 0x80]).unwrap();
        assert!(matches!(
            read_file(&workspace, "bin.dat", None, None).await,
            Err(ToolError::Invalid(_))
        ));
    }

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
    async fn tools_use_every_folder_of_the_environment_and_nothing_else() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(second.path().join("notes.txt"), "from the second folder\n").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "no").unwrap();
        let workspace = Workspace::new(first.path())
            .unwrap()
            .with_folders([second.path()])
            .unwrap();

        let notes = second
            .path()
            .join("notes.txt")
            .to_string_lossy()
            .to_string();
        let read = read_file(&workspace, &notes, None, None).await.unwrap();
        assert!(read.contains("from the second folder"), "{read}");
        let written = second.path().join("out.txt").to_string_lossy().to_string();
        write_file(&workspace, &written, "made").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(second.path().join("out.txt")).unwrap(),
            "made"
        );

        let secret = outside
            .path()
            .join("secret.txt")
            .to_string_lossy()
            .to_string();
        assert!(matches!(
            read_file(&workspace, &secret, None, None).await,
            Err(ToolError::OutsideWorkspace(_))
        ));
        let planted = outside.path().join("new.txt").to_string_lossy().to_string();
        assert!(matches!(
            write_file(&workspace, &planted, "x").await,
            Err(ToolError::OutsideWorkspace(_))
        ));
        assert!(!outside.path().join("new.txt").exists());
    }

    #[tokio::test]
    async fn read_only_grants_refuse_writes_and_edits() {
        use crate::permissions::{PathGrant, PathMode};
        let project = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let toolchain = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(toolchain.path().join("rustc.txt"), "1.89\n").unwrap();
        std::fs::create_dir(project.path().join("vendor")).unwrap();
        std::fs::write(project.path().join("vendor").join("lib.rs"), "old\n").unwrap();
        let grant = |path: &std::path::Path, mode| PathGrant {
            path: path.to_string_lossy().to_string(),
            mode,
        };
        let workspace = Workspace::new(project.path()).unwrap().with_grants(&[
            grant(cache.path(), PathMode::ReadWrite),
            grant(toolchain.path(), PathMode::Read),
            // A read-only grant inside a folder makes that part read-only.
            grant(&project.path().join("vendor"), PathMode::Read),
            grant(&outside.path().join("missing"), PathMode::Read),
        ]);
        assert_eq!(workspace.grants().len(), 3, "a missing path is skipped");

        let file = |dir: &std::path::Path, name: &str| dir.join(name).to_string_lossy().to_string();
        let read = read_file(&workspace, &file(toolchain.path(), "rustc.txt"), None, None)
            .await
            .unwrap();
        assert!(read.contains("1.89"));
        assert!(
            list_dir(&workspace, &toolchain.path().to_string_lossy())
                .await
                .is_ok()
        );
        assert!(matches!(
            write_file(&workspace, &file(toolchain.path(), "rustc.txt"), "x").await,
            Err(ToolError::ReadOnly(_))
        ));
        assert!(matches!(
            write_file(&workspace, &file(toolchain.path(), "new/file.txt"), "x").await,
            Err(ToolError::ReadOnly(_))
        ));
        assert!(matches!(
            edit_file(
                &workspace,
                &file(toolchain.path(), "rustc.txt"),
                "1.89",
                "2"
            )
            .await,
            Err(ToolError::ReadOnly(_))
        ));
        assert_eq!(
            std::fs::read_to_string(toolchain.path().join("rustc.txt")).unwrap(),
            "1.89\n"
        );
        assert!(matches!(
            edit_file(&workspace, "vendor/lib.rs", "old", "new").await,
            Err(ToolError::ReadOnly(_))
        ));
        assert!(
            read_file(&workspace, "vendor/lib.rs", None, None)
                .await
                .is_ok()
        );
        write_file(&workspace, "src/main.rs", "fn main() {}")
            .await
            .unwrap();

        write_file(&workspace, &file(cache.path(), "registry/index"), "x")
            .await
            .unwrap();
        assert!(cache.path().join("registry").join("index").exists());
        assert!(matches!(
            read_file(&workspace, &file(outside.path(), "x"), None, None).await,
            Err(ToolError::OutsideWorkspace(_))
        ));
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
