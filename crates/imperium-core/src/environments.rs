//! Environments: named sets of folders sessions work in, kept in `environments.json` in the
//! daemon's data folder. An environment's folders are the sandbox its sessions' tools may use;
//! the first is the working directory. An environment's runtime says where its tools run; for a
//! remote runtime (a Docker container, WSL, SSH) its folders are paths there and are not checked
//! on this machine.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use imperium_protocol::agents::{AgentPermissions, EnvironmentAgent};
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

/// The id of the built-in Home environment: fixed, and valid as a folder name
/// ([`crate::workspaces::safe_id`]), since its working folder is named after it.
pub const HOME_ID: &str = "home";

/// The Home environment as first made: no project folders, on this machine. Its working
/// folder holds its `Memory.md` and its sessions' scratch folders.
fn home() -> EnvironmentInfo {
    EnvironmentInfo {
        id: HOME_ID.into(),
        name: "Home".into(),
        icon: Some("house".into()),
        color: Some("#c9822b".into()),
        folders: Vec::new(),
        pinned: false,
        runtime: EnvironmentRuntime::Local,
        persistent_agent: false,
        pet: None,
        agent_session_id: None,
        // No agents until the user adds one.
        agents: Vec::new(),
        permissions: Default::default(),
        // There are no repositories to open.
        project_worktrees: false,
        builtin: true,
    }
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
        // Whether the file was left where it is although it could not be read: then Home is not
        // saved over it.
        let mut keep_file = false;
        let mut set_aside = |path: &Path, error: &dyn std::fmt::Display| {
            let aside = path.with_extension("json.unreadable");
            tracing::warn!(path = %path.display(), %error, "could not read the environments; setting the file aside");
            if let Err(error) = std::fs::rename(path, aside) {
                tracing::warn!(%error, "could not set the environments file aside; it is left as it is");
                keep_file = true;
            }
        };
        let list = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<File>(&text) {
                Ok(file) => file.environments,
                Err(error) => {
                    set_aside(&path, &error);
                    Vec::new()
                }
            },
            // No file yet: a fresh store.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            // Anything else (permissions, a folder in the way, a failing disk) says nothing about
            // what is in the file, so it is never replaced by a store of just Home.
            Err(error) => {
                set_aside(&path, &error);
                Vec::new()
            }
        };
        let mut list = list;
        // Environments saved before multiple agents had one persistent session: it becomes
        // the environment's agent, with the session it had (history and all).
        let mut migrated = false;
        for env in &mut list {
            migrated |= migrate_legacy_agent(env);
            normalize(env);
        }
        if migrated && let Err(error) = save(&path, &list) {
            tracing::warn!(path = %path.display(), %error, "could not save the migrated environments");
        }
        let store = Environments {
            path,
            list: Mutex::new(list),
        };
        store.ensure_home(!keep_file);
        store
    }

    /// Makes sure Home is there, first in the list, and marked built in: on a fresh start, and
    /// for a store written before Home existed. Nothing else in the store is touched.
    fn ensure_home(&self, save_it: bool) {
        let mut list = self.lock();
        let mut next = list.clone();
        match next.iter().position(|env| env.id == HOME_ID) {
            Some(0) => {
                if next[0].builtin {
                    return;
                }
                next[0].builtin = true;
            }
            Some(at) => {
                let mut env = next.remove(at);
                env.builtin = true;
                next.insert(0, env);
            }
            None => next.insert(0, home()),
        }
        if save_it && let Err(error) = save(&self.path, &next) {
            tracing::warn!(%error, "could not save the Home environment; it is kept in memory");
        }
        *list = next;
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
        let mut env = EnvironmentInfo {
            id: new_id().map_err(|error| EnvironmentError::Invalid(error.to_string()))?,
            name,
            icon: params.icon.filter(|icon| !icon.is_empty()),
            color: params.color.filter(|color| !color.is_empty()),
            folders: checked,
            runtime,
            // Deprecated alias: `persistent_agent: true` adds the default agent below.
            persistent_agent: false,
            agent_session_id: None,
            agents: Vec::new(),
            pet: params.pet.filter(|pet| !pet.is_empty()),
            pinned: false,
            permissions,
            project_worktrees: params.project_worktrees.unwrap_or(true),
            builtin: false,
        };
        if params.persistent_agent == Some(true) {
            add_default_agent(&mut env)?;
        }
        normalize(&mut env);
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
                if env.builtin && runtime != EnvironmentRuntime::Local {
                    return Err(EnvironmentError::Invalid(
                        "Home works in its working folder on this machine; its runtime cannot change"
                            .into(),
                    ));
                }
                if matches!(runtime, EnvironmentRuntime::Wsl { .. }) {
                    for folder in &mut env.folders {
                        *folder = wsl_path(folder);
                    }
                }
                env.runtime = runtime;
            }
            // Deprecated alias for the agents list.
            match params.persistent_agent {
                Some(true) if env.agents.is_empty() => add_default_agent(env)?,
                Some(false) => remove_default_agent(env)?,
                _ => {}
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

    /// Records an agent's session id.
    pub fn set_agent_session(
        &self,
        id: &str,
        agent_id: &str,
        session_id: Option<String>,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        self.change_one(id, |env| {
            let agent = find_agent(env, agent_id)?;
            agent.session_id = session_id;
            Ok(())
        })
    }

    /// Adds a persistent agent. Its name is unique in the environment (ignoring case).
    pub fn add_agent(
        &self,
        id: &str,
        name: &str,
        role: Option<&str>,
    ) -> Result<EnvironmentAgent, EnvironmentError> {
        let name = valid_agent_name(name)?;
        let role = valid_role(role.unwrap_or_default())?;
        let agent_id = new_id().map_err(|error| EnvironmentError::Invalid(error.to_string()))?;
        let env = self.change_one(id, |env| {
            if env
                .agents
                .iter()
                .any(|agent| folded(&agent.name) == folded(&name))
            {
                return Err(EnvironmentError::Invalid(format!(
                    "{} already has an agent named {name:?}",
                    env.name
                )));
            }
            env.agents.push(EnvironmentAgent {
                id: agent_id.clone(),
                name: name.clone(),
                role: role.clone(),
                session_id: None,
                permissions: AgentPermissions::default(),
            });
            Ok(())
        })?;
        Ok(env
            .agents
            .into_iter()
            .find(|agent| agent.id == agent_id)
            .expect("just added"))
    }

    /// Renames an agent or changes its role.
    pub fn update_agent(
        &self,
        id: &str,
        agent_id: &str,
        name: Option<&str>,
        role: Option<&str>,
    ) -> Result<EnvironmentAgent, EnvironmentError> {
        let name = name.map(valid_agent_name).transpose()?;
        let role = role.map(valid_role).transpose()?;
        let env = self.change_one(id, |env| {
            if let Some(name) = &name
                && env
                    .agents
                    .iter()
                    .any(|a| a.id != agent_id && folded(&a.name) == folded(name))
            {
                return Err(EnvironmentError::Invalid(format!(
                    "{} already has an agent named {name:?}",
                    env.name
                )));
            }
            let agent = find_agent(env, agent_id)?;
            if let Some(name) = name {
                agent.name = name;
            }
            if let Some(role) = role {
                agent.role = role;
            }
            Ok(())
        })?;
        Ok(env
            .agents
            .into_iter()
            .find(|agent| agent.id == agent_id)
            .expect("checked above"))
    }

    /// Replaces an agent's permission profile (checked and normalised first).
    pub fn set_agent_permissions(
        &self,
        id: &str,
        agent_id: &str,
        permissions: &AgentPermissions,
    ) -> Result<EnvironmentAgent, EnvironmentError> {
        let permissions = crate::permissions::checked_agent_permissions(permissions)
            .map_err(EnvironmentError::Invalid)?;
        let env = self.change_one(id, |env| {
            find_agent(env, agent_id)?.permissions = permissions.clone();
            Ok(())
        })?;
        Ok(env
            .agents
            .into_iter()
            .find(|agent| agent.id == agent_id)
            .expect("checked above"))
    }

    /// Removes an agent and returns it (its session id says what to archive).
    pub fn remove_agent(
        &self,
        id: &str,
        agent_id: &str,
    ) -> Result<EnvironmentAgent, EnvironmentError> {
        let mut removed = None;
        self.change_one(id, |env| {
            let at = env
                .agents
                .iter()
                .position(|agent| agent.id == agent_id)
                .ok_or_else(|| EnvironmentError::NotFound(agent_id.to_string()))?;
            removed = Some(env.agents.remove(at));
            // The last agent's conversation goes with it: the deprecated switch must not bring
            // it back under new instructions. (Only the switch itself and a legacy turned-off
            // agent keep such a link.)
            if env.agents.is_empty() {
                env.agent_session_id = None;
            }
            Ok(())
        })?;
        Ok(removed.expect("removed above"))
    }

    pub fn delete(&self, id: &str) -> Result<(), EnvironmentError> {
        self.change(|list| {
            if list.iter().any(|env| env.id == id && env.builtin) {
                return Err(EnvironmentError::Invalid(
                    "Home is built in and cannot be deleted; rename it or remove its folders instead"
                        .into(),
                ));
            }
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
        // Home is not a project's environment, even once it has been given this folder.
        if let Some(env) = self.lock().iter().find(|env| {
            !env.builtin
                && env
                    .folders
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
        next.iter_mut().for_each(normalize);
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
            normalize(env);
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

/// Keeps the deprecated fields in step with the agents.
///
/// With agents, `agent_session_id` is the first agent's session. Without, it is left as it is:
/// an environment saved with its persistent agent turned off keeps the (archived) session's id,
/// and turning the deprecated switch on again continues that session.
fn normalize(env: &mut EnvironmentInfo) {
    env.persistent_agent = !env.agents.is_empty();
    if let Some(first) = env.agents.first() {
        env.agent_session_id = first.session_id.clone();
    }
}

/// An environment saved with the old single persistent agent gets it as an agent.
fn migrate_legacy_agent(env: &mut EnvironmentInfo) -> bool {
    if env.persistent_agent
        && env.agents.is_empty()
        && let Ok(agent) = default_agent(env.agent_session_id.take())
    {
        env.agents.push(agent);
        return true;
    }
    false
}

const DEFAULT_AGENT_NAME: &str = "Persistent agent";

fn default_agent(session_id: Option<String>) -> Result<EnvironmentAgent, getrandom::Error> {
    Ok(EnvironmentAgent {
        id: new_id()?,
        name: DEFAULT_AGENT_NAME.into(),
        role: String::new(),
        session_id,
        permissions: AgentPermissions::default(),
    })
}

/// Adds the default agent for the deprecated switch, continuing the session a turned-off
/// persistent agent left behind, if any.
fn add_default_agent(env: &mut EnvironmentInfo) -> Result<(), EnvironmentError> {
    let agent = default_agent(env.agent_session_id.take())
        .map_err(|error| EnvironmentError::Invalid(error.to_string()))?;
    env.agents.push(agent);
    Ok(())
}

/// The deprecated switch off: removes the default agent, and only that. More than one agent, or
/// an agent with a role, is somebody's setup that a switch must not delete: the caller is told to
/// use `environments.agent_delete`.
fn remove_default_agent(env: &mut EnvironmentInfo) -> Result<(), EnvironmentError> {
    if env.agents.is_empty() {
        return Ok(());
    }
    if env.agents.len() > 1 || env.agents.iter().any(|agent| !agent.role.is_empty()) {
        return Err(EnvironmentError::Invalid(format!(
            "{} has {} agent(s) set up by name or role; persistent_agent is deprecated and cannot \
             remove them. Delete them with environments.agent_delete",
            env.name,
            env.agents.len()
        )));
    }
    if env.agents[0].name != DEFAULT_AGENT_NAME {
        return Err(EnvironmentError::Invalid(format!(
            "{} has an agent named {:?}, not the default {DEFAULT_AGENT_NAME:?}; delete it with \
             environments.agent_delete",
            env.name, env.agents[0].name
        )));
    }
    // Its session stays linked, so turning the switch on again continues it.
    env.agent_session_id = env.agents.remove(0).session_id;
    Ok(())
}

fn find_agent<'a>(
    env: &'a mut EnvironmentInfo,
    agent_id: &str,
) -> Result<&'a mut EnvironmentAgent, EnvironmentError> {
    env.agents
        .iter_mut()
        .find(|agent| agent.id == agent_id)
        .ok_or_else(|| EnvironmentError::NotFound(agent_id.to_string()))
}

fn valid_agent_name(name: &str) -> Result<String, EnvironmentError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(EnvironmentError::Invalid(
            "an agent needs a name of 1 to 60 characters".into(),
        ));
    }
    // The name goes into the agent's instructions and titles: one line of ordinary characters.
    if name.chars().any(char::is_control) {
        return Err(EnvironmentError::Invalid(
            "an agent's name cannot contain line breaks or other control characters".into(),
        ));
    }
    Ok(name.to_string())
}

/// A name as compared for uniqueness: trimmed and lower-cased with Unicode rules, as the
/// desktop does.
fn folded(name: &str) -> String {
    name.trim().to_lowercase()
}

fn valid_role(role: &str) -> Result<String, EnvironmentError> {
    let role = role.trim();
    if role.chars().count() > 4000 {
        return Err(EnvironmentError::Invalid(
            "an agent's role is at most 4000 characters".into(),
        ));
    }
    if role
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(EnvironmentError::Invalid(
            "an agent's role cannot contain control characters".into(),
        ));
    }
    Ok(role.to_string())
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
        // Home and the one left.
        assert_eq!(store(data.path()).list().len(), 2);
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
        assert_eq!(store(data.path()).list()[2].pet, None);
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
        assert_eq!(
            envs.list().len(),
            1,
            "only Home: nothing is created when a folder is refused"
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
        assert_eq!(envs.list().len(), 2, "Home and the folder's environment");
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
        let listed = envs.list();
        assert!(listed.len() == 1 && listed[0].builtin, "only Home is left");
        assert!(data.path().join("environments.json.unreadable").exists());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_set_aside_not_replaced_by_home_alone() {
        let data = tempfile::tempdir().unwrap();
        // A folder where the file should be: reading it fails with something other than NotFound.
        let in_the_way = data.path().join("environments.json");
        std::fs::create_dir_all(&in_the_way).unwrap();
        std::fs::write(in_the_way.join("precious.txt"), "kept").unwrap();
        let envs = store(data.path());
        assert_eq!(envs.list().len(), 1, "Home, in memory");
        let aside = data.path().join("environments.json.unreadable");
        assert_eq!(
            std::fs::read_to_string(aside.join("precious.txt")).unwrap(),
            "kept",
            "what was there is kept"
        );
    }

    #[test]
    fn a_folder_given_to_home_still_gets_its_own_environment() {
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        envs.add_folder(HOME_ID, &path(&project)).unwrap();
        let made = envs.for_folder(&path(&project)).unwrap();
        assert!(!made.builtin && made.id != HOME_ID);
        assert_eq!(envs.list().len(), 2);
    }

    #[test]
    fn home_always_exists_once_first_and_with_a_name_safe_for_a_folder() {
        let data = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let listed = envs.list();
        assert_eq!(listed.len(), 1);
        let home = &listed[0];
        assert_eq!((home.id.as_str(), home.name.as_str()), (HOME_ID, "Home"));
        assert!(home.builtin && home.folders.is_empty() && !home.persistent_agent);
        assert!(home.agents.is_empty(), "no agents by default");
        assert!(crate::workspaces::safe_id(&home.id));
        // Restarts keep the one Home, with what the user changed about it.
        envs.update(EnvironmentUpdateParams {
            id: HOME_ID.into(),
            name: Some("My place".into()),
            ..EnvironmentUpdateParams::default()
        })
        .unwrap();
        let again = store(data.path());
        assert_eq!(again.list().len(), 1);
        assert_eq!(again.get(HOME_ID).unwrap().name, "My place");
    }

    #[test]
    fn a_store_from_before_home_gains_it_first_without_losing_anything() {
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let old = store(data.path());
        let work = new(&old, "Work", None, &[path(&folder)]).unwrap();
        // Written as an older version did: no Home, no `builtin` field.
        let mut file: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(data.path().join("environments.json")).unwrap(),
        )
        .unwrap();
        let kept: Vec<_> = file["environments"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|env| env["id"] != HOME_ID)
            .cloned()
            .map(|mut env| {
                env.as_object_mut().unwrap().remove("builtin");
                env
            })
            .collect();
        file["environments"] = serde_json::Value::Array(kept);
        std::fs::write(data.path().join("environments.json"), file.to_string()).unwrap();

        let upgraded = store(data.path());
        let listed = upgraded.list();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, HOME_ID);
        assert_eq!(listed[1], work);
        assert_eq!(store(data.path()).list(), listed, "and it was saved");
    }

    #[test]
    fn home_cannot_be_deleted_or_moved_to_another_runtime() {
        let data = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        assert!(matches!(
            envs.delete(HOME_ID),
            Err(EnvironmentError::Invalid(_))
        ));
        assert!(envs.get(HOME_ID).is_some());
        let moved = envs.update(EnvironmentUpdateParams {
            id: HOME_ID.into(),
            runtime: Some(EnvironmentRuntime::Wsl {
                distro: "Ubuntu".into(),
            }),
            ..EnvironmentUpdateParams::default()
        });
        assert!(matches!(moved, Err(EnvironmentError::Invalid(_))));
        // Folders may be added by the user, who then opens that project to Home.
        let folder = tempfile::tempdir().unwrap();
        let with = envs.add_folder(HOME_ID, &path(&folder)).unwrap();
        assert_eq!(with.folders.len(), 1);
    }

    fn agent_env(envs: &Environments, folder: &tempfile::TempDir) -> EnvironmentInfo {
        new(envs, "Work", None, &[path(folder)]).unwrap()
    }

    #[test]
    fn agent_names_and_roles_are_checked() {
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let env = agent_env(&envs, &folder);
        // Length: 60 characters is the limit, counted as characters, not bytes.
        assert!(envs.add_agent(&env.id, &"é".repeat(60), None).is_ok());
        assert!(envs.add_agent(&env.id, &"é".repeat(61), None).is_err());
        assert!(envs.add_agent(&env.id, "  ", None).is_err());
        // One line of ordinary characters: no line breaks or other control characters.
        for name in ["Plan\nner", "Plan\rner", "Plan\u{7}ner", "Plan\u{1b}[2J"] {
            let error = envs.add_agent(&env.id, name, None).unwrap_err().to_string();
            assert!(error.contains("control characters"), "{name:?}: {error}");
        }
        // A role may have lines, but not other control characters, and is at most 4000.
        assert!(
            envs.add_agent(&env.id, "A", Some("line one\nline two"))
                .is_ok()
        );
        assert!(
            envs.add_agent(&env.id, "B", Some(&"x".repeat(4000)))
                .is_ok()
        );
        assert!(
            envs.add_agent(&env.id, "C", Some(&"x".repeat(4001)))
                .is_err()
        );
        assert!(envs.add_agent(&env.id, "D", Some("bad\u{0}role")).is_err());
    }

    #[test]
    fn agent_names_are_unique_with_unicode_case_folding_and_across_updates() {
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        let env = agent_env(&envs, &folder);
        let planner = envs.add_agent(&env.id, "Éditeur", None).unwrap();
        let other = envs.add_agent(&env.id, "Reviewer", None).unwrap();
        // É and é are one name; so are padded spellings.
        let error = envs.add_agent(&env.id, " éditeur ", None).unwrap_err();
        assert!(error.to_string().contains("already has an agent"));
        // A rename onto another agent's name is refused; onto its own name (any case) is not.
        let error = envs
            .update_agent(&env.id, &other.id, Some("ÉDITEUR"), None)
            .unwrap_err();
        assert!(error.to_string().contains("already has an agent"));
        assert!(
            envs.update_agent(&env.id, &planner.id, Some("éDITEUR"), None)
                .is_ok()
        );
    }

    #[test]
    fn an_agent_of_another_environment_is_not_found() {
        let data = tempfile::tempdir().unwrap();
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let envs = store(data.path());
        let first = agent_env(&envs, &one);
        let second = new(&envs, "Play", None, &[path(&two)]).unwrap();
        let agent = envs.add_agent(&first.id, "Planner", None).unwrap();
        assert!(matches!(
            envs.update_agent(&second.id, &agent.id, Some("X"), None),
            Err(EnvironmentError::NotFound(_))
        ));
        assert!(matches!(
            envs.remove_agent(&second.id, &agent.id),
            Err(EnvironmentError::NotFound(_))
        ));
        assert!(matches!(
            envs.set_agent_session(&second.id, &agent.id, Some("s".into())),
            Err(EnvironmentError::NotFound(_))
        ));
        assert_eq!(envs.get(&first.id).unwrap().agents.len(), 1);
    }

    fn switch(
        envs: &Environments,
        id: &str,
        on: bool,
    ) -> Result<EnvironmentInfo, EnvironmentError> {
        envs.update(EnvironmentUpdateParams {
            id: id.into(),
            persistent_agent: Some(on),
            ..EnvironmentUpdateParams::default()
        })
    }

    #[test]
    fn the_deprecated_switch_adds_one_agent_and_removes_only_the_default() {
        let data = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let envs = store(data.path());
        // Created through the alias: one default agent, and "on" again changes nothing.
        let env = envs
            .create(EnvironmentCreateParams {
                name: "Work".into(),
                folders: vec![path(&folder)],
                persistent_agent: Some(true),
                ..EnvironmentCreateParams::default()
            })
            .unwrap();
        assert_eq!(env.agents.len(), 1);
        assert_eq!(env.agents[0].name, DEFAULT_AGENT_NAME);
        let id = env.agents[0].id.clone();
        assert_eq!(switch(&envs, &env.id, true).unwrap().agents[0].id, id);
        envs.add_agent(&env.id, "Planner", None).unwrap();
        assert_eq!(switch(&envs, &env.id, true).unwrap().agents.len(), 2);
        // Off with two agents is refused and removes nothing.
        let error = switch(&envs, &env.id, false).unwrap_err().to_string();
        assert!(error.contains("agent_delete"), "{error}");
        assert_eq!(envs.get(&env.id).unwrap().agents.len(), 2);
        // Off with one agent that has a role, or a name of its own, is refused too.
        envs.remove_agent(&env.id, &id).unwrap();
        envs.update_agent(
            &env.id,
            &envs.get(&env.id).unwrap().agents[0].id,
            None,
            Some("Plans"),
        )
        .unwrap();
        assert!(switch(&envs, &env.id, false).is_err());
        let keep = envs.get(&env.id).unwrap().agents[0].id.clone();
        envs.update_agent(&env.id, &keep, None, Some("")).unwrap();
        assert!(
            switch(&envs, &env.id, false)
                .unwrap_err()
                .to_string()
                .contains("not the default")
        );
        // The plain default agent goes, and its session stays linked for the next "on".
        envs.remove_agent(&env.id, &keep).unwrap();
        switch(&envs, &env.id, true).unwrap();
        let default = envs.get(&env.id).unwrap().agents[0].id.clone();
        envs.set_agent_session(&env.id, &default, Some("old-session".into()))
            .unwrap();
        let off = switch(&envs, &env.id, false).unwrap();
        assert!(off.agents.is_empty() && !off.persistent_agent);
        assert_eq!(off.agent_session_id.as_deref(), Some("old-session"));
        let on = switch(&envs, &env.id, true).unwrap();
        assert_eq!(on.agents[0].session_id.as_deref(), Some("old-session"));
    }

    fn legacy_file(dir: &Path, persistent: bool, session: Option<&str>) {
        let folder = dir.join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        let env = serde_json::json!({
            "id": "e1", "name": "Old", "folders": [folder], "persistent_agent": persistent,
            "agent_session_id": session,
        });
        std::fs::write(
            dir.join("environments.json"),
            serde_json::json!({ "environments": [env] }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_legacy_persistent_session_migrates_once_and_a_second_load_changes_nothing() {
        let data = tempfile::tempdir().unwrap();
        legacy_file(data.path(), true, Some("sess-1"));
        let first = store(data.path()).list();
        assert_eq!(first[1].agents.len(), 1);
        assert_eq!(first[1].agents[0].session_id.as_deref(), Some("sess-1"));
        let saved = std::fs::read_to_string(data.path().join("environments.json")).unwrap();
        // Loading again finds the agent already there: same id, file untouched.
        let second = store(data.path()).list();
        assert_eq!(second[1].agents, first[1].agents);
        assert_eq!(
            std::fs::read_to_string(data.path().join("environments.json")).unwrap(),
            saved
        );
    }

    #[test]
    fn a_failed_save_during_migration_keeps_the_session_link() {
        let data = tempfile::tempdir().unwrap();
        legacy_file(data.path(), true, Some("sess-1"));
        // The temporary file cannot be written (a folder is in its place): the save fails.
        std::fs::create_dir(data.path().join("environments.json.tmp")).unwrap();
        let envs = store(data.path());
        let agents = envs.list()[1].agents.clone();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].session_id.as_deref(), Some("sess-1"));
        // The file still has the old form, so the next load migrates the same session.
        std::fs::remove_dir(data.path().join("environments.json.tmp")).unwrap();
        let again = store(data.path()).list();
        assert_eq!(again[1].agents[0].session_id.as_deref(), Some("sess-1"));
    }

    #[test]
    fn a_legacy_turned_off_agent_keeps_its_session_link_without_an_agent() {
        let data = tempfile::tempdir().unwrap();
        legacy_file(data.path(), false, Some("sess-old"));
        let env = store(data.path()).list().remove(1);
        assert!(env.agents.is_empty() && !env.persistent_agent);
        assert_eq!(env.agent_session_id.as_deref(), Some("sess-old"));
        // Turning the deprecated switch on continues that session.
        let envs = store(data.path());
        let on = switch(&envs, "e1", true).unwrap();
        assert_eq!(on.agents[0].session_id.as_deref(), Some("sess-old"));
    }
}
