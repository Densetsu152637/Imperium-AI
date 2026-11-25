use std::path::Path;

use crate::{ToolError, Workspace, remote_files};

/// Largest text returned to the model from one read, in characters (the line numbers and
/// notes count), and the budget of a read that gives a `limit`.
const MAX_READ_CHARS: usize = 100_000;

/// Lines one read returns when the call gives no `limit`, so a small model's context is not
/// filled by a large file; a note says how to read on.
const DEFAULT_READ_LINES: usize = 2000;

/// Character budget of a read with no `limit` and no known context window.
const DEFAULT_READ_BUDGET: usize = 32_000;

/// The context window a hosted model is assumed to have when the model table does not give
/// one: hosted models read far more than the 32k-character default allows, so a read with no
/// `limit` gets the full [`MAX_READ_CHARS`] budget (an eighth of this window, 4 characters a
/// token). A local model with an unknown window keeps the default.
pub const HOSTED_UNKNOWN_WINDOW_TOKENS: u64 = 200_000;

/// Smallest character budget a window can give a read with no `limit`.
const MIN_READ_BUDGET: usize = 8_000;

/// Entries one `list_dir` call returns; a longer folder is paged with `offset`.
const MAX_LIST_ENTRIES: usize = 500;

/// What one read may return, in characters: with an explicit `limit` the ceiling; without
/// one, about an eighth of the agent's context window (4 characters a token, at least
/// [`MIN_READ_BUDGET`]), or [`DEFAULT_READ_BUDGET`] when the window is not known.
fn read_budget(limit: Option<usize>, window_tokens: Option<u64>) -> usize {
    if limit.is_some() {
        return MAX_READ_CHARS;
    }
    match window_tokens {
        Some(window) => usize::try_from(window / 8 * 4)
            .unwrap_or(usize::MAX)
            .clamp(MIN_READ_BUDGET, MAX_READ_CHARS),
        None => DEFAULT_READ_BUDGET,
    }
}

/// [`read_file_in`] for a caller that does not know the agent's context window.
#[cfg(test)]
pub(crate) async fn read_file(
    workspace: &Workspace,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, ToolError> {
    read_file_in(workspace, path, offset, limit, None).await
}

/// Reads a text file as numbered lines. `window_tokens` is the calling agent's context window,
/// when known; it sizes the character budget of a read that gives no `limit`.
pub(crate) async fn read_file_in(
    workspace: &Workspace,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    window_tokens: Option<u64>,
) -> Result<String, ToolError> {
    let bytes = if workspace.runtime().is_some() {
        remote_files::read_bytes(workspace, path).await?
    } else {
        let full = workspace.resolve(path)?;
        if tokio::fs::metadata(&full)
            .await
            .is_ok_and(|meta| meta.is_dir())
        {
            return Err(ToolError::Invalid(format!(
                "{path} is a directory; use list_dir to see what is in it"
            )));
        }
        tokio::fs::read(&full)
            .await
            .map_err(|error| ToolError::Io(format!("{path}: {error}")))?
    };
    let (text, note) = readable_text(bytes, path)?;
    let budget = read_budget(limit, window_tokens);
    let start = offset.unwrap_or(1).max(1) - 1;
    let total = text.lines().count();
    let lines = text
        .lines()
        .skip(start)
        .take(limit.unwrap_or(DEFAULT_READ_LINES))
        .enumerate();
    let mut out = String::new();
    if let Some(note) = note {
        out.push_str(&format!("[note: {note}]\n"));
    }
    let mut chars = out.chars().count();
    // The last whole line shown, and why the read stopped early, if the budget did it.
    let mut last = start;
    let mut stopped_by_budget = false;
    let mut oversize = false;
    for (index, line) in lines {
        let number = start + index + 1;
        let numbered = format!("{number:>6}\t{line}\n");
        let length = numbered.chars().count();
        if chars + length > budget {
            if last == start {
                // The first line alone is past the budget (a minified file): its start is shown.
                out.extend(numbered.chars().take(budget.saturating_sub(chars)));
                out.push('\n');
                oversize = true;
            }
            stopped_by_budget = true;
            break;
        }
        out.push_str(&numbered);
        chars += length;
        last = number;
    }
    if oversize {
        out.push_str(&format!(
            "[truncated: line {} is longer than the {budget}-character read budget; use grep to find the part you need]\n",
            start + 1
        ));
    } else if last == start {
        out.push_str(&format!("[no lines in that range; the file has {total}]"));
    } else if last < total {
        let why = if stopped_by_budget {
            format!("; stopped at the {budget}-character read budget")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "[lines {}-{last} of {total}{why}; use offset={} to read more]\n",
            start + 1,
            last + 1
        ));
    }
    Ok(out)
}

/// How much of a valid-UTF-8 file is scanned for NUL bytes before it is called binary.
const NUL_SCAN_BYTES: usize = 8 * 1024;

/// The text of a file's bytes for reading: UTF-8, or UTF-16 when it starts with a byte order
/// mark (what PowerShell's `>` and many Windows tools write). Editing keeps to UTF-8, so a
/// UTF-16 file is never rewritten in another encoding.
///
/// A file that is not valid in its encoding is still read when it is text (no NUL bytes in a
/// UTF-8 file): the bad bytes show as U+FFFD and the second value says so for a note to the
/// model. A file with NUL bytes and no byte order mark is binary and is refused.
fn readable_text(bytes: Vec<u8>, shown: &str) -> Result<(String, Option<&'static str>), ToolError> {
    let utf16 = |rest: &[u8], big_endian: bool| {
        let (pairs, dropped) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs
            .iter()
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes(*pair)
                } else {
                    u16::from_le_bytes(*pair)
                }
            })
            .collect();
        match String::from_utf16(&units) {
            Ok(text) if dropped.is_empty() => (text, None),
            Ok(text) => (
                text,
                Some("the file ends in a lone byte, which is left out"),
            ),
            Err(_) => (
                String::from_utf16_lossy(&units),
                Some("some of the file is not valid UTF-16; it shows as U+FFFD"),
            ),
        }
    };
    Ok(match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
        _ => match String::from_utf8(bytes) {
            // Valid UTF-8 can still be binary (BOM-less UTF-16 of ASCII text is valid UTF-8 full
            // of NULs), so the head is checked for NUL bytes too.
            Ok(text) if text.as_bytes()[..text.len().min(NUL_SCAN_BYTES)].contains(&0) => {
                return Err(ToolError::Invalid(format!("{shown} is a binary file")));
            }
            Ok(text) => (text, None),
            Err(error) if error.as_bytes().contains(&0) => {
                return Err(ToolError::Invalid(format!("{shown} is a binary file")));
            }
            Err(error) => (
                String::from_utf8_lossy(error.as_bytes()).into_owned(),
                Some(
                    "the file is not valid UTF-8; invalid bytes show as U+FFFD. Do not rewrite the file from this text (edit_file will refuse it)",
                ),
            ),
        },
    })
}

pub(crate) async fn write_file(
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

pub(crate) async fn edit_file(
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

/// A sorted folder listing as one page of at most [`MAX_LIST_ENTRIES`] entries from `offset`
/// (0-based), with a note on how to continue when there are more.
pub(crate) fn page_listing(shown: &str, entries: &[String], offset: usize) -> String {
    if entries.is_empty() {
        return format!("{shown} is empty");
    }
    let total = entries.len();
    let page: Vec<&str> = entries
        .iter()
        .skip(offset)
        .take(MAX_LIST_ENTRIES)
        .map(String::as_str)
        .collect();
    if page.is_empty() {
        return format!("[no entries past offset {offset} ({total} in all)]");
    }
    let last = offset + page.len();
    let mut out = page.join("\n");
    if last < total {
        out.push_str(&format!(
            "\n[entries {}-{last} of {total}; use offset={last} to see more]",
            offset + 1
        ));
    }
    out
}

pub(crate) async fn list_dir(
    workspace: &Workspace,
    path: &str,
    offset: Option<usize>,
) -> Result<String, ToolError> {
    let offset = offset.unwrap_or(0);
    if workspace.runtime().is_some() {
        return remote_files::list(workspace, path, offset).await;
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
    Ok(page_listing(&workspace.display(&full), &entries, offset))
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
        // BOM-less UTF-16 is valid UTF-8 full of NULs: binary, not garbage.
        let bomless: Vec<u8> = "hello\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        std::fs::write(dir.path().join("nobom.txt"), bomless).unwrap();
        assert!(matches!(
            read_file(&workspace, "nobom.txt", None, None).await,
            Err(ToolError::Invalid(_))
        ));
        std::fs::write(dir.path().join("bin.dat"), [0xFFu8, 0x00, 0x80]).unwrap();
        assert!(matches!(
            read_file(&workspace, "bin.dat", None, None).await,
            Err(ToolError::Invalid(_))
        ));

        // Big endian, and an odd byte count that is said to be dropped.
        let mut big = vec![0xFE, 0xFF];
        big.extend("hi\n".encode_utf16().flat_map(u16::to_be_bytes));
        std::fs::write(dir.path().join("be.txt"), &big).unwrap();
        let read = read_file(&workspace, "be.txt", None, None).await.unwrap();
        assert_eq!(read, "     1\thi\n");
        big.push(0x41);
        std::fs::write(dir.path().join("odd.txt"), &big).unwrap();
        let read = read_file(&workspace, "odd.txt", None, None).await.unwrap();
        assert!(
            read.starts_with("[note: the file ends in a lone byte"),
            "{read}"
        );
        assert!(read.contains("     1\thi\n"), "{read}");
    }

    #[tokio::test]
    async fn a_latin1_file_reads_lossily_with_a_note_and_a_folder_points_to_list_dir() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("old.txt"), b"caf\xE9\n").unwrap();
        let read = read_file(&workspace, "old.txt", None, None).await.unwrap();
        assert!(
            read.starts_with("[note: the file is not valid UTF-8"),
            "{read}"
        );
        assert!(read.contains("Do not rewrite the file"), "{read}");
        assert!(read.contains("caf\u{fffd}"), "{read}");

        std::fs::create_dir(dir.path().join("src")).unwrap();
        let Err(ToolError::Invalid(message)) = read_file(&workspace, "src", None, None).await
        else {
            panic!("a directory is not a file");
        };
        assert!(
            message.contains("is a directory; use list_dir"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_long_line_or_a_long_file_is_cut_at_the_character_budget() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        // One minified line far past the budget: its start is shown, cut by characters.
        std::fs::write(dir.path().join("min.js"), "\u{e9}".repeat(300_000)).unwrap();
        let read = read_file(&workspace, "min.js", None, None).await.unwrap();
        assert!(read.starts_with("     1\t\u{e9}"));
        assert!(read.contains("[truncated:"));
        assert!(read.chars().count() < MAX_READ_CHARS + 300);
        // Many long lines stop at the budget too (a limit above the default line cap).
        let lines = "y".repeat(99) + "\n";
        std::fs::write(dir.path().join("many.txt"), lines.repeat(2000)).unwrap();
        let read = read_file(&workspace, "many.txt", None, Some(2000))
            .await
            .unwrap();
        assert!(read.contains("[lines 1-"), "{read}");
        assert!(
            read.contains("-character read budget; use offset="),
            "{read}"
        );
        assert!(read.chars().count() < MAX_READ_CHARS + 200);
        let small = read_file(&workspace, "many.txt", Some(1), Some(2))
            .await
            .unwrap();
        assert!(!small.contains("[truncated:"));
    }

    #[test]
    fn a_hosted_model_with_an_unknown_window_reads_the_full_budget() {
        // Unknown window (a local model): the small default.
        assert_eq!(read_budget(None, None), DEFAULT_READ_BUDGET);
        // Unknown window on a hosted model: the engine passes the assumed hosted window.
        assert_eq!(
            read_budget(None, Some(HOSTED_UNKNOWN_WINDOW_TOKENS)),
            MAX_READ_CHARS
        );
    }

    #[tokio::test]
    async fn a_read_with_the_hosted_window_is_not_cut_at_the_local_default() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let lines = "z".repeat(99)
            + "
";
        std::fs::write(dir.path().join("big.txt"), lines.repeat(900)).unwrap();
        let local = read_file_in(&workspace, "big.txt", None, None, None)
            .await
            .unwrap();
        assert!(local.contains("32000-character read budget"), "{local}");
        let hosted = read_file_in(
            &workspace,
            "big.txt",
            None,
            None,
            Some(HOSTED_UNKNOWN_WINDOW_TOKENS),
        )
        .await
        .unwrap();
        assert!(!hosted.contains("read budget"), "{hosted}");
        assert!(hosted.contains("   900	"), "{hosted}");
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

        assert_eq!(list_dir(&workspace, ".", None).await.unwrap(), "src/");
        assert_eq!(list_dir(&workspace, "src", None).await.unwrap(), "lib.rs");
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
        use imperium_api::{PathGrant, PathMode};
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
            list_dir(&workspace, &toolchain.path().to_string_lossy(), None)
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

    fn numbered(count: usize) -> String {
        (1..=count).map(|n| format!("row {n}\n")).collect()
    }

    /// A window big enough that the character budget is the 100,000-character ceiling.
    const BIG_WINDOW: Option<u64> = Some(1_000_000);

    #[tokio::test]
    async fn a_read_without_a_limit_stops_at_the_default_line_cap_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("big.txt"), numbered(5000)).unwrap();
        let read = read_file_in(&workspace, "big.txt", None, None, BIG_WINDOW)
            .await
            .unwrap();
        assert!(read.contains("  2000\trow 2000\n"), "{read}");
        assert!(!read.contains("row 2001"), "{read}");
        assert!(
            read.contains("[lines 1-2000 of 5000; use offset=2001 to read more]"),
            "{read}"
        );
        // Following the note reads the next page, and the last page has no note.
        let next = read_file_in(&workspace, "big.txt", Some(2001), None, BIG_WINDOW)
            .await
            .unwrap();
        assert!(next.starts_with("  2001\trow 2001\n"), "{next}");
        assert!(next.contains("[lines 2001-4000 of 5000; use offset=4001 to read more]"));
        let last = read_file_in(&workspace, "big.txt", Some(4001), None, BIG_WINDOW)
            .await
            .unwrap();
        assert!(last.contains("  5000\trow 5000\n"), "{last}");
        assert!(!last.contains("[lines"), "{last}");
    }

    #[tokio::test]
    async fn the_character_budget_follows_the_window_and_stops_on_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        // 400 lines of 400 characters; each numbered line is 408 characters.
        let line = "x".repeat(400);
        std::fs::write(dir.path().join("wide.txt"), format!("{line}\n").repeat(400)).unwrap();
        // A small window gets the 8,000-character floor: 19 whole lines fit, the 20th does not.
        let small = read_file_in(&workspace, "wide.txt", None, None, Some(8_000))
            .await
            .unwrap();
        assert!(small.contains("    19\t"), "{small}");
        assert!(!small.contains("    20\t"), "{small}");
        assert!(
            small.ends_with(
                "[lines 1-19 of 400; stopped at the 8000-character read budget; use offset=20 to read more]\n"
            ),
            "{small}"
        );
        // Without a known window the budget is 32,000 characters: 78 lines.
        let unknown = read_file_in(&workspace, "wide.txt", None, None, None)
            .await
            .unwrap();
        assert!(
            unknown.contains("[lines 1-78 of 400; stopped at the 32000-character read budget; use offset=79 to read more]"),
            "{unknown}"
        );
        // A window of 160,000 tokens gives 80,000 characters.
        let large = read_file_in(&workspace, "wide.txt", None, None, Some(160_000))
            .await
            .unwrap();
        assert!(large.contains("; stopped at the 80000-character read budget; use offset="));
        // An explicit limit keeps the ceiling whatever the window.
        let limited = read_file_in(&workspace, "wide.txt", None, Some(400), Some(8_000))
            .await
            .unwrap();
        assert!(limited.contains("stopped at the 100000-character read budget"));
        // The next offset the note gives continues exactly where the read stopped.
        let next = read_file_in(&workspace, "wide.txt", Some(20), None, Some(8_000))
            .await
            .unwrap();
        assert!(next.starts_with("    20\t"), "{next}");
    }

    #[tokio::test]
    async fn one_line_past_the_budget_points_to_grep() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("min.js"), "z".repeat(50_000) + "\nnext\n").unwrap();
        let read = read_file_in(&workspace, "min.js", None, None, Some(8_000))
            .await
            .unwrap();
        assert!(read.starts_with("     1\tzzz"), "{read}");
        assert!(
            read.ends_with(
                "[truncated: line 1 is longer than the 8000-character read budget; use grep to find the part you need]\n"
            ),
            "{read}"
        );
    }

    #[tokio::test]
    async fn an_explicit_limit_and_offset_read_that_range_with_a_note_when_more_remains() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("f.txt"), numbered(40)).unwrap();
        let read = read_file(&workspace, "f.txt", Some(10), Some(5))
            .await
            .unwrap();
        assert!(read.starts_with("    10\trow 10\n"), "{read}");
        assert!(read.contains("    14\trow 14\n"), "{read}");
        assert!(!read.contains("row 15"), "{read}");
        assert!(read.contains("[lines 10-14 of 40; use offset=15 to read more]"));
        // A range that reaches the end, and a whole small file, carry no note.
        let tail = read_file(&workspace, "f.txt", Some(38), Some(10))
            .await
            .unwrap();
        assert!(!tail.contains("[lines"), "{tail}");
        let whole = read_file(&workspace, "f.txt", None, None).await.unwrap();
        assert_eq!(whole, numbered_lines(40));
        // Past the end says how long the file is.
        let past = read_file(&workspace, "f.txt", Some(99), None)
            .await
            .unwrap();
        assert_eq!(past, "[no lines in that range; the file has 40]");
    }

    fn numbered_lines(count: usize) -> String {
        (1..=count).map(|n| format!("{n:>6}\trow {n}\n")).collect()
    }

    #[tokio::test]
    async fn list_dir_pages_a_long_folder_and_leaves_a_short_one_alone() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        for n in 0..620 {
            std::fs::write(dir.path().join(format!("f{n:04}.txt")), "").unwrap();
        }
        let first = list_dir(&workspace, ".", None).await.unwrap();
        assert!(first.starts_with("f0000.txt\n"), "{first}");
        assert!(first.contains("f0499.txt\n"), "{first}");
        assert!(!first.contains("f0500.txt"), "{first}");
        assert!(first.ends_with("[entries 1-500 of 620; use offset=500 to see more]"));
        let second = list_dir(&workspace, ".", Some(500)).await.unwrap();
        assert!(second.starts_with("f0500.txt\n"), "{second}");
        assert!(second.contains("f0619.txt"), "{second}");
        assert!(!second.contains("use offset"), "{second}");
        let past = list_dir(&workspace, ".", Some(900)).await.unwrap();
        assert_eq!(past, "[no entries past offset 900 (620 in all)]");
    }
}
