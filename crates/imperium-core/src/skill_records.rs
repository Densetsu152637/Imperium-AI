//! The record of skills an agent installed and the load-time gate for them
//! (docs/06-extensibility.md#the-load-time-gate).
//!
//! `<data>/skill-installs.json` holds one [`Record`] per skill folder an agent installed under
//! `<config>/skills`, keyed by canonical folder path: the skill's name, where it came from, the
//! content hash the daemon saw at install and an optional [`Approval`], the same one a plugin
//! has ([`crate::plugin_records`]). Skills the user put there themselves have no record and no
//! provenance file and load as they always did.
//!
//! [`gate`] runs when a session starts. A skill folder an agent installed (a record with
//! `origin = agent`, or an `.imperium-source.json` naming a session, which is how skills an
//! agent installed before this record existed are recognised) loads only with an approval the
//! verifier honours whose hash equals what is there now. A skill without one is withheld whole
//! and listed with how to approve it. An approved one loads after every project and plugin skill,
//! so it can never shadow one. Nothing here decides an approval: [`record_approval`] writes one
//! only after `app_identity::conclude` ran every check of who may approve.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use imperium_api::encoding::hex;
use imperium_api::time::unix_ms;
use imperium_config::Paths;
use imperium_extensions::plugins;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::plugin_records::{
    self, Approval, ApprovalKind, ApprovalVerifier, Origin, Withheld, is_link,
};

const STORE_FILE: &str = "skill-installs.json";

/// Why a skill an agent installed is withheld before the user approved it.
pub(crate) const UNAPPROVED: &str = "an agent installed it and you have not approved it: approve it in the Imperium desktop app (Plugins, Installed) to load it";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) name: String,
    pub(crate) origin: Origin,
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) commit: Option<String>,
    pub(crate) hash: String,
    #[serde(default)]
    pub(crate) approval: Option<Approval>,
}

/// The records, by canonical folder path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Store {
    pub(crate) skills: BTreeMap<String, Record>,
}

/// What the gate decided for the skill folders under `<config>/skills`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Gate {
    /// Agent-installed skills the user has not approved (or whose content changed), and why.
    pub(crate) withheld: Vec<Withheld>,
    /// Agent-installed skills the user approved: they load after project and plugin skills.
    pub(crate) agent: Vec<PathBuf>,
}

fn store_path(paths: &Paths) -> PathBuf {
    paths.data_dir().join(STORE_FILE)
}

fn skills_dir(paths: &Paths) -> PathBuf {
    paths.config_dir().join("skills")
}

/// The records. An unreadable store is an error: the gate then treats it as empty (every
/// agent-installed skill is withheld) and nothing overwrites it.
fn read_store(paths: &Paths) -> Result<Store, String> {
    match std::fs::read_to_string(store_path(paths)) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| {
            format!(
                "the skill record file is unreadable ({error}); fix or delete {} first",
                store_path(paths).display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(error) => Err(format!("{}: {error}", store_path(paths).display())),
    }
}

/// Serialises read-modify-write of the record file.
static STORE_LOCK: Mutex<()> = Mutex::new(());

fn add_record(paths: &Paths, key: String, record: Record) -> Result<(), String> {
    let _guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = read_store(paths)?;
    store.skills.insert(key, record);
    plugin_records::write_json(&store_path(paths), &store)
}

/// The record of the skill folder `folder`, if there is a readable one.
pub(crate) fn record_for(paths: &Paths, folder: &Path) -> Option<Record> {
    let store = read_store(paths).ok()?;
    let canon = folder.canonicalize().ok()?;
    store.skills.get(&*canon.to_string_lossy()).cloned()
}

/// The digest an approval of a skill is bound to in place of a plugin's run summary: a skill
/// runs nothing, so it is one constant, and the content hash carries the approval.
pub(crate) fn runs_digest() -> String {
    hex(&Sha256::digest(b"imperium skill: no hooks or servers"))
}

/// Records the skill an agent installed into `folder` (`origin = agent`, no approval: an agent
/// cannot approve) with the hash the gate will compute.
pub(crate) fn record_install(
    paths: &Paths,
    folder: &Path,
    name: &str,
    source: Option<String>,
    commit: Option<String>,
) -> Result<(), String> {
    let seen = plugin_records::measure(folder)?;
    if let Some(link) = seen.link {
        return Err(format!(
            "the skill holds a link at {link}, which is not installed"
        ));
    }
    let key =
        plugin_records::key_of(folder).ok_or_else(|| format!("{}: not found", folder.display()))?;
    add_record(
        paths,
        key,
        Record {
            name: name.to_string(),
            origin: Origin::Agent,
            source,
            commit,
            hash: seen.hash,
            approval: None,
        },
    )
}

/// The skill folders directly under `<config>/skills`.
fn skill_folders(paths: &Paths) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(skills_dir(paths))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("SKILL.md").is_file())
        .collect();
    folders.sort();
    folders
}

/// The load-time gate for the skills under `<config>/skills` (module documentation).
pub(crate) fn gate(paths: &Paths, verifier: &dyn ApprovalVerifier) -> Gate {
    let store = read_store(paths).unwrap_or_else(|error| {
        tracing::warn!(%error, "the skill record file is unreadable; agent-installed skills are withheld");
        Store::default()
    });
    let mut gate = Gate::default();
    for folder in skill_folders(paths) {
        let canon = folder.canonicalize().ok();
        let record = canon
            .as_ref()
            .and_then(|canon| store.skills.get(&*canon.to_string_lossy()));
        // The user's own skills have neither a record nor a provenance file naming a session.
        let by_agent = record.is_some_and(|record| record.origin == Origin::Agent)
            || plugins::agent_installed(&folder);
        if !by_agent {
            continue;
        }
        match judge(&folder, canon.as_deref(), record, verifier) {
            Some(reason) => gate.withheld.push(Withheld { folder, reason }),
            None => gate.agent.push(folder),
        }
    }
    gate
}

fn judge(
    folder: &Path,
    canon: Option<&Path>,
    record: Option<&Record>,
    verifier: &dyn ApprovalVerifier,
) -> Option<String> {
    let Some(canon) = canon else {
        return Some("its folder cannot be resolved".to_string());
    };
    if std::fs::symlink_metadata(folder).is_ok_and(|meta| is_link(&meta)) {
        return Some("it is a symbolic link or junction".to_string());
    }
    let seen = match plugin_records::look(canon) {
        Ok(seen) => seen,
        Err(error) => return Some(format!("it cannot be read: {error}")),
    };
    if let Some(link) = &seen.link {
        return Some(format!("it contains a symbolic link or junction ({link})"));
    }
    let Some(record) = record else {
        return Some(UNAPPROVED.to_string());
    };
    if seen.hash != record.hash {
        return Some("its content changed since Imperium recorded it".to_string());
    }
    match &record.approval {
        None => Some(UNAPPROVED.to_string()),
        Some(approval) if !verifier.honours(approval.kind) => {
            Some("its approval does not count here".to_string())
        }
        Some(approval) if approval.hash != seen.hash => {
            Some("it changed since it was approved: approve it again to load".to_string())
        }
        Some(_) => None,
    }
}

/// The skill folder `folder`, canonical, with its content hash, the constant [`runs_digest`] and
/// its name, as they are now. A folder that is or holds a link has no hash to approve.
pub(crate) fn content(folder: &Path) -> Result<(PathBuf, String, String, String), String> {
    if std::fs::symlink_metadata(folder).is_ok_and(|meta| is_link(&meta)) {
        return Err(format!(
            "{} is a link or junction: it is not approved",
            folder.display()
        ));
    }
    let canon = folder
        .canonicalize()
        .map_err(|error| format!("{}: {error}", folder.display()))?;
    let seen = plugin_records::look(&canon)?;
    if let Some(link) = &seen.link {
        return Err(format!(
            "the skill holds a link at {link}: it is not approved"
        ));
    }
    let text = std::fs::read_to_string(canon.join("SKILL.md"))
        .map_err(|error| format!("the folder holds no skill that can be read: {error}"))?;
    let (fields, _) = imperium_api::markdown::front_matter(&text);
    let name = fields.get("name").cloned().unwrap_or_else(|| {
        canon
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    Ok((canon, seen.hash, runs_digest(), name))
}

/// The folder of `<config>/skills` named exactly `name`, canonical: one plain folder name, not a
/// link, resolving inside `<config>/skills`.
pub(crate) fn exact_folder(paths: &Paths, name: &str) -> Result<PathBuf, String> {
    crate::plugin_install::check_folder_name(name)?;
    let dir = skills_dir(paths);
    let missing = || format!("no skill folder named {name:?} is installed");
    let root = dir.canonicalize().map_err(|_| missing())?;
    let entry = std::fs::read_dir(&dir)
        .map_err(|_| missing())?
        .flatten()
        .find(|entry| entry.file_name().to_str() == Some(name))
        .ok_or_else(missing)?;
    let path = entry.path();
    let meta = std::fs::symlink_metadata(&path).map_err(|_| missing())?;
    if is_link(&meta) {
        return Err(format!(
            "{name:?} is a link: a skill folder that is a link is not inspected or approved"
        ));
    }
    if !meta.is_dir() {
        return Err(missing());
    }
    let canon = path.canonicalize().map_err(|_| missing())?;
    if canon.parent() != Some(root.as_path()) {
        return Err(format!(
            "{name:?} does not resolve to a folder of {}",
            dir.display()
        ));
    }
    Ok(canon)
}

/// Records an approval of `kind` for the skill folder `folder`, if `verifier` lets that kind be
/// written. The folder's hash must equal `hash` and `runs` the constant digest (what the approval
/// was asked for). A folder with no record gets one (`origin = agent` when its provenance file
/// names a session, else `user`). Nothing here decides who may approve:
/// `app_identity::conclude` does before it calls this.
pub(crate) fn record_approval(
    paths: &Paths,
    verifier: &dyn ApprovalVerifier,
    folder: &Path,
    kind: ApprovalKind,
    hash: &str,
    runs: &str,
) -> Result<(), String> {
    if !verifier.may_write(kind) {
        return Err(format!(
            "this build does not record {} approvals: nothing was recorded",
            kind.as_str()
        ));
    }
    let (canon, now_hash, _, name) = content(folder)?;
    if now_hash != hash || runs != runs_digest() {
        return Err(
            "the skill changed since the approval was asked for: nothing was recorded".into(),
        );
    }
    let key = canon.to_string_lossy().into_owned();
    let mut record = record_for(paths, &canon).unwrap_or_else(|| Record {
        name,
        origin: if plugins::agent_installed(&canon) {
            Origin::Agent
        } else {
            Origin::User
        },
        source: None,
        commit: None,
        hash: hash.to_string(),
        approval: None,
    });
    record.hash = hash.to_string();
    record.approval = Some(Approval {
        hash: hash.to_string(),
        runs: runs.to_string(),
        at_unix_ms: unix_ms(),
        kind,
    });
    add_record(paths, key, record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_records::TestVerifier;

    struct Home {
        _dir: tempfile::TempDir,
        paths: Paths,
    }

    fn home() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        Home { _dir: dir, paths }
    }

    fn skill(home: &Home, folder: &str, by_agent: bool) -> PathBuf {
        let root = skills_dir(&home.paths).join(folder);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {folder}\ndescription: a skill\n---\nbody\n"),
        )
        .unwrap();
        if by_agent {
            std::fs::write(root.join(plugins::SOURCE_FILE), r#"{"session_id": "s1"}"#).unwrap();
        }
        root
    }

    #[test]
    fn a_users_own_skill_loads_and_an_agent_installed_one_waits_for_approval() {
        let home = home();
        skill(&home, "mine", false);
        let theirs = skill(&home, "theirs", true);
        // Installed before the record existed (provenance file only): withheld.
        let gate_now = gate(&home.paths, &TestVerifier);
        assert_eq!(gate_now.withheld.len(), 1);
        assert_eq!(gate_now.withheld[0].reason, UNAPPROVED);
        assert!(gate_now.agent.is_empty());
        // Installed through the tool: a record with no approval is withheld too.
        record_install(
            &home.paths,
            &theirs,
            "theirs",
            Some("https://x/y".into()),
            None,
        )
        .unwrap();
        assert_eq!(gate(&home.paths, &TestVerifier).withheld.len(), 1);
        // Approved: it loads, ranked as an agent's skill.
        let (_, hash, runs, name) = content(&theirs).unwrap();
        assert_eq!(name, "theirs");
        record_approval(
            &home.paths,
            &TestVerifier,
            &theirs,
            ApprovalKind::Counted,
            &hash,
            &runs,
        )
        .unwrap();
        let gate_now = gate(&home.paths, &TestVerifier);
        assert!(gate_now.withheld.is_empty(), "{:?}", gate_now.withheld);
        assert_eq!(gate_now.agent, [theirs.clone()]);
        // An approval of other content is refused, and a change after the approval withholds it.
        let error = record_approval(
            &home.paths,
            &TestVerifier,
            &theirs,
            ApprovalKind::Counted,
            "0000",
            &runs,
        )
        .unwrap_err();
        assert!(error.contains("changed since"), "{error}");
        std::fs::write(
            theirs.join("SKILL.md"),
            "---\nname: theirs\ndescription: a skill\n---\nedited\n",
        )
        .unwrap();
        assert_eq!(gate(&home.paths, &TestVerifier).withheld.len(), 1);
    }

    #[test]
    fn the_folder_an_approval_names_is_one_plain_skill_folder() {
        let home = home();
        skill(&home, "pdf", true);
        assert!(exact_folder(&home.paths, "pdf").is_ok());
        assert!(exact_folder(&home.paths, "PDF-missing").is_err());
        assert!(exact_folder(&home.paths, "..").is_err());
        assert!(exact_folder(&home.paths, "a/b").is_err());
    }
}
