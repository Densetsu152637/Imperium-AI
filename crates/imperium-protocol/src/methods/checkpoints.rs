use super::*;

// ---- Checkpoints and rewinding ----

/// A checkpoint of a session's files: one commit per git repository the session's folders are
/// in, on a private ref (`refs/imperium/checkpoints/<session>/<number>`), taken without touching
/// the repository's index, HEAD, or branches. It holds every file git would see (tracked and
/// untracked), not ignored ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpoint {
    /// 1 for the session's first checkpoint, counting up.
    pub number: u32,
    /// When it was taken, in Unix milliseconds.
    #[ts(type = "number")]
    pub at_unix_ms: u64,
    /// `turn`: the files before a root turn that changed them. `rewind`: the files just before
    /// a rewind, so the rewind can be undone by rewinding to it.
    #[ts(type = "\"turn\" | \"rewind\"")]
    pub kind: String,
    /// For `turn`: the message the turn started with.
    #[serde(default)]
    pub prompt: String,
    /// One commit per repository.
    pub repos: Vec<CheckpointRepo>,
    /// The conversation as it was can be rewound to (`turn` checkpoints).
    #[serde(default)]
    pub conversation: bool,
}

/// One repository's commit in a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckpointRepo {
    /// The top folder of the repository's working tree (a linked worktree's own folder).
    pub root: String,
    /// The checkpoint commit (its parent is HEAD as it was then).
    pub commit: String,
    /// The private ref that keeps it.
    pub reference: String,
}

/// Notification sent to every client when a session records a checkpoint; the payload is
/// [`SessionCheckpointParams`].
pub const SESSION_CHECKPOINT: &str = "session.checkpoint";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpointParams {
    pub session_id: String,
    pub checkpoint: SessionCheckpoint,
}

/// `session.checkpoints`: a session's checkpoints, oldest first.
#[derive(Debug)]
pub struct SessionCheckpoints;

impl Method for SessionCheckpoints {
    const NAME: &'static str = "session.checkpoints";
    type Params = SessionCheckpointsParams;
    type Result = SessionCheckpointsResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpointsParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionCheckpointsResult {
    /// Whether the session records checkpoints: its folders are on this machine, at least one
    /// is in a git repository, and `sessions.checkpoints` is on.
    pub available: bool,
    /// Why not, when `available` is false.
    #[serde(default)]
    pub reason: Option<String>,
    pub checkpoints: Vec<SessionCheckpoint>,
}

/// What `session.rewind` puts back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RewindMode {
    /// The files and the conversation.
    #[default]
    Both,
    /// Only the files; the conversation goes on as it is.
    Files,
    /// Only the conversation; the files stay as they are.
    Conversation,
}

/// `session.rewind`: puts a session back to a checkpoint. Files: every file git would see in
/// the session's repositories is restored to the checkpoint (changed and deleted files written
/// back, files added since removed); ignored files, the index, HEAD, and branches are left
/// alone, and the files as they were are first recorded as a `rewind` checkpoint, so the rewind
/// can be undone. Conversation: the session is forked at the checkpoint into a new session
/// whose conversation ends just before that turn's message (returned, to edit and send again);
/// the original session is kept as it is. Refused while a turn is running.
#[derive(Debug)]
pub struct SessionRewind;

impl Method for SessionRewind {
    const NAME: &'static str = "session.rewind";
    type Params = SessionRewindParams;
    type Result = SessionRewindResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionRewindParams {
    pub session_id: String,
    /// The checkpoint's number.
    pub checkpoint: u32,
    #[serde(default)]
    pub mode: RewindMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionRewindResult {
    /// Files written back (changed or deleted since the checkpoint), absolute.
    pub restored: Vec<String>,
    /// Files removed (added since the checkpoint), absolute.
    pub removed: Vec<String>,
    /// The `rewind` checkpoint holding the files as they were before, when files were rewound.
    #[serde(default)]
    pub undo_checkpoint: Option<u32>,
    /// The new session holding the conversation up to the checkpoint, when the conversation
    /// was rewound.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The message the rewound turn started with, to edit and send again.
    #[serde(default)]
    pub prompt: Option<String>,
}
