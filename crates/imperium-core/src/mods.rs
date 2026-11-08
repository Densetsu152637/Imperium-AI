//! UI mods installed in `<config>/mods/<id>/`: a `mod.json` manifest and an ES module entry.
//! The daemon only reads them; the desktop app runs each in a sandboxed frame.

use std::path::Path;

use imperium_protocol::methods::{ModInfo, ModListResult, ModPanel};
use serde::Deserialize;

/// Largest entry module served.
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
/// Permissions a mod may ask for; the desktop app enforces them.
const PERMISSIONS: [&str; 2] = ["telemetry:read", "sessions:read"];

#[derive(Deserialize)]
struct Manifest {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default = "default_entry")]
    entry: String,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    contributes: Contributes,
}

#[derive(Deserialize, Default)]
struct Contributes {
    #[serde(default)]
    panels: Vec<Panel>,
}

#[derive(Deserialize)]
struct Panel {
    id: String,
    title: String,
}

fn default_entry() -> String {
    "index.js".into()
}

/// Every mod folder under `mods_dir`, with problems reported per mod.
pub fn list(mods_dir: &Path) -> ModListResult {
    let mut mods = Vec::new();
    let mut problems = Vec::new();
    let mut dirs: Vec<_> = std::fs::read_dir(mods_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("mod.json").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        match load(&dir) {
            Ok(info) if mods.iter().any(|m: &ModInfo| m.id == info.id) => {
                problems.push(format!(
                    "{}: the id {} is already used",
                    dir.display(),
                    info.id
                ));
            }
            Ok(info) => mods.push(info),
            Err(error) => problems.push(format!("{}: {error}", dir.display())),
        }
    }
    ModListResult { mods, problems }
}

fn load(dir: &Path) -> Result<ModInfo, String> {
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(dir.join("mod.json")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("mod.json: {error}"))?;
    if let Some(unknown) = manifest
        .permissions
        .iter()
        .find(|permission| !PERMISSIONS.contains(&permission.as_str()))
    {
        return Err(format!("unknown permission {unknown}"));
    }
    let entry = Path::new(&manifest.entry);
    if entry
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("entry must be a path inside the mod folder".into());
    }
    let path = dir.join(entry);
    let size = std::fs::metadata(&path)
        .map_err(|error| format!("{}: {error}", manifest.entry))?
        .len();
    if size > MAX_SOURCE_BYTES {
        return Err(format!("{} is larger than 2 MiB", manifest.entry));
    }
    let source = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    Ok(ModInfo {
        name: manifest.name.unwrap_or_else(|| manifest.id.clone()),
        id: manifest.id,
        version: manifest.version.unwrap_or_default(),
        permissions: manifest.permissions,
        panels: manifest
            .contributes
            .panels
            .into_iter()
            .map(|panel| ModPanel {
                id: panel.id,
                title: panel.title,
            })
            .collect(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mods_load_with_their_panels_and_bad_ones_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("tokens");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(
            good.join("mod.json"),
            r#"{"id": "tokens-per-dollar", "permissions": ["telemetry:read"],
                "contributes": {"panels": [{"id": "tpd", "title": "Tokens / $", "slot": "sidebar.right"}]}}"#,
        )
        .unwrap();
        std::fs::write(good.join("index.js"), "export default 1;").unwrap();
        let bad = dir.path().join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            bad.join("mod.json"),
            r#"{"id": "bad", "permissions": ["fs:write"]}"#,
        )
        .unwrap();
        let escape = dir.path().join("escape");
        std::fs::create_dir_all(&escape).unwrap();
        std::fs::write(
            escape.join("mod.json"),
            r#"{"id": "escape", "entry": "../x.js"}"#,
        )
        .unwrap();

        let listing = list(dir.path());
        assert_eq!(listing.mods.len(), 1);
        let info = &listing.mods[0];
        assert_eq!(info.name, "tokens-per-dollar");
        assert_eq!(info.panels[0].title, "Tokens / $");
        assert_eq!(info.source, "export default 1;");
        assert_eq!(listing.problems.len(), 2, "{:?}", listing.problems);
        assert!(list(&dir.path().join("missing")).mods.is_empty());
    }
}
