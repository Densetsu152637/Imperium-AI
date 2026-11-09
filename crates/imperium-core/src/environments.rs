//! Environments: named sets of folders sessions work in, kept in `environments.json` in the
//! daemon's data folder. An environment's folders are the sandbox its sessions' tools may use;
//! the first is the working directory.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use imperium_protocol::methods::EnvironmentInfo;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    #[error("no environment {0:?}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("folder {0}: {1}")]
    Folder(String, std::io::Error),
    #[error("could not save the environments: {0}")]
    Save(std::io::Error),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    environments: Vec<EnvironmentInfo>,
}

/// The environment store. Every change is written to disk before it is visible.
#[derive(Debug)]
pub struct Environments {
    path: PathBuf,
    list: Mutex<Vec<EnvironmentInfo>>,
}

impl Environments {
    /// Reads `path`; a missing file is an empty store, an unreadable one is set aside with a
    /// warning so it is not overwritten.
    pub fn load(path: PathBuf) -> Self {
        let list = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<File>(&text) {
                Ok(file) => file.environments,
                Err(error) => {
                    let aside = path.with_extension("json.unreadable");
                    tracing::warn!(path = %path.display(), %error, "could not read the environments; setting the file aside");
                    let _ = std::fs::rename(&path, aside);
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Environments {
            path,
            list: Mutex::new(list),
        }
    }

    pub fn list(&self) -> Vec<EnvironmentInfo> {
        self.lock().clone()
    }

    pub fn get(&self, id: &str) -> Option<EnvironmentInfo> {
        self.lock().iter().find(|env| env.id == id).cloned()
    }

    /// Finds an environment by id, else by name (ignoring case); a name shared by several
    /// environments is refused as ambiguous.
    pub fn find(&self, id_or_name: &str) -> Result<EnvironmentInfo, EnvironmentError> {
        if let Some(env) = self.get(id_or_name) {
            return Ok(env);
        }
        let list = self.lock();
        let named: Vec<_> = list
            .iter()
            .filter(|env| env.name.eq_ignore_ascii_case(id_or_name.trim()))
            .collect();
        match named.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(EnvironmentError::NotFound(id_or_name.to_string())),
            _ => Err(EnvironmentError::Invalid(format!(
                "{} environments are named {id_or_name:?}; use an id",
                named.len()
            ))),
        }
    }

    pub fn create(
        &self,
        name: &str,
        icon: Option<String>,
        color: Option<String>,
        folders: &[String],
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        let name = valid_name(name)?;
        let mut checked: Vec<String> = Vec::new();
        for folder in folders {
            let folder = existing_folder(folder)?;
            if !checked.iter().any(|known| same(known, &folder)) {
                checked.push(folder);
            }
        }
        let env = EnvironmentInfo {
            id: new_id().map_err(|error| EnvironmentError::Invalid(error.to_string()))?,
            name,
            icon: icon.filter(|icon| !icon.is_empty()),
            color: color.filter(|color| !color.is_empty()),
            folders: checked,
            pinned: false,
        };
        self.change(|list| {
            list.push(env.clone());
            Ok(())
        })?;
        Ok(env)
    }

    pub fn update(
        &self,
        id: &str,
        name: Option<&str>,
        icon: Option<String>,
        color: Option<String>,
        pinned: Option<bool>,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        let name = name.map(valid_name).transpose()?;
        self.change_one(id, |env| {
            if let Some(name) = name {
                env.name = name;
            }
            if let Some(icon) = icon {
                env.icon = (!icon.is_empty()).then_some(icon);
            }
            if let Some(color) = color {
                env.color = (!color.is_empty()).then_some(color);
            }
            if let Some(pinned) = pinned {
                env.pinned = pinned;
            }
            Ok(())
        })
    }

    pub fn delete(&self, id: &str) -> Result<(), EnvironmentError> {
        self.change(|list| {
            let before = list.len();
            list.retain(|env| env.id != id);
            if list.len() == before {
                return Err(EnvironmentError::NotFound(id.to_string()));
            }
            Ok(())
        })
    }

    /// Adds an existing folder; one already in the environment is left where it is.
    pub fn add_folder(&self, id: &str, folder: &str) -> Result<EnvironmentInfo, EnvironmentError> {
        let folder = existing_folder(folder)?;
        self.change_one(id, |env| {
            if !env.folders.iter().any(|known| same(known, &folder)) {
                env.folders.push(folder);
            }
            Ok(())
        })
    }

    /// Removes a folder (compared as the same folder, whatever its spelling). The folder need
    /// not exist any more.
    pub fn remove_folder(
        &self,
        id: &str,
        folder: &str,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        self.change_one(id, |env| {
            let before = env.folders.len();
            env.folders.retain(|known| !same(known, folder));
            if env.folders.len() == before {
                return Err(EnvironmentError::Invalid(format!(
                    "{folder} is not a folder of {}",
                    env.name
                )));
            }
            Ok(())
        })
    }

    /// The environment whose working directory is `folder`, created (named after the folder)
    /// when there is none. This is how a bare project folder becomes an environment.
    pub fn for_folder(&self, folder: &str) -> Result<EnvironmentInfo, EnvironmentError> {
        let folder = existing_folder(folder)?;
        if let Some(env) = self.lock().iter().find(|env| {
            env.folders
                .first()
                .is_some_and(|first| same(first, &folder))
        }) {
            return Ok(env.clone());
        }
        let name = Path::new(&folder)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| folder.clone());
        self.create(&name, None, None, &[folder])
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<EnvironmentInfo>> {
        self.list
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Applies `edit` to a copy, saves it, then makes it visible.
    fn change(
        &self,
        edit: impl FnOnce(&mut Vec<EnvironmentInfo>) -> Result<(), EnvironmentError>,
    ) -> Result<(), EnvironmentError> {
        let mut list = self.lock();
        let mut next = list.clone();
        edit(&mut next)?;
        save(&self.path, &next).map_err(EnvironmentError::Save)?;
        *list = next;
        Ok(())
    }

    fn change_one(
        &self,
        id: &str,
        edit: impl FnOnce(&mut EnvironmentInfo) -> Result<(), EnvironmentError>,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        let mut changed = None;
        self.change(|list| {
            let env = list
                .iter_mut()
                .find(|env| env.id == id)
                .ok_or_else(|| EnvironmentError::NotFound(id.to_string()))?;
            edit(env)?;
            changed = Some(env.clone());
            Ok(())
        })?;
        Ok(changed.expect("set when the change succeeds"))
    }
}

fn save(path: &Path, list: &[EnvironmentInfo]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&serde_json::json!({ "environments": list }))
        .map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

fn valid_name(name: &str) -> Result<String, EnvironmentError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(EnvironmentError::Invalid(
            "an environment needs a name".into(),
        ));
    }
    Ok(name.to_string())
}

/// An existing directory, as people write it (absolute, without Windows' `\\?\` prefix).
fn existing_folder(folder: &str) -> Result<String, EnvironmentError> {
    let path = Path::new(folder.trim());
    if !path.is_absolute() {
        return Err(EnvironmentError::Invalid(format!(
            "{folder} is not an absolute path"
        )));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| EnvironmentError::Folder(folder.to_string(), error))?;
    if !canonical.is_dir() {
        return Err(EnvironmentError::Invalid(format!(
            "{folder} is not a folder"
        )));
    }
    Ok(plain(&canonical))
}

fn plain(path: &Path) -> String {
    let shown = path.display().to_string();
    match shown.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => rest.to_string(),
        _ => shown,
    }
}

/// Whether two paths name the same folder: equal once canonical, or (for folders that no longer
/// exist) equal as written, ignoring separators, a trailing separator, and case on Windows.
fn same(a: &str, b: &str) -> bool {
    match (Path::new(a).canonicalize(), Path::new(b).canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => {
            let normal = |path: &str| {
                let path = path.replace('\\', "/");
                let path = path.trim_end_matches('/').to_string();
                if cfg!(windows) {
                    path.to_lowercase()
                } else {
                    path
                }
            };
            normal(a) == normal(b)
        }
    }
}

fn new_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> Environments {
        Environments::load(dir.join("environments.json"))
    }

    fn path(dir: &tempfile::TempDir) -> String {
        dir.path().to_string_lossy().into_owned()
    }

    #[test]
    fn environments_are_created_changed_and_kept_on_disk() {
        let data = tempfile::tempdir().unwrap();
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let env = envs
            .create(" Work ", Some("code".into()), None, &[path(&one)])
            .unwrap();
        assert_eq!(env.name, "Work");
        assert_eq!(env.folders.len(), 1);

        let env = envs.add_folder(&env.id, &path(&two)).unwrap();
        assert_eq!(env.folders.len(), 2);
        // The same folder spelled differently is not added twice.
        let again = format!("{}{}", path(&two), std::path::MAIN_SEPARATOR);
        assert_eq!(envs.add_folder(&env.id, &again).unwrap().folders.len(), 2);
        let other = envs.create("Other", None, None, &[]).unwrap();
        let renamed = envs
            .update(&env.id, Some("Day job"), None, Some("#2f6fdb".into()), None)
            .unwrap();
        assert_eq!(renamed.name, "Day job");
        assert_eq!(renamed.icon.as_deref(), Some("code"));
        assert_eq!(renamed.color.as_deref(), Some("#2f6fdb"));

        // A fresh store reads what was saved.
        let reloaded = store(data.path());
        assert_eq!(reloaded.list(), envs.list());
        assert_eq!(reloaded.find("day JOB").unwrap().id, env.id);

        let env = reloaded.remove_folder(&env.id, &path(&one)).unwrap();
        assert_eq!(env.folders, vec![existing_folder(&path(&two)).unwrap()]);
        assert!(reloaded.remove_folder(&env.id, &path(&one)).is_err());
        reloaded.delete(&other.id).unwrap();
        assert!(matches!(
            reloaded.delete(&other.id),
            Err(EnvironmentError::NotFound(_))
        ));
        assert_eq!(store(data.path()).list().len(), 1);
    }

    #[test]
    fn bad_names_and_folders_are_refused() {
        let data = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        assert!(envs.create("  ", None, None, &[]).is_err());
        assert!(envs.create("x", None, None, &["relative".into()]).is_err());
        let missing = data.path().join("missing").to_string_lossy().into_owned();
        assert!(matches!(
            envs.create("x", None, None, &[missing]),
            Err(EnvironmentError::Folder(..))
        ));
        assert!(
            envs.list().is_empty(),
            "nothing is created when a folder is refused"
        );
        envs.create("Twin", None, None, &[]).unwrap();
        envs.create("twin", None, None, &[]).unwrap();
        assert!(matches!(
            envs.find("TWIN"),
            Err(EnvironmentError::Invalid(_))
        ));
    }

    #[test]
    fn a_bare_folder_maps_to_one_environment() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let first = envs.for_folder(&path(&project)).unwrap();
        let again = envs.for_folder(&path(&project)).unwrap();
        assert_eq!(first.id, again.id);
        assert_eq!(envs.list().len(), 1);
        assert_eq!(
            first.name,
            project.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[test]
    fn an_unreadable_file_is_set_aside() {
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("environments.json"), "not json").unwrap();
        let envs = store(data.path());
        assert!(envs.list().is_empty());
        assert!(data.path().join("environments.json.unreadable").exists());
    }
}
