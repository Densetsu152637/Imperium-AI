use super::*;

// ---- Changes agents make to the app config ----

/// Where an installed mod, plugin, or skill came from, recorded in its folder as
/// `.imperium-source.json` when an agent installs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InstallSource {
    /// `git` (cloned from a URL) or `path` (copied from a folder on this machine).
    #[ts(type = "\"git\" | \"path\"")]
    pub kind: String,
    /// The git URL or the folder, as resolved.
    pub location: String,
    /// The commit installed, for `git`.
    #[serde(default)]
    pub revision: Option<String>,
    /// The branch, tag, or commit asked for, for `git`.
    #[serde(default)]
    pub requested: Option<String>,
    /// The folder inside the source that was installed, when not its top.
    #[serde(default)]
    pub subdir: Option<String>,
    /// When it was installed, in Unix milliseconds.
    #[ts(type = "number")]
    pub installed_at_unix_ms: u64,
    /// The session whose agent installed it.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// One change an agent made to the app config, as the change journal keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigChange {
    #[ts(type = "number")]
    pub id: u64,
    #[ts(type = "number")]
    pub at_unix_ms: u64,
    /// The session whose agent made it.
    #[serde(default)]
    pub session_id: Option<String>,
    /// `model_card` (a card written), `install`, or `uninstall` (a mod, plugin, or skill).
    #[ts(type = "\"model_card\" | \"install\" | \"uninstall\"")]
    pub kind: String,
    /// One line saying what changed.
    pub summary: String,
    /// For `model_card`: the model table row or local model.
    #[serde(default)]
    pub model: Option<String>,
    /// For `model_card`: `table` (`models.overrides."<id>".card`) or `local`
    /// (`runtime.llamacpp.models."<id>".card`), both in the app settings file.
    #[serde(default)]
    pub model_kind: Option<String>,
    /// For `model_card`: the card the app settings file held before, if any.
    #[serde(default)]
    pub before: Option<String>,
    /// For `model_card`: the card written; none when it was cleared.
    #[serde(default)]
    pub after: Option<String>,
    /// For `install` and `uninstall`: `mod`, `plugin`, or `skill`.
    #[serde(default)]
    pub extension_kind: Option<String>,
    /// For `install` and `uninstall`: the extension's name.
    #[serde(default)]
    pub name: Option<String>,
    /// For `install` and `uninstall`: its folder.
    #[serde(default)]
    pub path: Option<String>,
    /// For `install`, and an `uninstall` of something an agent installed: where it came from.
    #[serde(default)]
    pub source: Option<InstallSource>,
    /// For `install` of a plugin with hooks or MCP servers: it was added to
    /// `extensions.disabled_plugins`, so its hooks and servers do not run until the user turns
    /// it on.
    #[serde(default)]
    pub disabled: bool,
    /// For `uninstall`: where the folder was moved, so undo can put it back.
    #[serde(default)]
    pub kept_at: Option<String>,
    /// For `install` and `uninstall` of a plugin: the content hash of its folder when it was
    /// installed or removed, so undo puts back (or removes) only that content.
    #[serde(default)]
    pub sha256: Option<String>,
    /// For `uninstall` of a plugin Imperium had a record of: that record as JSON, so undo can
    /// restore it with the folder.
    #[serde(default)]
    pub record: Option<String>,
    /// When it was undone, in Unix milliseconds.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub undone_at_unix_ms: Option<u64>,
}

/// `config.changes`: the changes agents made to the app config, newest first.
#[derive(Debug)]
pub struct ConfigChanges;

impl Method for ConfigChanges {
    const NAME: &'static str = "config.changes";
    type Params = ConfigChangesParams;
    type Result = ConfigChangesResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigChangesParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigChangesResult {
    pub changes: Vec<ConfigChange>,
}

/// `config.undo`: undoes one change from `config.changes`: a card goes back to what it was
/// (refused when it was edited since), an install is removed, and an uninstalled extension is
/// put back.
#[derive(Debug)]
pub struct ConfigUndo;

impl Method for ConfigUndo {
    const NAME: &'static str = "config.undo";
    type Params = ConfigUndoParams;
    type Result = ConfigUndoResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigUndoParams {
    #[ts(type = "number")]
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConfigUndoResult {
    /// The change, now marked undone.
    pub change: ConfigChange,
    /// What undoing it did.
    pub message: String,
}
