//! The record of installed plugins and the load-time gate (docs/06-extensibility.md#the-load-time-gate,
//! [plan](../../../docs/plans/plugin-marketplace.md#the-load-time-gate)).
//!
//! `<data>/plugin-installs.json` holds one [`Record`] per plugin folder under
//! `<config>/plugins`, keyed by canonical folder path: the plugin's name, where it came from, the
//! content hash and run-summary digest the daemon last saw, and an optional [`Approval`].
//! `<data>/plugin-migration.json` is a separate marker written once, after the first-start
//! migration ([`migrate`]).
//!
//! [`gate`] runs when a session starts. It names every folder to withhold (the whole plugin,
//! whatever `extensions.disabled_plugins` says) and why, and the folders whose content was
//! downloaded. It only reads: no hook, server or build step runs. An unreadable store is
//! treated as empty, so every folder with code is withheld until approved; a damaged store is
//! never overwritten.
//!
//! No approval is created here. Nothing counts a new approval until stage 8c, so the only
//! approvals the gate meets are the carried-over ones [`migrate`] writes (and, in tests, records
//! written directly). [`ApprovalVerifier`] says which stored kinds the gate honours.
//!
//! The content hash is SHA-256 over each file's relative path, length and bytes in path order
//! (as `mod_install::tree_sha256`), leaving out `.git` and Imperium's own `.imperium-*.json` at
//! the folder's top. It is cached in memory per folder with its key, the sorted list of every
//! file's relative path, size and modification time; a walk (stat only) that gives another list
//! rehashes the folder.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use imperium_api::encoding::hex;
use imperium_api::time::unix_ms;
use imperium_config::Paths;
use imperium_extensions::plugins;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const STORE_FILE: &str = "plugin-installs.json";
const MARKER_FILE: &str = "plugin-migration.json";

/// Where a plugin came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Origin {
    Marketplace,
    Agent,
    User,
    Preexisting,
}

impl Origin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Origin::Marketplace => "marketplace",
            Origin::Agent => "agent",
            Origin::User => "user",
            Origin::Preexisting => "preexisting",
        }
    }

    /// Downloaded content never expands the daemon's `${VAR}` and its hooks never rewrite.
    fn downloaded(self) -> bool {
        matches!(self, Origin::Marketplace | Origin::Agent)
    }
}

/// How an approval came about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ApprovalKind {
    Counted,
    DialogOnly,
    CarriedOver,
    /// For development builds only.
    Dev,
}

impl ApprovalKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ApprovalKind::Counted => "counted",
            ApprovalKind::DialogOnly => "dialog-only",
            ApprovalKind::CarriedOver => "carried-over",
            ApprovalKind::Dev => "dev",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Approval {
    /// The content hash that was approved.
    pub(crate) hash: String,
    /// The digest of the run summary (`plugins::runs`) that was approved.
    pub(crate) runs: String,
    pub(crate) at_unix_ms: u64,
    pub(crate) kind: ApprovalKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub(crate) name: String,
    pub(crate) origin: Origin,
    /// `name@marketplace` for a marketplace install.
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) commit: Option<String>,
    pub(crate) hash: String,
    /// The digest of the run summary when the record was written.
    pub(crate) runs: String,
    #[serde(default)]
    pub(crate) approval: Option<Approval>,
}

/// The records, by canonical folder path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Store {
    pub(crate) plugins: BTreeMap<String, Record>,
}

/// Decides which stored approvals the gate honours.
pub(crate) trait ApprovalVerifier: Send + Sync {
    fn honours(&self, kind: ApprovalKind) -> bool;
    /// Whether [`record_approval`] may write an approval of `kind`. The production verifier
    /// writes nothing until stage 8c, so no caller can make an approval count before then.
    fn may_write(&self, kind: ApprovalKind) -> bool;
}

/// A release build: counted, dialog-only and carried-over approvals; never `dev`. Nothing writes
/// a counted or dialog-only approval until stage 8c.
pub(crate) struct ReleaseVerifier;

impl ApprovalVerifier for ReleaseVerifier {
    fn honours(&self, kind: ApprovalKind) -> bool {
        kind != ApprovalKind::Dev
    }

    fn may_write(&self, _: ApprovalKind) -> bool {
        false
    }
}

/// A debug build: also honours `dev` approvals.
pub(crate) struct DebugVerifier;

impl ApprovalVerifier for DebugVerifier {
    fn honours(&self, _: ApprovalKind) -> bool {
        true
    }

    /// Only the development approval, which release daemons ignore.
    fn may_write(&self, kind: ApprovalKind) -> bool {
        kind == ApprovalKind::Dev
    }
}

/// A verifier for tests: honours and writes every kind.
#[cfg(test)]
pub(crate) struct TestVerifier;

#[cfg(test)]
impl ApprovalVerifier for TestVerifier {
    fn honours(&self, _: ApprovalKind) -> bool {
        true
    }

    fn may_write(&self, _: ApprovalKind) -> bool {
        true
    }
}

pub(crate) fn production_verifier() -> &'static dyn ApprovalVerifier {
    if cfg!(debug_assertions) {
        &DebugVerifier
    } else {
        &ReleaseVerifier
    }
}

fn store_path(paths: &Paths) -> PathBuf {
    paths.data_dir().join(STORE_FILE)
}

fn marker_path(paths: &Paths) -> PathBuf {
    paths.data_dir().join(MARKER_FILE)
}

fn plugins_dir(paths: &Paths) -> PathBuf {
    paths.config_dir().join("plugins")
}

enum Stored {
    Missing,
    Unreadable(String),
    Found(Store),
}

fn read_store(paths: &Paths) -> Stored {
    match std::fs::read_to_string(store_path(paths)) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(store) => Stored::Found(store),
            Err(error) => Stored::Unreadable(error.to_string()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stored::Missing,
        Err(error) => Stored::Unreadable(error.to_string()),
    }
}

/// Writes through a temporary file in the same folder and a rename, so a crash leaves the old
/// file or the new one.
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    use std::io::Write;

    let fail = |error: std::io::Error| format!("{}: {error}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(fail)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(fail)?;
    file.write_all(text.as_bytes()).map_err(fail)?;
    file.sync_all().map_err(fail)?;
    drop(file);
    std::fs::rename(&temporary, path).map_err(fail)
}

/// One file's place in a folder: relative path (with `/`), size, modification time.
type Stamp = (String, u64, Option<SystemTime>);

/// What [`look`] found in a folder.
pub(crate) struct Seen {
    /// The content hash; empty when a link was found (the folder is not hashed then).
    pub(crate) hash: String,
    /// The first symbolic link, junction or other reparse point, as a relative path.
    pub(crate) link: Option<String>,
}

/// Hashes by canonical folder path: the walk's stamps that made the hash, and the hash.
static HASHES: Mutex<BTreeMap<PathBuf, (Vec<Stamp>, String)>> = Mutex::new(BTreeMap::new());

pub(crate) fn is_link(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink() || reparse_point(meta)
}

#[cfg(windows)]
fn reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// Whether `name` at the folder's top is left out of the hash: `.git`, and the one file Imperium
/// writes into a plugin folder. Any other file, `.imperium-*.json` ones a plugin ships included,
/// is content (a hook can run it).
fn excluded(name: &str) -> bool {
    name == ".git" || name == plugins::SOURCE_FILE
}

/// Lists every file under `root` without following links. Stops at the first link found.
fn walk(root: &Path, prefix: &str, stamps: &mut Vec<Stamp>) -> Result<Option<String>, String> {
    let folder = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(prefix)
    };
    let fail = |error: std::io::Error| format!("{}: {error}", folder.display());
    for entry in std::fs::read_dir(&folder).map_err(fail)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let meta = std::fs::symlink_metadata(entry.path())
            .map_err(|error| format!("{relative}: {error}"))?;
        // The link check comes before any exclusion: a link is refused whatever its name.
        if is_link(&meta) {
            return Ok(Some(relative));
        }
        if prefix.is_empty() && excluded(&relative) {
            continue;
        }
        if meta.is_dir() {
            if let Some(link) = walk(root, &relative, stamps)? {
                return Ok(Some(link));
            }
        } else if meta.is_file() {
            stamps.push((relative, meta.len(), meta.modified().ok()));
        }
    }
    Ok(None)
}

/// The hash of the files `stamps` lists, each read once; a file whose length changed while it
/// was read is an error.
fn hash_files(root: &Path, stamps: &[Stamp]) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    for (relative, _, _) in stamps {
        let fail = |error: std::io::Error| format!("{relative}: {error}");
        let mut file = std::fs::File::open(root.join(relative)).map_err(fail)?;
        let length = file.metadata().map_err(fail)?.len();
        digest.update(relative.as_bytes());
        digest.update([0u8]);
        digest.update(length.to_le_bytes());
        let mut seen = 0u64;
        loop {
            let count = file.read(&mut buffer).map_err(fail)?;
            if count == 0 {
                break;
            }
            seen += u64::try_from(count).unwrap_or(u64::MAX);
            digest.update(&buffer[..count]);
        }
        if seen != length {
            return Err(format!("{relative} changed while it was read"));
        }
    }
    Ok(hex(&digest.finalize()))
}

/// The content hash of the folder `root` (and whether it holds a link), not cached: for a folder
/// that is not installed yet (an inspected plugin in a staging folder).
pub(crate) fn measure(root: &Path) -> Result<Seen, String> {
    let mut stamps = Vec::new();
    if let Some(link) = walk(root, "", &mut stamps)? {
        return Ok(Seen {
            hash: String::new(),
            link: Some(link),
        });
    }
    stamps.sort();
    Ok(Seen {
        hash: hash_files(root, &stamps)?,
        link: None,
    })
}

/// The record of the plugin folder `folder`, if there is a readable one.
pub(crate) fn record_for(paths: &Paths, folder: &Path) -> Option<Record> {
    let Stored::Found(store) = read_store(paths) else {
        return None;
    };
    let canon = folder.canonicalize().ok()?;
    store.plugins.get(&*canon.to_string_lossy()).cloned()
}

/// Serialises read-modify-write of the record file.
static STORE_LOCK: Mutex<()> = Mutex::new(());

/// The key a folder's record is stored under: its canonical path. None when the folder cannot be
/// resolved (it does not exist).
pub(crate) fn key_of(folder: &Path) -> Option<String> {
    Some(folder.canonicalize().ok()?.to_string_lossy().into_owned())
}

/// Whether records can be written: the first-start migration has run (a record written before it
/// would take its place and leave the folders already there unrecorded) and the store is
/// readable (it is never overwritten).
pub(crate) fn writable(paths: &Paths) -> Result<(), String> {
    if std::fs::metadata(marker_path(paths)).is_err() {
        return Err("the installed plugins have not been recorded yet: restart the daemon".into());
    }
    match read_store(paths) {
        Stored::Unreadable(error) => Err(unreadable(paths, &error)),
        _ => Ok(()),
    }
}

fn unreadable(paths: &Paths, error: &str) -> String {
    format!(
        "the plugin record file is unreadable ({error}); fix or delete {} first",
        store_path(paths).display()
    )
}

/// Adds (or replaces) the record of the plugin folder `folder`. Refused before the first-start
/// migration has run, and when the store cannot be read (it is never overwritten).
pub(crate) fn add_record(paths: &Paths, folder: &Path, record: Record) -> Result<(), String> {
    let key = key_of(folder).ok_or_else(|| format!("{}: not found", folder.display()))?;
    add_record_at(paths, key, record)
}

/// [`add_record`] under a key worked out earlier (`key_of`).
pub(crate) fn add_record_at(paths: &Paths, key: String, record: Record) -> Result<(), String> {
    let _guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    writable(paths)?;
    let mut store = match read_store(paths) {
        Stored::Found(store) => store,
        _ => Store::default(),
    };
    store.plugins.insert(key, record);
    write_json(&store_path(paths), &store)
}

/// Records an approval of `kind` for the plugin folder `folder`, if `verifier` lets that kind be
/// written. The folder's content hash and run-summary digest must equal `hash` and `runs` (what
/// the approval was asked for), so other content than was shown is never approved. A folder with
/// no record gets one with `origin = user`; a recorded one takes the approved hash and digest as
/// its own (this is also how a skills-only plugin the user edited is recorded again). Nothing here
/// decides who may approve: the caller (stage 8a) does, and nothing calls this yet but tests.
#[allow(dead_code)] // Stage 8a's `plugins.approve` calls it.
pub(crate) fn record_approval(
    paths: &Paths,
    verifier: &dyn ApprovalVerifier,
    folder: &Path,
    kind: ApprovalKind,
    hash: &str,
    runs: &str,
) -> Result<(), String> {
    if !verifier.may_write(kind) {
        return Err("this build does not count approvals yet: nothing was recorded".into());
    }
    let canon = folder
        .canonicalize()
        .map_err(|error| format!("{}: {error}", folder.display()))?;
    let seen = look(&canon)?;
    if let Some(link) = &seen.link {
        return Err(format!(
            "the plugin holds a link at {link}: it is not approved"
        ));
    }
    let plugin = parse(&canon).ok_or("the folder holds no plugin that can be read")?;
    let now = plugins::runs(&plugin).digest();
    if seen.hash != hash || now != runs {
        return Err(
            "the plugin changed since the approval was asked for: nothing was recorded".into(),
        );
    }
    let key = canon.to_string_lossy().into_owned();
    let mut record = record_for(paths, &canon).unwrap_or_else(|| Record {
        name: plugin.name.clone(),
        origin: Origin::User,
        id: None,
        source: None,
        commit: None,
        hash: hash.to_string(),
        runs: runs.to_string(),
        approval: None,
    });
    record.hash = hash.to_string();
    record.runs = runs.to_string();
    record.approval = Some(Approval {
        hash: hash.to_string(),
        runs: runs.to_string(),
        at_unix_ms: unix_ms(),
        kind,
    });
    add_record_at(paths, key, record)
}

/// Removes the record stored under `key` and returns it. A missing store holds none; an
/// unreadable one is left as it is.
pub(crate) fn take_record(paths: &Paths, key: &str) -> Result<Option<Record>, String> {
    let _guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut store = match read_store(paths) {
        Stored::Found(store) => store,
        Stored::Missing => return Ok(None),
        Stored::Unreadable(error) => return Err(unreadable(paths, &error)),
    };
    let taken = store.plugins.remove(key);
    if taken.is_some() {
        write_json(&store_path(paths), &store)?;
    }
    Ok(taken)
}

/// The content hash of the plugin folder `canon` (and whether it holds a link), from the cache
/// when the walk's list of paths, sizes and times is the cached one.
pub(crate) fn look(canon: &Path) -> Result<Seen, String> {
    let mut stamps = Vec::new();
    if let Some(link) = walk(canon, "", &mut stamps)? {
        return Ok(Seen {
            hash: String::new(),
            link: Some(link),
        });
    }
    stamps.sort();
    {
        let cache = HASHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached, hash)) = cache.get(canon)
            && *cached == stamps
        {
            return Ok(Seen {
                hash: hash.clone(),
                link: None,
            });
        }
    }
    let hash = hash_files(canon, &stamps)?;
    HASHES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(canon.to_path_buf(), (stamps, hash.clone()));
    Ok(Seen { hash, link: None })
}

/// The plugin a folder holds, as `discover` loads it (nothing is started).
pub(crate) fn parse(folder: &Path) -> Option<plugins::Plugin> {
    plugins::discover(&[folder.to_path_buf()], Duration::from_secs(60), &[])
        .0
        .into_iter()
        .next()
}

fn has_code(plugin: &plugins::Plugin) -> bool {
    !plugin.hooks.is_empty() || !plugin.servers.is_empty()
}

/// A folder the gate refuses, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Withheld {
    pub(crate) folder: PathBuf,
    pub(crate) reason: String,
}

/// What the gate decided for the plugin folders under `<config>/plugins`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Gate {
    pub(crate) withheld: Vec<Withheld>,
    /// The folders the gate lets through: the only ones under `<config>/plugins` a session may
    /// load (a folder that appears after the gate ran is not among them).
    pub(crate) allowed: Vec<PathBuf>,
    /// Folders whose record says the content was downloaded (a marketplace or an agent).
    pub(crate) downloaded: Vec<PathBuf>,
}

/// The load-time gate: withholds a plugin whose folder
/// 1. has hooks or servers and no record,
/// 2. has a folder name, recorded name and manifest name that are not all equal,
/// 3. is, or holds, a symbolic link or reparse point,
/// 4. hashes differently from its record, or
/// 5. has hooks or servers without an approval the verifier honours whose hash and run summary
///    equal what is there now.
///
/// An unrecorded folder with neither hooks nor servers loads (unless it holds a link).
pub(crate) fn gate(paths: &Paths, verifier: &dyn ApprovalVerifier) -> Gate {
    let store = match read_store(paths) {
        Stored::Found(store) => store,
        Stored::Missing => Store::default(),
        Stored::Unreadable(error) => {
            tracing::warn!(%error, "the plugin record file is unreadable; plugins with code are withheld");
            Store::default()
        }
    };
    let mut gate = Gate::default();
    for folder in plugins::plugin_folders(&[plugins_dir(paths)]) {
        let record = folder
            .canonicalize()
            .ok()
            .and_then(|canon| store.plugins.get(&*canon.to_string_lossy()));
        if let Some(record) = record
            && record.origin.downloaded()
        {
            gate.downloaded.push(folder.clone());
        }
        match judge(&folder, record, verifier) {
            Some(reason) => gate.withheld.push(Withheld { folder, reason }),
            None => gate.allowed.push(folder),
        }
    }
    gate
}

fn judge(
    folder: &Path,
    record: Option<&Record>,
    verifier: &dyn ApprovalVerifier,
) -> Option<String> {
    let canon = match folder.canonicalize() {
        Ok(canon) => canon,
        Err(error) => return Some(format!("its folder cannot be resolved: {error}")),
    };
    let link_reason = |link: &str| format!("it contains a symbolic link or junction ({link})");
    if std::fs::symlink_metadata(folder).is_ok_and(|meta| is_link(&meta)) {
        return Some(link_reason("the folder itself"));
    }
    let seen = match look(&canon) {
        Ok(seen) => seen,
        Err(error) => return Some(format!("it cannot be read: {error}")),
    };
    if let Some(link) = &seen.link {
        return Some(link_reason(link));
    }
    // A folder that does not parse as a plugin does not load either.
    let plugin = parse(folder)?;
    let code = has_code(&plugin);
    let Some(record) = record else {
        return code.then(|| "not installed by Imperium: approve to load".to_string());
    };
    let folder_name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A preexisting or user plugin (a clone, say) may sit in a folder of another name; one
    // that Imperium downloaded sits in `<name>`. A moved folder is a new canonical path with no
    // record either way, and a changed manifest name changes the hash.
    let named_for_it = !record.origin.downloaded() || folder_name == record.name;
    if !named_for_it || record.name != plugin.name {
        return Some(format!(
            "the folder name ({folder_name}), the recorded name ({}) and the manifest name ({}) differ",
            record.name, plugin.name
        ));
    }
    if seen.hash != record.hash {
        return Some("its content changed since Imperium recorded it".to_string());
    }
    if !code {
        return None;
    }
    let digest = plugins::runs(&plugin).digest();
    match &record.approval {
        None => Some("not approved: approve to load".to_string()),
        Some(approval) if !verifier.honours(approval.kind) => {
            Some("its approval does not count here".to_string())
        }
        Some(approval) if approval.hash != seen.hash || approval.runs != digest => {
            Some("what it runs changed since it was approved: approve to load".to_string())
        }
        Some(_) => None,
    }
}

/// What [`migrate`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Migration {
    /// This many folders were recorded; each of `skipped` could not be carried over, with why
    /// and what to do.
    Recorded { count: usize, skipped: Vec<String> },
    /// The marker already exists: the store is not rebuilt even if it is missing or unreadable.
    Done,
    /// The store exists but cannot be read: it is left as it is and nothing is carried over.
    Damaged,
}

/// The first-start migration: records every folder already under `<config>/plugins` with
/// `origin = preexisting` (`agent` for one an agent installed) and its current hash and run
/// summary; a folder whose plugin is not in `disabled` gets a carried-over approval, so nothing
/// that runs today stops. Then writes the marker. Does nothing once the marker exists, and never
/// overwrites a store it cannot read.
pub(crate) fn migrate(paths: &Paths, disabled: &[String]) -> Result<Migration, String> {
    // Only a marker that is certainly absent lets the migration run: a marker that cannot be
    // read counts as present.
    let absent = matches!(
        std::fs::metadata(marker_path(paths)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    if !absent {
        return Ok(Migration::Done);
    }
    let (count, skipped) = match read_store(paths) {
        Stored::Unreadable(_) => return Ok(Migration::Damaged),
        // A store with no marker (a crash between the two writes): keep it, finish the marker.
        Stored::Found(_) => (0, Vec::new()),
        Stored::Missing => {
            let (store, skipped) = record_present(paths, disabled);
            write_json(&store_path(paths), &store)?;
            (store.plugins.len(), skipped)
        }
    };
    write_json(
        &marker_path(paths),
        &serde_json::json!({ "version": 1, "migrated_unix_ms": unix_ms() }),
    )?;
    Ok(Migration::Recorded { count, skipped })
}

/// The records for the folders present, and a report for each folder that is not carried over.
fn record_present(paths: &Paths, disabled: &[String]) -> (Store, Vec<String>) {
    let mut store = Store::default();
    let mut skipped = Vec::new();
    for folder in plugins::plugin_folders(&[plugins_dir(paths)]) {
        let (Ok(canon), Some(plugin)) = (folder.canonicalize(), parse(&folder)) else {
            continue;
        };
        let shown = folder.display();
        if std::fs::symlink_metadata(&folder).is_ok_and(|meta| is_link(&meta)) {
            skipped.push(format!(
                "{shown} is a link: replace it with a real folder, or approve it once approvals exist"
            ));
            continue;
        }
        let seen = match look(&canon) {
            Ok(seen) => seen,
            Err(error) => {
                skipped.push(format!(
                    "{shown} cannot be read ({error}): fix it, or approve it once approvals exist"
                ));
                continue;
            }
        };
        if let Some(link) = &seen.link {
            skipped.push(format!(
                "{shown} contains a link at {link}: remove it, or approve it once approvals exist"
            ));
            continue;
        }
        let runs = plugins::runs(&plugin).digest();
        let approval = (!disabled.contains(&plugin.name)).then(|| Approval {
            hash: seen.hash.clone(),
            runs: runs.clone(),
            at_unix_ms: unix_ms(),
            kind: ApprovalKind::CarriedOver,
        });
        store.plugins.insert(
            canon.to_string_lossy().into_owned(),
            Record {
                origin: if plugins::agent_installed(&folder) {
                    Origin::Agent
                } else {
                    Origin::Preexisting
                },
                name: plugin.name,
                id: None,
                source: None,
                commit: None,
                hash: seen.hash,
                runs,
                approval,
            },
        );
    }
    (store, skipped)
}

/// A link `link` to `target`, or false when the machine will not make one.
#[cfg(test)]
pub(crate) fn make_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        // `mklink` reads a `/` in the path as a switch: use the native separators.
        let link: PathBuf = link.components().collect();
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(target)
            .output()
            .is_ok_and(|output| output.status.success())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        false
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    struct Home {
        _dir: tempfile::TempDir,
        paths: Paths,
    }

    fn home() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        Home { _dir: dir, paths }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const HOOK: &str = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "check"}]}]}}"#;

    /// A plugin folder under `<config>/plugins/<folder>` with a skill, and a hook when asked.
    fn plugin(home: &Home, folder: &str, name: &str, hook: bool) -> PathBuf {
        let root = plugins_dir(&home.paths).join(folder);
        write(
            &root.join(".claude-plugin/plugin.json"),
            &format!(r#"{{"name": "{name}"}}"#),
        );
        write(
            &root.join("skills/s/SKILL.md"),
            "---\nname: s\ndescription: a skill\n---\nbody\n",
        );
        if hook {
            write(&root.join("hooks/hooks.json"), HOOK);
        }
        root
    }

    fn canon_key(folder: &Path) -> String {
        folder
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// Records `folder` as it is now, with `approval` of that content.
    fn record(home: &Home, folder: &Path, origin: Origin, approval: Option<ApprovalKind>) {
        let canon = folder.canonicalize().unwrap();
        let plugin = parse(folder).unwrap();
        let hash = look(&canon).unwrap().hash;
        let runs = plugins::runs(&plugin).digest();
        let mut store = match read_store(&home.paths) {
            Stored::Found(store) => store,
            _ => Store::default(),
        };
        store.plugins.insert(
            canon_key(folder),
            Record {
                name: plugin.name,
                origin,
                id: None,
                source: None,
                commit: None,
                hash: hash.clone(),
                runs: runs.clone(),
                approval: approval.map(|kind| Approval {
                    hash,
                    runs,
                    at_unix_ms: 1,
                    kind,
                }),
            },
        );
        write_json(&store_path(&home.paths), &store).unwrap();
    }

    fn reasons(home: &Home, verifier: &dyn ApprovalVerifier) -> Vec<(String, String)> {
        gate(&home.paths, verifier)
            .withheld
            .into_iter()
            .map(|w| {
                (
                    w.folder.file_name().unwrap().to_string_lossy().into_owned(),
                    w.reason,
                )
            })
            .collect()
    }

    fn withheld(home: &Home) -> Vec<String> {
        reasons(home, &DebugVerifier)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn an_approval_is_written_only_when_the_verifier_allows_it() {
        let home = home();
        migrate(&home.paths, &[]).unwrap();
        let folder = plugin(&home, "p", "p", true);
        let canon = folder.canonicalize().unwrap();
        let hash = look(&canon).unwrap().hash;
        let runs = plugins::runs(&parse(&folder).unwrap()).digest();
        let ask = |verifier: &dyn ApprovalVerifier, hash: &str, runs: &str| {
            record_approval(
                &home.paths,
                verifier,
                &folder,
                ApprovalKind::Counted,
                hash,
                runs,
            )
        };

        // The production verifier writes nothing (no approval counts before stage 8c).
        assert!(!production_verifier().may_write(ApprovalKind::Counted));
        let error = ask(production_verifier(), &hash, &runs).unwrap_err();
        assert!(error.contains("does not count"), "{error}");
        assert!(record_for(&home.paths, &folder).is_none());
        assert_eq!(reasons(&home, &ReleaseVerifier).len(), 1);

        // Content other than what was asked about is not approved.
        let error = ask(&TestVerifier, "0000", &runs).unwrap_err();
        assert!(error.contains("changed since"), "{error}");
        let error = ask(&TestVerifier, &hash, "0000").unwrap_err();
        assert!(error.contains("changed since"), "{error}");
        assert!(record_for(&home.paths, &folder).is_none());

        // An unrecorded folder gets an `origin = user` record with the approval, and loads.
        ask(&TestVerifier, &hash, &runs).unwrap();
        let record = record_for(&home.paths, &folder).unwrap();
        assert_eq!(record.origin, Origin::User);
        assert_eq!(
            record.approval.map(|approval| approval.kind),
            Some(ApprovalKind::Counted)
        );
        assert!(reasons(&home, &ReleaseVerifier).is_empty());

        // Any later change withholds it again.
        write(
            &folder.join("skills/s/SKILL.md"),
            "---\nname: s\ndescription: x\n---\nedited\n",
        );
        assert_eq!(reasons(&home, &ReleaseVerifier).len(), 1);
    }

    #[test]
    fn code_needs_an_approval_the_verifier_honours() {
        let home = home();
        let none = plugin(&home, "none", "none", true);
        let counted = plugin(&home, "counted", "counted", true);
        let dialog = plugin(&home, "dialog", "dialog", true);
        let carried = plugin(&home, "carried", "carried", true);
        let dev = plugin(&home, "dev", "dev", true);
        record(&home, &none, Origin::User, None);
        record(&home, &counted, Origin::User, Some(ApprovalKind::Counted));
        record(&home, &dialog, Origin::User, Some(ApprovalKind::DialogOnly));
        record(
            &home,
            &carried,
            Origin::User,
            Some(ApprovalKind::CarriedOver),
        );
        record(&home, &dev, Origin::User, Some(ApprovalKind::Dev));
        assert_eq!(withheld(&home), ["none"]);
        let mut release = reasons(&home, &ReleaseVerifier);
        release.sort();
        assert_eq!(
            release.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["dev", "none"]
        );
    }

    #[test]
    fn an_unrecorded_folder_with_code_is_withheld_and_a_skills_only_one_loads() {
        let home = home();
        plugin(&home, "copied", "copied", true);
        plugin(&home, "skills", "skills", false);
        let moved = plugin(&home, "before", "before", true);
        record(&home, &moved, Origin::User, Some(ApprovalKind::Counted));
        assert!(withheld(&home).contains(&"copied".to_string()));
        let reasons = reasons(&home, &DebugVerifier);
        assert!(reasons.iter().any(|(name, reason)| name == "copied"
            && reason == "not installed by Imperium: approve to load"));
        assert!(!reasons.iter().any(|(name, _)| name == "skills"));
        std::fs::rename(&moved, plugins_dir(&home.paths).join("after")).unwrap();
        let reasons = self::reasons(&home, &DebugVerifier);
        assert!(reasons.iter().any(|(name, _)| name == "after"));
    }

    #[test]
    fn rewriting_the_source_file_changes_neither_the_gate_nor_what_is_downloaded() {
        let home = home();
        let root = plugin(&home, "agentic", "agentic", true);
        write(&root.join(plugins::SOURCE_FILE), r#"{"session_id": "s1"}"#);
        record(&home, &root, Origin::Agent, Some(ApprovalKind::Counted));
        for text in ["{}", "not json", r#"{"session_id": null}"#] {
            write(&root.join(plugins::SOURCE_FILE), text);
            let gate = gate(&home.paths, &DebugVerifier);
            assert!(gate.withheld.is_empty(), "{text}: {:?}", gate.withheld);
            assert_eq!(gate.downloaded.len(), 1, "{text}");
        }
        std::fs::remove_file(root.join(plugins::SOURCE_FILE)).unwrap();
        assert_eq!(gate(&home.paths, &DebugVerifier).downloaded.len(), 1);
    }

    #[test]
    fn a_renamed_manifest_or_folder_is_withheld() {
        let home = home();
        let root = plugin(&home, "lint", "lint", true);
        record(&home, &root, Origin::User, Some(ApprovalKind::Counted));
        assert!(withheld(&home).is_empty());
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "other"}"#,
        );
        let reasons = reasons(&home, &DebugVerifier);
        assert!(reasons[0].1.contains("differ"), "{reasons:?}");

        // A downloaded plugin's folder renamed and its record moved with it (a rewritten store):
        // the names differ.
        let second = plugin(&home, "second", "second", true);
        record(
            &home,
            &second,
            Origin::Marketplace,
            Some(ApprovalKind::Counted),
        );
        let renamed = plugins_dir(&home.paths).join("renamed");
        std::fs::rename(&second, &renamed).unwrap();
        let mut store = match read_store(&home.paths) {
            Stored::Found(store) => store,
            _ => unreachable!(),
        };
        let moved = store.plugins.remove(&canon_key(&root)).unwrap();
        store.plugins.insert(canon_key(&renamed), moved);
        write_json(&store_path(&home.paths), &store).unwrap();
        assert!(withheld(&home).contains(&"renamed".to_string()));
    }

    #[test]
    fn a_link_inside_a_plugin_withholds_it() {
        let home = home();
        let root = plugin(&home, "linked", "linked", true);
        record(&home, &root, Origin::User, Some(ApprovalKind::Counted));
        let outside = tempfile::tempdir().unwrap();
        assert!(make_link(outside.path(), &root.join("skills/out")));
        let reasons = reasons(&home, &DebugVerifier);
        assert!(reasons[0].1.contains("symbolic link"), "{reasons:?}");
        // A skills-only folder with a link is withheld too.
        let skills = plugin(&home, "skillful", "skillful", false);
        assert!(make_link(outside.path(), &skills.join("skills/out")));
        assert!(withheld(&home).contains(&"skillful".to_string()));
    }

    #[test]
    fn renaming_a_file_or_editing_a_skill_withholds_the_whole_plugin() {
        let home = home();
        let root = plugin(&home, "lint", "lint", true);
        write(&root.join("skills/s/a.md"), "a");
        record(&home, &root, Origin::User, Some(ApprovalKind::Counted));
        assert!(withheld(&home).is_empty());
        // Same count, same newest time (a rename keeps it): only the path differs.
        std::fs::rename(root.join("skills/s/a.md"), root.join("skills/s/b.md")).unwrap();
        assert_eq!(withheld(&home), ["lint"]);
        std::fs::rename(root.join("skills/s/b.md"), root.join("skills/s/a.md")).unwrap();
        assert!(withheld(&home).is_empty());
        write(
            &root.join("skills/s/SKILL.md"),
            "---\nname: s\n---\nedited\n",
        );
        assert_eq!(withheld(&home), ["lint"]);
    }

    #[test]
    fn a_hook_added_with_a_forged_record_hash_is_still_withheld() {
        let home = home();
        let root = plugin(&home, "lint", "lint", true);
        record(&home, &root, Origin::User, Some(ApprovalKind::Counted));
        write(
            &root.join("hooks/hooks.json"),
            r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "check"}, {"type": "command", "command": "evil"}]}]}}"#,
        );
        // The record now matches the content; the approval still names the old run summary.
        let canon = root.canonicalize().unwrap();
        let mut store = match read_store(&home.paths) {
            Stored::Found(store) => store,
            _ => unreachable!(),
        };
        let entry = store.plugins.get_mut(&canon_key(&root)).unwrap();
        entry.hash = look(&canon).unwrap().hash;
        write_json(&store_path(&home.paths), &store).unwrap();
        let reasons = reasons(&home, &DebugVerifier);
        assert!(reasons[0].1.contains("what it runs changed"), "{reasons:?}");
    }

    #[test]
    fn migration_carries_over_what_runs_today_once() {
        let home = home();
        let on = plugin(&home, "on", "on", true);
        plugin(&home, "off", "off", true);
        plugin(&home, "skills", "skills", false);
        let disabled = vec!["off".to_string()];
        assert_eq!(
            migrate(&home.paths, &disabled),
            Ok(Migration::Recorded {
                count: 3,
                skipped: Vec::new()
            })
        );
        assert_eq!(withheld(&home), ["off"]);
        let Stored::Found(store) = read_store(&home.paths) else {
            panic!("no store");
        };
        let recorded = &store.plugins[&canon_key(&on)];
        assert_eq!(recorded.origin, Origin::Preexisting);
        assert_eq!(
            recorded.approval.as_ref().unwrap().kind,
            ApprovalKind::CarriedOver
        );
        let off = &store.plugins[&canon_key(&plugins_dir(&home.paths).join("off"))];
        assert!(off.approval.is_none());

        // A second start does not migrate again: a folder added later is unrecorded.
        plugin(&home, "later", "later", true);
        assert_eq!(migrate(&home.paths, &[]), Ok(Migration::Done));
        assert!(withheld(&home).contains(&"later".to_string()));
    }

    #[test]
    fn a_preexisting_clone_in_a_folder_of_another_name_keeps_loading() {
        let home = home();
        plugin(&home, "some-repo", "tool", true);
        migrate(&home.paths, &[]).unwrap();
        assert!(withheld(&home).is_empty());
        let moved = plugins_dir(&home.paths).join("renamed-repo");
        std::fs::rename(plugins_dir(&home.paths).join("some-repo"), &moved).unwrap();
        // Moved: another canonical path, so no record.
        assert_eq!(withheld(&home), ["renamed-repo"]);
    }

    #[test]
    fn migration_reports_a_folder_with_a_link_and_the_gate_withholds_it() {
        let home = home();
        let linked = plugin(&home, "linked", "linked", true);
        plugin(&home, "fine", "fine", true);
        let outside = tempfile::tempdir().unwrap();
        assert!(make_link(outside.path(), &linked.join("skills/out")));
        let Ok(Migration::Recorded { count, skipped }) = migrate(&home.paths, &[]) else {
            panic!("not migrated");
        };
        assert_eq!(count, 1);
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(skipped[0].contains("link") && skipped[0].contains("approve"));
        assert_eq!(withheld(&home), ["linked"]);
    }

    #[test]
    fn a_shipped_imperium_json_file_is_content_and_a_link_named_so_is_refused() {
        let home = home();
        let root = plugin(&home, "lint", "lint", true);
        write(&root.join(".imperium-run.json"), "{\"a\": 1}");
        record(&home, &root, Origin::User, Some(ApprovalKind::Counted));
        assert!(withheld(&home).is_empty());
        write(&root.join(".imperium-run.json"), "{\"a\": 22}");
        assert_eq!(withheld(&home), ["lint"]);
        // Imperium's own provenance file is not content.
        let other = plugin(&home, "other", "other", true);
        write(&other.join(plugins::SOURCE_FILE), "{}");
        record(&home, &other, Origin::User, Some(ApprovalKind::Counted));
        write(&other.join(plugins::SOURCE_FILE), r#"{"changed": true}"#);
        assert_eq!(withheld(&home), ["lint"]);
        // A link named like an excluded file is still found.
        let outside = tempfile::tempdir().unwrap();
        std::fs::remove_file(other.join(plugins::SOURCE_FILE)).unwrap();
        assert!(make_link(outside.path(), &other.join(plugins::SOURCE_FILE)));
        assert!(withheld(&home).contains(&"other".to_string()));
    }

    #[test]
    fn the_gate_names_the_folders_it_allows() {
        let home = home();
        let good = plugin(&home, "good", "good", false);
        plugin(&home, "bad", "bad", true);
        let gate = gate(&home.paths, &DebugVerifier);
        assert_eq!(gate.allowed, [good]);
        assert_eq!(gate.withheld.len(), 1);
    }

    #[test]
    fn with_the_marker_present_a_missing_or_unreadable_store_withholds_everything_with_code() {
        let home = home();
        plugin(&home, "a", "a", true);
        plugin(&home, "b", "b", false);
        migrate(&home.paths, &[]).unwrap();
        assert!(withheld(&home).is_empty());

        std::fs::remove_file(store_path(&home.paths)).unwrap();
        assert_eq!(migrate(&home.paths, &[]), Ok(Migration::Done));
        assert_eq!(withheld(&home), ["a"]);
        assert!(matches!(read_store(&home.paths), Stored::Missing));

        write(&store_path(&home.paths), "{ not json");
        assert_eq!(migrate(&home.paths, &[]), Ok(Migration::Done));
        assert_eq!(withheld(&home), ["a"]);
        assert_eq!(
            std::fs::read_to_string(store_path(&home.paths)).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn a_damaged_store_without_a_marker_is_not_overwritten() {
        let home = home();
        plugin(&home, "a", "a", true);
        write(&store_path(&home.paths), "garbage");
        assert_eq!(migrate(&home.paths, &[]), Ok(Migration::Damaged));
        assert_eq!(
            std::fs::read_to_string(store_path(&home.paths)).unwrap(),
            "garbage"
        );
        assert!(!marker_path(&home.paths).exists());
        assert_eq!(withheld(&home), ["a"]);
    }

    #[test]
    fn a_thousand_file_folder_hashes_and_is_cached() {
        let home = home();
        let root = plugin(&home, "big", "big", false);
        for number in 0..1000 {
            write(
                &root.join(format!("skills/s/data/{}/{number}.txt", number % 20)),
                &"x".repeat(2048),
            );
        }
        let canon = root.canonicalize().unwrap();
        let started = Instant::now();
        let first = look(&canon).unwrap();
        let hashing = started.elapsed();
        let started = Instant::now();
        let second = look(&canon).unwrap();
        let cached = started.elapsed();
        eprintln!("1,000 files: first look {hashing:?}, cached look {cached:?}");
        assert_eq!(first.hash, second.hash);
        assert!(hashing < Duration::from_secs(30), "{hashing:?}");
    }
}
