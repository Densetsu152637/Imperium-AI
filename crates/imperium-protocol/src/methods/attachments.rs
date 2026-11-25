use super::*;

/// `session.attach`: stages a file for the next message. Give `path` (a file on the daemon's
/// machine) or `data` (the file's bytes, base64). A file inside the session's folders or path
/// grants is referenced where it is; anything else is copied into the session's attachments
/// folder under the data directory, which the session's agents may read. Files over
/// `sessions.attachment_max_mib` are refused.
#[derive(Debug)]
pub struct SessionAttach;

impl Method for SessionAttach {
    const NAME: &'static str = "session.attach";
    type Params = SessionAttachParams;
    type Result = SessionAttachResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAttachParams {
    pub session_id: String,
    /// The file name to show; for `path`, its last component when empty.
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// The file's bytes, base64 (standard alphabet, padded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<String>,
    /// The file is an `@` mention: stage it only where the session may read it (inside its
    /// folders or path grants, in a remote runtime too) and no rule asks about or denies
    /// reading it. A file elsewhere is refused instead of copied into the attachments folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mention: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAttachResult {
    pub attachment: crate::session::Attachment,
    /// Whether the session's model can view images now; without vision an image is sent as a
    /// path reference.
    pub vision: bool,
    /// For an image of at most 4 MiB given by `path`: its bytes as a `data:` URL, so the client
    /// can draw a thumbnail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_data: Option<String>,
}

/// `session.files`: the picker behind `@` mentions. Searches the session's folders and the
/// project worktrees open beside them for files and directories whose path fuzzily matches
/// `query` (everything, shallowest first, when it is empty), best first. The listing honours
/// `.gitignore` and leaves out what a permission rule asks about or denies, so a client never
/// names a path the file tools would refuse; in a remote runtime it is made there. Give a
/// chosen file's `path` to `session.attach` with `mention: true`.
#[derive(Debug)]
pub struct SessionFiles;

impl Method for SessionFiles {
    const NAME: &'static str = "session.files";
    type Params = SessionFilesParams;
    type Result = SessionFilesResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionFilesParams {
    pub session_id: String,
    #[serde(default)]
    pub query: String,
    /// The most matches to return; 50 when absent, at most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionFilesResult {
    pub files: Vec<SessionFile>,
    /// More entries matched than were returned, or the search stopped early on a huge tree.
    pub truncated: bool,
}

/// A file or directory of a session's folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionFile {
    /// The absolute path, as `session.attach` takes it.
    pub path: String,
    /// The session folder it is in.
    pub folder: String,
    /// The path inside that folder, with `/` separators.
    pub relative: String,
    /// A directory: mentioned as a path, never attached.
    pub directory: bool,
}

/// `session.init`: the daemon's `/init`. Sends the built-in prompt that asks the agent to
/// analyse the project and write (or, when it exists, propose updates to) the project's
/// instruction file, `AGENTS.md`, as an ordinary user message: the agent explores and writes
/// with its normal tools, so permissions and approvals apply. `guidance` is the user's own
/// words to add to the prompt. Like `session.send` the message queues while the agent is busy
/// and waits on the session's budgets; the result is the same.
#[derive(Debug)]
pub struct SessionInit;

impl Method for SessionInit {
    const NAME: &'static str = "session.init";
    type Params = SessionInitParams;
    type Result = SessionSendResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionInitParams {
    pub session_id: String,
    /// What the user wants emphasised, such as `focus on the build and test commands`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub guidance: Option<String>,
    /// Go ahead although a budget in the session's scope is used up and says to ask first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confirm_budget: Option<bool>,
}

/// `session.builtin`: runs one of the built-in slash commands. Like `session.init`, which is
/// the same as `command: "init"`, the daemon sends the command's built-in prompt as an ordinary
/// user message and the agent works with its normal tools, so permissions and approvals apply
/// and nothing is edited unless the user approves it. `args` is what followed the command
/// (guidance for `init`; for the review commands an optional effort level, then an optional
/// target: a pull request number, a branch or a path, and the pending changes when absent).
/// Like `session.send` the message queues while the agent is busy and waits on the session's
/// budgets; the result is the same.
#[derive(Debug)]
pub struct SessionBuiltin;

impl Method for SessionBuiltin {
    const NAME: &'static str = "session.builtin";
    type Params = SessionBuiltinParams;
    type Result = SessionSendResult;
}

/// A built-in slash command that `session.builtin` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum BuiltinCommand {
    Init,
    CodeReview,
    SecurityReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionBuiltinParams {
    pub session_id: String,
    pub command: BuiltinCommand,
    /// The text after the command name; empty when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub args: Option<String>,
    /// Go ahead although a budget in the session's scope is used up and says to ask first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub confirm_budget: Option<bool>,
}

/// `voice.transcribe`: turns recorded speech into text through `[media.transcription]` (an
/// OpenAI-compatible `/audio/transcriptions` endpoint). The text is returned, not sent.
#[derive(Debug)]
pub struct VoiceTranscribe;

impl Method for VoiceTranscribe {
    const NAME: &'static str = "voice.transcribe";
    type Params = VoiceTranscribeParams;
    type Result = VoiceTranscribeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VoiceTranscribeParams {
    /// The recording, base64.
    pub audio: String,
    /// Its media type, such as `audio/webm` or `audio/wav`.
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VoiceTranscribeResult {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSendResult {
    /// The message's id; when it waits, `message_queued` carries the same id.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionQueueParams {
    pub session_id: String,
    /// `list`, `edit` (with `id` and `text`), `remove` (with `id`), or `steer` (with `id`).
    /// `steer` turns a queued message into steering for the running turn, in place: it leaves
    /// the queue when the agent reads it at its next safe point (or runs as the next turn,
    /// ahead of the other queued messages, when the turn ended first). Editing or removing a
    /// message that was already delivered fails, and editing one that is steering fails.
    #[ts(type = "\"list\" | \"edit\" | \"remove\" | \"steer\"")]
    pub action: String,
    /// A running subagent's task id; the root agent when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}

/// The agent's queue after the action, in delivery order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionQueueResult {
    pub messages: Vec<crate::session::QueuedMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionRespondParams {
    pub session_id: String,
    #[ts(type = "number")]
    pub request_id: u64,
    pub decision: PermissionDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAnswerParams {
    pub session_id: String,
    #[ts(type = "number")]
    pub request_id: u64,
    pub answers: Vec<String>,
}

/// An empty acknowledgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Accepted {}
