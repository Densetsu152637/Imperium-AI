use super::*;

/// `mods.list`: UI mods installed in `<config>/mods`, with their entry modules.
#[derive(Debug)]
pub struct ModList;

impl Method for ModList {
    const NAME: &'static str = "mods.list";
    type Params = ModListParams;
    type Result = ModListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModListResult {
    pub mods: Vec<ModInfo>,
    /// Mods that could not be loaded, with the reason.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `telemetry:read`, `sessions:read`, `pets:read`, `storage`; the desktop app grants only these.
    pub permissions: Vec<String>,
    pub panels: Vec<ModPanel>,
    /// Shipped with Imperium (opt-in in the desktop app) rather than installed by the user.
    pub bundled: bool,
    /// The entry ES module's source.
    pub source: String,
    /// Where it was installed from, when an agent installed it (`install_extension`).
    #[serde(default)]
    pub installed_from: Option<InstallSource>,
    /// Off until the user turns it on: approved without a terminal prompt, or modified since.
    #[serde(default)]
    pub opt_in: bool,
    /// Its files no longer match what the user approved, so it has no permissions.
    #[serde(default)]
    pub modified: bool,
    /// The manifest asks to run in the app's own window instead of a sandboxed frame.
    #[serde(default)]
    pub main_context: bool,
    /// The user trusted it at install, for this exact content, so it may run in the app's own window.
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModPanel {
    pub id: String,
    pub title: String,
    /// Where the panel docks: `sidebar.left` or `sidebar.right` (the default).
    #[serde(default)]
    pub slot: Option<String>,
}

/// `mods.inspect`: fetches a mod's source into a staging folder and reports what it would
/// install, without installing it: its permissions, its content hash, and, when a mod of the
/// same id is installed, how its requested permissions differ. Runs nothing from the source.
/// Used by `imperium mods install` and `imperium mods update` to ask the user first; not part
/// of the desktop app's protocol, so it has no generated TypeScript type.
#[derive(Debug)]
pub struct ModInspect;

impl Method for ModInspect {
    const NAME: &'static str = "mods.inspect";
    type Params = ModInspectParams;
    type Result = ModPreview;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModInspectParams {
    /// A git URL (`https://`, `ssh://`, `git@host:path`, `file://`) or an absolute folder on
    /// the daemon's machine.
    pub source: String,
    /// A branch, tag, or commit (git only).
    #[serde(default)]
    pub revision: Option<String>,
    /// The folder inside the source that holds the mod.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModPreview {
    pub id: String,
    pub name: String,
    pub version: String,
    /// What the mod's manifest requests.
    pub permissions: Vec<String>,
    pub panels: Vec<ModPanel>,
    /// The manifest asks to run in the app's own window (a trusted mod).
    #[serde(default)]
    pub main_context: bool,
    /// SHA-256 of the mod's files (paths and contents, in order; not `.git` or Imperium's own
    /// bookkeeping files), as hex. `mods.install` installs only content with this hash.
    pub sha256: String,
    /// The git URL or folder, as resolved.
    pub location: String,
    /// The commit fetched, for git.
    #[serde(default)]
    pub revision: Option<String>,
    /// The mod of this id that is installed now, if any.
    #[serde(default)]
    pub installed: Option<ModInstalled>,
    /// Requested now but not approved for the installed version (all of them for a new mod).
    pub added_permissions: Vec<String>,
    /// Approved for the installed version but no longer requested.
    pub removed_permissions: Vec<String>,
}

/// An installed mod, as an update compares against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModInstalled {
    pub version: String,
    /// The permissions the user approved when it was installed (what its manifest requested,
    /// for a mod installed without an approval record).
    pub permissions: Vec<String>,
    /// The hash recorded at install, when there is one.
    #[serde(default)]
    pub sha256: Option<String>,
    /// The user trusted this version to run in the app's own window.
    #[serde(default)]
    pub trusted: bool,
}

/// `mods.install`: installs or updates a mod in `<config>/mods/<id>` from the source
/// `mods.inspect` showed, but only if its content still has the hash the user approved and it
/// asks for nothing beyond the permissions the user approved. Nothing in it runs.
#[derive(Debug)]
pub struct ModInstall;

impl Method for ModInstall {
    const NAME: &'static str = "mods.install";
    type Params = ModInstallParams;
    type Result = ModInstallResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModInstallParams {
    pub source: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    /// The hash from `mods.inspect`; the install is refused if the fetched content differs.
    pub sha256: String,
    /// The permissions the user approved; the install is refused if the mod requests another.
    pub permissions: Vec<String>,
    /// Replace the installed mod of that id (an update) rather than refusing to overwrite it.
    /// Only a mod installed from the same source it was approved from can be replaced.
    #[serde(default)]
    pub replace: bool,
    /// The user answered a prompt at a terminal. Otherwise the mod stays off until the user
    /// turns it on in the desktop app.
    #[serde(default)]
    pub interactive: bool,
    /// The user typed the confirmation at a terminal that lets this mod, which asks for it,
    /// run in the app's own window. Refused for a mod that does not ask. Never carried over to
    /// an update: each new content hash asks again.
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModInstallResult {
    pub id: String,
    pub version: String,
    /// Its folder.
    pub path: String,
    pub permissions: Vec<String>,
    pub sha256: String,
    /// The version it replaced, for an update.
    #[serde(default)]
    pub replaced: Option<String>,
    /// Whether the mod now stands as approved at a terminal prompt (it is on unless the user
    /// turns it off); false means it stays off until the user turns it on.
    #[serde(default)]
    pub interactive: bool,
    /// Whether the mod is trusted to run in the app's own window.
    #[serde(default)]
    pub trusted: bool,
    /// Anything the daemon did not do as asked, such as a grant it did not count.
    #[serde(default)]
    pub notes: Vec<String>,
}
