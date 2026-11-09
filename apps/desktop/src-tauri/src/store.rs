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
}
