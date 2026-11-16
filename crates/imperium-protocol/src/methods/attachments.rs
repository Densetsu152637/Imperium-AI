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
