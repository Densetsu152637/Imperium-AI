//! The interface's own files: settings, panel layout, and the last-known sessions it shows while
//! the daemon is away. They live in Imperium's config folder under `desktop/`, beside the
//! daemon's `config.toml`, rather than in the webview's storage or the installer's app-data
//! folder, so they survive updates, reinstalls, and uninstalls.

use std::path::PathBuf;

use imperium_config::Paths;

/// Where the documents live.
fn folder() -> Result<PathBuf, String> {
    Paths::from_env()
        .map(|paths| paths.config_dir().join("desktop"))
        .map_err(|error| error.to_string())
}

/// A document's file. Names are plain words, so a name can never reach outside the folder.
fn file(name: &str) -> Result<PathBuf, String> {
    let plain = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !plain {
        return Err(format!("not a document name: {name:?}"));
    }
    Ok(folder()?.join(format!("{name}.json")))
}

/// The document's text, or `None` if it was never saved.
#[tauri::command]
pub fn store_read(name: String) -> Result<Option<String>, String> {
    match std::fs::read_to_string(file(&name)?) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Replaces the document. The text goes to a temporary file first and is then renamed over the
/// old one, so a crash mid-write never leaves half a file.
#[tauri::command]
pub fn store_write(name: String, contents: String) -> Result<(), String> {
    let path = file(&name)?;
    let dir = folder()?;
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, contents).map_err(|error| error.to_string())?;
    std::fs::rename(&partial, &path).map_err(|error| error.to_string())
}

/// The largest transcript the shell will save.
const EXPORT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Whether `filename` is a plain `.md` or `.json` name (a suggestion for the save dialog, never
/// a path) and `contents` is a size worth saving; returns the extension.
fn check_export(filename: &str, contents: &str) -> Result<&'static str, String> {
    let extension = if filename.ends_with(".md") {
        "md"
    } else if filename.ends_with(".json") {
        "json"
    } else {
        return Err(format!("not an export file name: {filename:?}"));
    };
    let plain = filename.len() <= 120
        && !filename.starts_with('.')
        && filename.chars().all(|c| {
            !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        });
    if !plain {
        return Err(format!("not an export file name: {filename:?}"));
    }
    if contents.len() > EXPORT_MAX_BYTES {
        return Err("the transcript is too large to save".into());
    }
    Ok(extension)
}

/// What to ask before saving a transcript the daemon warned about, if anything. The question is
/// asked here, in the shell, as a guard for the user rather than a security boundary: the warning
/// is passed in by the page, which can send none, so a missing warning is not a promise that the
/// transcript is clean.
fn confirmation(warning: Option<&str>) -> Option<String> {
    let warning = warning?.trim();
    (!warning.is_empty()).then(|| format!("{warning}\n\nSave it anyway?"))
}

/// Saves an exported transcript: asks first when the daemon `warning`ed that it looks like it
/// holds a credential, then opens the save dialog itself, suggesting `filename`, and writes
/// `contents` to the one path the user picks there. Returns false when either was cancelled.
/// The page cannot choose where the file goes, so it cannot write anywhere on its own (it can,
/// though, pass `warning: null` to skip the question).
#[tauri::command]
pub async fn save_export(
    app: tauri::AppHandle,
    filename: String,
    contents: String,
    warning: Option<String>,
) -> Result<bool, String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let extension = check_export(&filename, &contents)?;
    if let Some(question) = confirmation(warning.as_deref()) {
        let asker = app.clone();
        let proceed = tauri::async_runtime::spawn_blocking(move || {
            asker
                .dialog()
                .message(question)
                .title("Export transcript")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancelCustom(
                    "Save anyway".into(),
                    "Cancel".into(),
                ))
                .blocking_show()
        })
        .await
        .map_err(|error| error.to_string())?;
        if !proceed {
            return Ok(false);
        }
    }
    let label = if extension == "json" {
        "JSON"
    } else {
        "Markdown"
    };
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Export transcript")
            .set_file_name(&filename)
            .add_filter(label, &[extension])
            .blocking_save_file()
    })
    .await
    .map_err(|error| error.to_string())?;
    let Some(path) = chosen else {
        return Ok(false);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    std::fs::write(path, contents).map_err(|error| error.to_string())?;
    Ok(true)
}

/// The folder, so the interface can tell the user where their settings are.
#[tauri::command]
pub fn store_folder() -> Result<String, String> {
    folder().map(|dir| dir.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_are_documents() {
        assert!(file("settings").is_ok());
        assert!(file("layout-2").is_ok());
        for bad in ["", "../config", "a/b", "Settings", "x.json", "a\\b"] {
            assert!(file(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_a_warned_export_is_confirmed_first() {
        assert_eq!(confirmation(None), None);
        assert_eq!(confirmation(Some("  \n")), None);
        let question = confirmation(Some("Looks like a GitHub token.")).unwrap();
        assert!(question.starts_with("Looks like a GitHub token."));
        assert!(question.ends_with("Save it anyway?"));
    }

    #[test]
    fn exports_take_only_plain_md_and_json_names_of_a_sane_size() {
        assert_eq!(check_export("fix-the-parser.md", "# x"), Ok("md"));
        assert_eq!(check_export("session.json", "{}"), Ok("json"));
        for bad in [
            "",
            "notes.txt",
            "../x.md",
            "a/b.md",
            "a\\b.json",
            "C:evil.md",
            ".hidden.md",
            "bad\u{0}.md",
            "x.md.exe",
        ] {
            assert!(check_export(bad, "x").is_err(), "{bad:?}");
        }
        assert!(check_export(&format!("{}.md", "a".repeat(200)), "x").is_err());
        let huge = "x".repeat(EXPORT_MAX_BYTES + 1);
        assert!(check_export("big.md", &huge).is_err());
    }
}
