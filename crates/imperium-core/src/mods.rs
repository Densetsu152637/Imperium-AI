//! UI mods: a `mod.json` manifest and an ES module entry, installed in `<config>/mods/<id>/` or
//! shipped with Imperium in its `mods/` folder. The daemon only reads them; the desktop app runs
//! each in a sandboxed frame, and a bundled mod is off until the user turns it on.

use std::path::{Path, PathBuf};

use imperium_protocol::methods::{ModInfo, ModListResult, ModPanel};
use serde::Deserialize;

/// Largest entry module served.
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
/// Permissions a mod may ask for; the desktop app enforces them.
const PERMISSIONS: [&str; 4] = ["telemetry:read", "sessions:read", "pets:read", "storage"];

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

/// Where mods shipped with Imperium may be: the resource folders, then the source tree. Debug
/// builds look in the source tree first, since building the desktop app leaves a copy of
/// `mods/` beside the daemon that goes stale as soon as a mod is rebuilt.
pub fn bundled_roots() -> Vec<PathBuf> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods");
    let mut roots = crate::state::bundled_dirs("mods");
    if cfg!(debug_assertions) {
        roots.insert(0, source);
    } else {
        roots.push(source);
    }
    roots
}

/// Every mod in the user's folder, then in the bundled folders. A mod installed by the user wins
/// over a bundled one with the same id; problems are reported per mod.
pub fn list(user_dir: &Path, bundled: &[PathBuf]) -> ModListResult {
    let mut mods = Vec::new();
    let mut problems = Vec::new();
    let roots =
        std::iter::once((user_dir, false)).chain(bundled.iter().map(|dir| (dir.as_path(), true)));
    for (root, is_bundled) in roots {
        // Ids taken by earlier folders: this one yields to them without complaint.
        let taken: Vec<String> = mods.iter().map(|m: &ModInfo| m.id.clone()).collect();
        let mut dirs: Vec<_> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join("mod.json").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            match load(&dir, is_bundled) {
                Ok(info) if taken.contains(&info.id) => {}
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
    }
    ModListResult { mods, problems }
}

fn load(dir: &Path, bundled: bool) -> Result<ModInfo, String> {
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
        bundled,
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

        let listing = list(dir.path(), &[]);
        assert_eq!(listing.mods.len(), 1);
        let info = &listing.mods[0];
        assert_eq!(info.name, "tokens-per-dollar");
        assert_eq!(info.panels[0].title, "Tokens / $");
        assert_eq!(info.source, "export default 1;");
        assert_eq!(listing.problems.len(), 2, "{:?}", listing.problems);
        assert!(!info.bundled);
        assert!(list(&dir.path().join("missing"), &[]).mods.is_empty());
    }

    fn write_mod(root: &Path, folder: &str, id: &str, source: &str) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mod.json"),
            format!(r#"{{"id": "{id}", "permissions": ["pets:read", "storage"]}}"#),
        )
        .unwrap();
        std::fs::write(dir.join("index.js"), source).unwrap();
    }

    #[test]
    fn installed_mods_win_over_bundled_ones_and_bundled_are_flagged() {
        let config = tempfile::tempdir().unwrap();
        let shipped = tempfile::tempdir().unwrap();
        let beside = tempfile::tempdir().unwrap();
        write_mod(config.path(), "mine", "nursery", "installed");
        write_mod(shipped.path(), "nursery", "nursery", "bundled");
        write_mod(shipped.path(), "extra", "extra", "bundled extra");
        write_mod(beside.path(), "extra", "extra", "same id, other root");

        let roots = [shipped.path().to_path_buf(), beside.path().to_path_buf()];
        let listing = list(config.path(), &roots);
        assert!(listing.problems.is_empty(), "{:?}", listing.problems);
        let ids: Vec<_> = listing.mods.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["nursery", "extra"]);
        assert_eq!(listing.mods[0].source, "installed");
        assert!(!listing.mods[0].bundled);
        assert_eq!(listing.mods[1].source, "bundled extra");
        assert!(listing.mods[1].bundled);
        assert_eq!(listing.mods[1].permissions, ["pets:read", "storage"]);
    }
}
