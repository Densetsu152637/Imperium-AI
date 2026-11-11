//! Environments: named sets of folders sessions work in, kept in `environments.json` in the
//! daemon's data folder. An environment's folders are the sandbox its sessions' tools may use;
//! the first is the working directory. An environment's runtime says where its tools run; for a
//! remote runtime (a Docker container, WSL, SSH) its folders are paths there and are not checked
//! on this machine.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use imperium_protocol::methods::{
    EnvironmentCreateParams, EnvironmentInfo, EnvironmentRuntime, EnvironmentUpdateParams,
};
use imperium_tools::runtime::{normalize_posix, wsl_path};
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
        params: EnvironmentCreateParams,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        let name = valid_name(&params.name)?;
        let runtime = valid_runtime(params.runtime.unwrap_or_default())?;
        let permissions = params
            .permissions
            .as_ref()
            .map(crate::permissions::checked)
            .transpose()
            .map_err(EnvironmentError::Invalid)?
            .unwrap_or_default();
        let mut checked: Vec<String> = Vec::new();
        for folder in &params.folders {
            let folder = folder_for(&runtime, folder)?;
            if !checked
                .iter()
                .any(|known| same_in(&runtime, known, &folder))
            {
                checked.push(folder);
            }
        }
        let env = EnvironmentInfo {
            id: new_id().map_err(|error| EnvironmentError::Invalid(error.to_string()))?,
            name,
            icon: params.icon.filter(|icon| !icon.is_empty()),
            color: params.color.filter(|color| !color.is_empty()),
            folders: checked,
            runtime,
            persistent_agent: params.persistent_agent.unwrap_or(false),
            agent_session_id: None,
            pet: params.pet.filter(|pet| !pet.is_empty()),
            pinned: false,
            permissions,
            project_worktrees: params.project_worktrees.unwrap_or(true),
        };
        self.change(|list| {
            list.push(env.clone());
            Ok(())
        })?;
        Ok(env)
    }

    /// Applies the fields `params` sets. A new runtime keeps the folders (translating Windows
    /// paths for WSL).
    pub fn update(
        &self,
        params: EnvironmentUpdateParams,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        let name = params.name.as_deref().map(valid_name).transpose()?;
        let runtime = params.runtime.map(valid_runtime).transpose()?;
        let permissions = params
            .permissions
            .as_ref()
            .map(crate::permissions::checked)
            .transpose()
            .map_err(EnvironmentError::Invalid)?;
        self.change_one(&params.id, |env| {
            if let Some(name) = name {
                env.name = name;
            }
            if let Some(icon) = params.icon {
                env.icon = (!icon.is_empty()).then_some(icon);
            }
            if let Some(color) = params.color {
                env.color = (!color.is_empty()).then_some(color);
            }
            if let Some(pinned) = params.pinned {
                env.pinned = pinned;
            }
            if let Some(runtime) = runtime {
                if matches!(runtime, EnvironmentRuntime::Wsl { .. }) {
                    for folder in &mut env.folders {
                        *folder = wsl_path(folder);
                    }
                }
                env.runtime = runtime;
            }
            if let Some(on) = params.persistent_agent {
                env.persistent_agent = on;
            }
            if let Some(pet) = params.pet {
                env.pet = (!pet.is_empty()).then_some(pet);
            }
            if let Some(permissions) = permissions {
                env.permissions = permissions;
            }
            if let Some(on) = params.project_worktrees {
                env.project_worktrees = on;
            }
            Ok(())
        })
    }

    /// Records the persistent agent's session id.
    pub fn set_agent_session(
        &self,
        id: &str,
        session_id: Option<String>,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        self.change_one(id, |env| {
            env.agent_session_id = session_id;
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
        let runtime = self
            .get(id)
            .ok_or_else(|| EnvironmentError::NotFound(id.to_string()))?
            .runtime;
        let folder = folder_for(&runtime, folder)?;
        self.change_one(id, |env| {
            if !env
                .folders
                .iter()
                .any(|known| same_in(&runtime, known, &folder))
            {
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
            let runtime = env.runtime.clone();
            let folder = match runtime {
                EnvironmentRuntime::Wsl { .. } => wsl_path(folder),
                _ => folder.to_string(),
            };
            env.folders
                .retain(|known| !same_in(&runtime, known, &folder));
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
        self.create(EnvironmentCreateParams {
            name,
            folders: vec![folder],
            ..EnvironmentCreateParams::default()
        })
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

/// A runtime with its fields trimmed and checked: names that are empty, contain spaces, or
/// start with `-` (which the client programs would read as an option) are refused.
fn valid_runtime(runtime: EnvironmentRuntime) -> Result<EnvironmentRuntime, EnvironmentError> {
    let word = |what: &str, value: &str| -> Result<String, EnvironmentError> {
        let value = value.trim();
        if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_whitespace) {
            return Err(EnvironmentError::Invalid(format!(
                "{value:?} is not a valid {what}"
            )));
        }
        Ok(value.to_string())
    };
    Ok(match runtime {
        EnvironmentRuntime::Local => EnvironmentRuntime::Local,
        EnvironmentRuntime::Docker { image, container } => EnvironmentRuntime::Docker {
            image: word("Docker image", &image)?,
            container: container
                .filter(|container| !container.trim().is_empty())
                .map(|container| word("container name", &container))
                .transpose()?,
        },
        EnvironmentRuntime::Wsl { distro } => EnvironmentRuntime::Wsl {
            distro: word("WSL distribution", &distro)?,
        },
        EnvironmentRuntime::Ssh { host } => EnvironmentRuntime::Ssh {
            host: word("SSH host", &host)?,
        },
    })
}

/// A folder as the environment keeps it: an existing local folder, or for a remote runtime an
/// absolute path inside it.
fn folder_for(runtime: &EnvironmentRuntime, folder: &str) -> Result<String, EnvironmentError> {
    if !runtime.is_remote() {
        return existing_folder(folder);
    }
    let written = match runtime {
        EnvironmentRuntime::Wsl { .. } => wsl_path(folder),
        _ => folder.trim().to_string(),
    };
    normalize_posix(&written).ok_or_else(|| {
        EnvironmentError::Invalid(format!(
            "{folder} is not an absolute path inside the runtime (such as /home/me/project)"
        ))
    })
}

/// Whether two folders are the same: compared as written for a remote runtime.
fn same_in(runtime: &EnvironmentRuntime, a: &str, b: &str) -> bool {
    if runtime.is_remote() {
        let normal = |path: &str| normalize_posix(path).unwrap_or_else(|| path.to_string());
        return normal(a) == normal(b);
    }
    same(a, b)
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

    fn new(
        envs: &Environments,
        name: &str,
        icon: Option<String>,
        folders: &[String],
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        envs.create(EnvironmentCreateParams {
            name: name.into(),
            icon,
            folders: folders.to_vec(),
            ..EnvironmentCreateParams::default()
        })
    }

    fn remote(
        envs: &Environments,
        runtime: EnvironmentRuntime,
        folders: &[&str],
    ) -> EnvironmentInfo {
        envs.create(EnvironmentCreateParams {
            name: "Remote".into(),
            folders: folders.iter().map(|folder| folder.to_string()).collect(),
            runtime: Some(runtime),
            ..EnvironmentCreateParams::default()
        })
        .unwrap()
    }

    #[test]
    fn remote_folders_are_paths_inside_the_runtime() {
        let data = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let ssh = remote(
            &envs,
            EnvironmentRuntime::Ssh {
                host: " me@box:22 ".into(),
            },
            &["/srv/app/", "/srv/app", "/home/me/../me/notes"],
        );
        assert_eq!(
            ssh.runtime,
            EnvironmentRuntime::Ssh {
                host: "me@box:22".into()
            }
        );
        assert_eq!(ssh.folders, ["/srv/app", "/home/me/notes"]);
        assert!(envs.add_folder(&ssh.id, "relative").is_err());
        let wsl = remote(
            &envs,
            EnvironmentRuntime::Wsl {
                distro: "Ubuntu".into(),
            },
            &[r"C:\work\app"],
        );
        assert_eq!(wsl.folders, ["/mnt/c/work/app"]);
        let wsl = envs.remove_folder(&wsl.id, r"C:\work\app").unwrap();
        assert!(wsl.folders.is_empty());

        // Switching a local environment to WSL translates its folders.
        let local = tempfile::tempdir().unwrap();
        let env = new(&envs, "Local", None, &[path(&local)]).unwrap();
        let switched = envs
            .update(EnvironmentUpdateParams {
                id: env.id.clone(),
                runtime: Some(EnvironmentRuntime::Wsl {
                    distro: "Ubuntu".into(),
                }),
                persistent_agent: Some(true),
                ..EnvironmentUpdateParams::default()
            })
            .unwrap();
        assert!(switched.persistent_agent);
        if cfg!(windows) {
            assert!(
                switched.folders[0].starts_with("/mnt/"),
                "{:?}",
                switched.folders
            );
        }
        // A Docker image keeps local folders.
        let docker = remote(
            &envs,
            EnvironmentRuntime::Docker {
                image: "rust:1".into(),
                container: Some(" ".into()),
            },
            &[&path(&local)],
        );
        assert_eq!(
            docker.runtime,
            EnvironmentRuntime::Docker {
                image: "rust:1".into(),
                container: None
            }
        );
        for bad in [
            EnvironmentRuntime::Ssh {
                host: "-oProxyCommand=x".into(),
            },
            EnvironmentRuntime::Wsl { distro: "".into() },
            EnvironmentRuntime::Docker {
                image: "a b".into(),
                container: None,
            },
        ] {
            assert!(
                envs.create(EnvironmentCreateParams {
                    name: "bad".into(),
                    runtime: Some(bad),
                    ..EnvironmentCreateParams::default()
                })
                .is_err()
            );
        }
        // Saved and read back.
        assert_eq!(store(data.path()).list(), envs.list());
    }

    #[test]
    fn environments_are_created_changed_and_kept_on_disk() {
        let data = tempfile::tempdir().unwrap();
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let env = new(&envs, " Work ", Some("code".into()), &[path(&one)]).unwrap();
        assert_eq!(env.name, "Work");
        assert_eq!(env.folders.len(), 1);

        let env = envs.add_folder(&env.id, &path(&two)).unwrap();
        assert_eq!(env.folders.len(), 2);
        // The same folder spelled differently is not added twice.
        let again = format!("{}{}", path(&two), std::path::MAIN_SEPARATOR);
        assert_eq!(envs.add_folder(&env.id, &again).unwrap().folders.len(), 2);
        let other = new(&envs, "Other", None, &[]).unwrap();
        let renamed = envs
            .update(EnvironmentUpdateParams {
                id: env.id.clone(),
                name: Some("Day job".into()),
                color: Some("#2f6fdb".into()),
                ..EnvironmentUpdateParams::default()
            })
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
    fn the_default_pet_is_set_changed_and_cleared() {
        let data = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let plain = new(&envs, "Plain", None, &[]).unwrap();
        assert_eq!(plain.pet, None);
        let env = envs
            .create(EnvironmentCreateParams {
                name: "Pets".into(),
                pet: Some("pixel-clawd".into()),
                ..EnvironmentCreateParams::default()
            })
            .unwrap();
        assert_eq!(env.pet.as_deref(), Some("pixel-clawd"));
        // A fresh store reads it back; an update that leaves it out keeps it.
        let reloaded = store(data.path());
        assert_eq!(
            reloaded.get(&env.id).unwrap().pet.as_deref(),
            Some("pixel-clawd")
        );
        let renamed = reloaded
            .update(EnvironmentUpdateParams {
                id: env.id.clone(),
                name: Some("Renamed".into()),
                ..EnvironmentUpdateParams::default()
            })
            .unwrap();
        assert_eq!(renamed.pet.as_deref(), Some("pixel-clawd"));
        let changed = reloaded
            .update(EnvironmentUpdateParams {
                id: env.id.clone(),
                pet: Some("other".into()),
                ..EnvironmentUpdateParams::default()
            })
            .unwrap();
        assert_eq!(changed.pet.as_deref(), Some("other"));
        let cleared = reloaded
            .update(EnvironmentUpdateParams {
                id: env.id,
                pet: Some(String::new()),
                ..EnvironmentUpdateParams::default()
            })
            .unwrap();
        assert_eq!(cleared.pet, None);
        assert_eq!(store(data.path()).list()[1].pet, None);
    }

    #[test]
    fn bad_names_and_folders_are_refused() {
        let data = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        assert!(new(&envs, "  ", None, &[]).is_err());
        assert!(new(&envs, "x", None, &["relative".into()]).is_err());
        let missing = data.path().join("missing").to_string_lossy().into_owned();
        assert!(matches!(
            new(&envs, "x", None, &[missing]),
            Err(EnvironmentError::Folder(..))
        ));
        assert!(
            envs.list().is_empty(),
            "nothing is created when a folder is refused"
        );
        new(&envs, "Twin", None, &[]).unwrap();
        new(&envs, "twin", None, &[]).unwrap();
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
