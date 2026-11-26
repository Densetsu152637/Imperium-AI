//! The config change journal: every change an agent makes to the user's app config (a model
//! card written, a mod, plugin, or skill installed or removed) is appended to
//! `<data>/config-changes.json`, which `config.changes` lists and `config.undo` reverses.

use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use imperium_api::time::unix_ms;
use imperium_protocol::methods::{ConfigChange, ConfigUndoResult};
use serde::{Deserialize, Serialize};

use crate::extension_install::{
    self, ExtensionKind, InstallRequest, move_tree, read_source, remove_tree,
};
use crate::model_config::{self, CardKind};
use crate::plugin_records;
use crate::state::DaemonState;

/// Most changes kept; older ones are dropped (and can no longer be undone).
const KEEP: usize = 1000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Journal {
    next_id: u64,
    changes: Vec<ConfigChange>,
}

fn journal_path(state: &DaemonState) -> PathBuf {
    state.paths().data_dir().join("config-changes.json")
}

fn read(path: &Path) -> Journal {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "the config change journal is unreadable; starting a new one");
            Journal::default()
        }),
        Err(_) => Journal::default(),
    }
}

fn write(path: &Path, journal: &Journal) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(journal).map_err(|error| error.to_string())?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path)
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

/// A change with only what every kind has filled in.
fn blank(kind: &str, session_id: Option<&str>, summary: String) -> ConfigChange {
    ConfigChange {
        id: 0,
        at_unix_ms: unix_ms(),
        session_id: session_id.map(str::to_string),
        kind: kind.into(),
        summary,
        model: None,
        model_kind: None,
        before: None,
        after: None,
        extension_kind: None,
        name: None,
        path: None,
        source: None,
        disabled: false,
        kept_at: None,
        sha256: None,
        record: None,
        undone_at_unix_ms: None,
    }
}

/// Appends `change` with the next id and returns it as recorded.
fn record(state: &DaemonState, mut change: ConfigChange) -> ConfigChange {
    let _journal = state
        .config_journal
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let path = journal_path(state);
    let mut journal = read(&path);
    journal.next_id = journal.next_id.max(1);
    change.id = journal.next_id;
    journal.next_id += 1;
    journal.changes.push(change.clone());
    if journal.changes.len() > KEEP {
        let drop = journal.changes.len() - KEEP;
        journal.changes.drain(..drop);
    }
    if let Err(error) = write(&path, &journal) {
        tracing::warn!(%error, "could not record a config change");
    }
    tracing::info!(id = change.id, summary = %change.summary, "an agent changed the app config");
    change
}

/// The recorded changes, newest first.
pub(crate) fn list(state: &DaemonState) -> Vec<ConfigChange> {
    let _journal = state
        .config_journal
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut changes = read(&journal_path(state)).changes;
    changes.reverse();
    changes
}

/// `set_model_card`: writes the card and records the change.
pub(crate) fn set_model_card(
    state: &DaemonState,
    session_id: Option<&str>,
    model: &str,
    card: Option<&str>,
) -> Result<ConfigChange, String> {
    let (id, kind) = model_config::card_target(state, model)?;
    let written = model_config::set_card(state, &id, kind, card, None)?;
    let summary = match &written.after {
        Some(_) => format!("Set the model card of {id}"),
        None => format!("Cleared the model card of {id}"),
    };
    let mut change = blank("model_card", session_id, summary);
    change.model = Some(written.id);
    change.model_kind = Some(kind.as_str().into());
    change.before = written.before;
    change.after = written.after;
    Ok(record(state, change))
}

/// `install_extension`: installs and records the install.
pub(crate) async fn install(
    state: &DaemonState,
    request: InstallRequest,
) -> Result<(ConfigChange, Vec<String>), String> {
    let session_id = request.session_id.clone();
    let installed = extension_install::install(state, request).await?;
    Ok(record_install(
        state,
        session_id.as_deref(),
        installed,
        None,
    ))
}

/// `install_extension` of a marketplace plugin (`name@marketplace`): installs it with
/// `origin = agent` and records the install.
pub(crate) async fn install_from_marketplace(
    state: &DaemonState,
    session_id: Option<&str>,
    id: &str,
    revision: Option<&str>,
) -> Result<(ConfigChange, Vec<String>), String> {
    let installed =
        crate::plugin_install::install_for_agent(state, id, revision, session_id).await?;
    Ok(record_install(state, session_id, installed, Some(id)))
}

fn record_install(
    state: &DaemonState,
    session_id: Option<&str>,
    installed: extension_install::Installed,
    id: Option<&str>,
) -> (ConfigChange, Vec<String>) {
    let source = &installed.source;
    let from = match &source.revision {
        Some(revision) => format!("{} at {}", source.location, short(revision)),
        None => source.location.clone(),
    };
    let mut change = blank(
        "install",
        session_id,
        format!(
            "Installed the {} {} from {from}",
            installed.kind.as_str(),
            id.unwrap_or(&installed.name)
        ),
    );
    change.extension_kind = Some(installed.kind.as_str().into());
    change.name = Some(installed.name.clone());
    change.path = Some(installed.path.display().to_string());
    change.source = Some(installed.source.clone());
    change.disabled = installed.disabled;
    change.sha256 = installed.sha256.clone();
    (record(state, change), installed.notes)
}

/// A marketplace plugin installed by `plugins.install`: records the install. The folder carries
/// the same source, so `config.undo` removes it.
pub(crate) fn plugin_installed(
    state: &DaemonState,
    id: &str,
    path: &Path,
    source: &imperium_protocol::methods::InstallSource,
    sha256: &str,
) -> ConfigChange {
    let mut change = blank("install", None, format!("Installed the plugin {id}"));
    change.sha256 = Some(sha256.to_string());
    change.extension_kind = Some("plugin".into());
    change.name = id.rsplit_once('@').map(|(name, _)| name.to_string());
    change.path = Some(path.display().to_string());
    change.source = Some(source.clone());
    record(state, change)
}

fn short(revision: &str) -> &str {
    revision.get(..12).unwrap_or(revision)
}

/// `uninstall_extension`: moves the extension's folder aside and records it.
pub(crate) fn uninstall(
    state: &DaemonState,
    session_id: Option<&str>,
    kind: ExtensionKind,
    name: &str,
) -> Result<ConfigChange, String> {
    let dir = extension_install::find_installed(state.paths().config_dir(), kind, name)?;
    uninstall_folder(state, session_id, kind, name, &dir).map(|(change, _)| change)
}

/// Moves the extension folder `dir` aside and records it. For a plugin its record is removed too
/// (kept in the journal entry with the folder's hash, so `config.undo` restores both).
pub(crate) fn uninstall_folder(
    state: &DaemonState,
    session_id: Option<&str>,
    kind: ExtensionKind,
    name: &str,
    dir: &Path,
) -> Result<(ConfigChange, Vec<String>), String> {
    let mut notes = Vec::new();
    let dir = dir.to_path_buf();
    let source = read_source(&dir);
    // The record and the hash are read before the folder moves (its key is its canonical path).
    let mut plugin = None;
    if kind == ExtensionKind::Plugin {
        let key = plugin_records::key_of(&dir);
        let record = plugin_records::record_for(state.paths(), &dir);
        let hash = dir
            .canonicalize()
            .ok()
            .and_then(|canon| plugin_records::look(&canon).ok())
            .map(|seen| seen.hash)
            .filter(|hash| !hash.is_empty());
        plugin = Some((key, record, hash));
    }
    let skill_key = (kind == ExtensionKind::Skill)
        .then(|| plugin_records::key_of(&dir))
        .flatten();
    let folder = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let trash = extension_install::trash_dir(state);
    std::fs::create_dir_all(&trash)
        .map_err(|error| format!("could not create {}: {error}", trash.display()))?;
    let kept = trash.join(format!("{}-{}-{folder}", unix_ms(), kind.as_str()));
    move_tree(&dir, &kept)?;
    if let Some(key) = &skill_key
        && let Err(error) = crate::skill_records::take_record(state.paths(), key)
    {
        tracing::warn!(%error, "could not remove the record of an uninstalled skill");
    }
    let mut change = blank(
        "uninstall",
        session_id,
        format!("Removed the {} {name}", kind.as_str()),
    );
    change.extension_kind = Some(kind.as_str().into());
    change.name = Some(name.to_string());
    change.path = Some(dir.display().to_string());
    change.source = source;
    change.kept_at = Some(kept.display().to_string());
    if let Some((key, found, hash)) = plugin {
        change.sha256 = hash;
        if let Some(found) = found {
            change.record = serde_json::to_string(&found).ok();
        }
        if let Some(key) = key
            && let Err(error) = plugin_records::take_record(state.paths(), &key)
        {
            tracing::warn!(%error, "could not remove the record of an uninstalled plugin");
            notes.push(format!(
                "the plugin's record could not be removed ({error}); the folder was moved anyway"
            ));
        }
    }
    Ok((record(state, change), notes))
}

/// Whether the `kind` called `name` was installed by an agent (its folder records a session).
pub(crate) fn installed_by_agent(state: &DaemonState, kind: ExtensionKind, name: &str) -> bool {
    extension_install::find_installed(state.paths().config_dir(), kind, name)
        .ok()
        .and_then(|dir| read_source(&dir))
        .is_some_and(|source| source.session_id.is_some())
}

/// `config.undo`: reverses change `id` and marks it undone.
pub(crate) fn undo(state: &DaemonState, id: u64) -> Result<ConfigUndoResult, String> {
    let change = {
        let _journal = state
            .config_journal
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        read(&journal_path(state))
            .changes
            .into_iter()
            .find(|change| change.id == id)
            .ok_or_else(|| format!("no config change {id}"))?
    };
    if change.undone_at_unix_ms.is_some() {
        return Err(format!("change {id} was already undone"));
    }
    let message = match change.kind.as_str() {
        "model_card" => undo_card(state, &change)?,
        "install" => undo_install(state, &change)?,
        "uninstall" => undo_uninstall(state, &change)?,
        other => return Err(format!("change {id} is a {other}, which cannot be undone")),
    };
    let _journal = state
        .config_journal
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let path = journal_path(state);
    let mut journal = read(&path);
    let mut undone = change;
    undone.undone_at_unix_ms = Some(unix_ms());
    if let Some(entry) = journal.changes.iter_mut().find(|entry| entry.id == id) {
        *entry = undone.clone();
    }
    write(&path, &journal)?;
    tracing::info!(id, %message, "a config change was undone");
    Ok(ConfigUndoResult {
        change: undone,
        message,
    })
}

fn undo_card(state: &DaemonState, change: &ConfigChange) -> Result<String, String> {
    let model = change.model.as_deref().ok_or("the change names no model")?;
    let kind = change
        .model_kind
        .as_deref()
        .and_then(CardKind::parse)
        .ok_or("the change names no card location")?;
    model_config::set_card(
        state,
        model,
        kind,
        change.before.as_deref(),
        Some(change.after.as_deref()),
    )?;
    Ok(match &change.before {
        Some(_) => format!("The model card of {model} is back to what it was."),
        None => format!("The model card {model} was given is removed."),
    })
}

fn undo_install(state: &DaemonState, change: &ConfigChange) -> Result<String, String> {
    let path = PathBuf::from(change.path.as_deref().ok_or("the change names no folder")?);
    let name = change.name.as_deref().unwrap_or_default();
    let kind = ExtensionKind::parse(change.extension_kind.as_deref().unwrap_or_default())?;
    // Only the folder this install made: its recorded source must still be the one installed.
    let installed = read_source(&path);
    if installed.is_none() || installed != change.source {
        return Err(format!(
            "{} is no longer the {} that was installed, so it was left alone",
            path.display(),
            kind.as_str()
        ));
    }
    if path
        .parent()
        .is_none_or(|parent| parent != kind.dir(state.paths().config_dir()))
    {
        return Err(format!("{} is outside the config folder", path.display()));
    }
    // A plugin goes only if its content is still what was installed, and its record goes with it.
    let key = (kind == ExtensionKind::Plugin)
        .then(|| plugin_records::key_of(&path))
        .flatten();
    if kind == ExtensionKind::Plugin
        && let (Some(expected), Some(canon)) = (&change.sha256, path.canonicalize().ok())
    {
        let seen = plugin_records::look(&canon)?;
        if seen.link.is_some() || seen.hash != *expected {
            return Err(format!(
                "{} was changed or replaced since it was installed, so it and its record were left alone: uninstall it instead",
                path.display()
            ));
        }
    }
    let skill_key = (kind == ExtensionKind::Skill)
        .then(|| plugin_records::key_of(&path))
        .flatten();
    remove_tree(&path);
    if path.exists() {
        return Err(format!("could not remove {}", path.display()));
    }
    if let Some(key) = skill_key
        && let Err(error) = crate::skill_records::take_record(state.paths(), &key)
    {
        tracing::warn!(%error, "could not remove the record of an uninstalled skill");
    }
    if let Some(key) = key
        && let Err(error) = plugin_records::take_record(state.paths(), &key)
    {
        tracing::warn!(%error, "could not remove the record of an uninstalled plugin");
    }
    if change.disabled
        && let Err(error) = extension_install::set_plugin_disabled(state, name, false)
    {
        tracing::warn!(%error, "could not take an uninstalled plugin off extensions.disabled_plugins");
    }
    Ok(format!("Removed the {} {name}.", kind.as_str()))
}

fn undo_uninstall(state: &DaemonState, change: &ConfigChange) -> Result<String, String> {
    let path = PathBuf::from(change.path.as_deref().ok_or("the change names no folder")?);
    let kept = PathBuf::from(
        change
            .kept_at
            .as_deref()
            .ok_or("the removed folder was not kept")?,
    );
    if !kept.is_dir() {
        return Err(format!(
            "the removed folder is gone from {}",
            kept.display()
        ));
    }
    if path.exists() {
        return Err(format!(
            "{} exists again; remove it first to restore the old one",
            path.display()
        ));
    }
    // Folder names that differ only by letter case are one folder on Windows and macOS.
    if let (Some(parent), Some(wanted)) = (path.parent(), path.file_name()) {
        let wanted = wanted.to_string_lossy();
        for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
            let existing = entry.file_name().to_string_lossy().into_owned();
            if existing.eq_ignore_ascii_case(&wanted) {
                return Err(format!(
                    "{} exists and differs from {wanted} only by letter case; remove it first to restore the old one",
                    entry.path().display()
                ));
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    // A plugin comes back only as it was removed: the kept copy must still hash as journalled.
    let is_plugin = change.extension_kind.as_deref() == Some("plugin");
    if is_plugin && let (Some(expected), Ok(canon)) = (&change.sha256, kept.canonicalize()) {
        let seen = plugin_records::look(&canon)?;
        if seen.link.is_some() || seen.hash != *expected {
            return Err(format!(
                "the kept copy in {} changed since it was removed, so it was not restored",
                kept.display()
            ));
        }
    }
    move_tree(&kept, &path)?;
    if is_plugin && let Some(text) = &change.record {
        let restored = serde_json::from_str::<plugin_records::Record>(text)
            .map_err(|error| format!("the journalled plugin record is unreadable: {error}"))
            .and_then(|found| {
                let key = plugin_records::key_of(&path)
                    .ok_or_else(|| format!("{}: not found", path.display()))?;
                plugin_records::add_record_at(state.paths(), key, found)
            });
        if let Err(error) = restored {
            // The folder goes back where it was kept, so no folder is left without its record.
            if let Err(back) = move_tree(&path, &kept) {
                tracing::warn!(%back, "could not put a plugin folder back in the trash");
            }
            return Err(format!("could not restore the plugin record: {error}"));
        }
    }
    Ok(format!(
        "Restored the {} {}.",
        change.extension_kind.as_deref().unwrap_or("extension"),
        change.name.as_deref().unwrap_or_default()
    ))
}
