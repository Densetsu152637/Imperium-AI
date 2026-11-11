//! `session.attach`, the attachments `session.send` carries, and `voice.transcribe`.
//!
//! Each session has an attachments folder, `<data>/attachments/<session id>/`, granted to its
//! tools read-only (local workspaces). A file inside the session's folders or path grants is
//! referenced where it is; a pasted, uploaded, or outside file is copied into the folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use imperium_engine::Session;
use imperium_media::attachments::{self, AttachmentError, Source, Staging};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    SessionAttachParams, SessionAttachResult, VoiceTranscribeParams, VoiceTranscribeResult,
};
use imperium_protocol::session::Attachment;

use crate::state::DaemonState;

/// The longest push-to-talk recording accepted, in bytes (about 25 minutes of Opus speech).
const MAX_RECORDING_BYTES: usize = 25 * 1024 * 1024;

/// A session's attachments folder.
pub fn store_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join("attachments").join(session_id)
}

/// A fresh attachment id, unique in the daemon's lifetime and across restarts.
fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    format!("a{millis:x}{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// The largest attachment `sessions.attachment_max_mib` allows, in bytes.
fn max_bytes(state: &DaemonState) -> u64 {
    u64::from(state.settings().sessions.attachment_max_mib.max(1)) * 1024 * 1024
}

/// Whether the session's tools may read `path` (canonical) where it is: inside its folders or
/// path grants on this machine. A remote runtime's folders are not on this machine.
fn readable(session: &Session) -> impl Fn(&Path) -> bool + '_ {
    let workspace = session.workspace();
    move |path: &Path| {
        workspace.runtime().is_none() && workspace.resolve(&path.to_string_lossy()).is_ok()
    }
}

fn session_vision(state: &DaemonState, session: &Session) -> bool {
    session
        .model()
        .is_some_and(|model| state.model_has_vision(&model))
}

fn invalid(error: AttachmentError) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, error.to_string())
}

/// `session.attach`.
pub fn attach(
    state: &DaemonState,
    session: &Session,
    params: SessionAttachParams,
) -> Result<SessionAttachResult, ErrorObject> {
    let source = match (params.path, params.data) {
        (Some(path), None) => Source::Path(PathBuf::from(path)),
        (None, Some(data)) => Source::Base64(data),
        _ => return Err(invalid(AttachmentError::NoSource)),
    };
    let store = store_dir(state.data_dir(), session.id());
    let readable = readable(session);
    let staging = Staging {
        store: &store,
        readable: &readable,
        max_bytes: max_bytes(state),
    };
    let attachment =
        attachments::stage(&staging, &next_id(), &params.name, source).map_err(invalid)?;
    let image_data = attachments::image_data(&attachment);
    Ok(SessionAttachResult {
        vision: session_vision(state, session),
        attachment,
        image_data,
    })
}

/// The attachments of a `session.send`, checked again and marked with whether the session's
/// model sees images.
pub fn verify(
    state: &DaemonState,
    session: &Session,
    sent: &[Attachment],
) -> Result<Vec<Attachment>, ErrorObject> {
    if sent.is_empty() {
        return Ok(Vec::new());
    }
    let store = store_dir(state.data_dir(), session.id());
    let readable = readable(session);
    let staging = Staging {
        store: &store,
        readable: &readable,
        max_bytes: max_bytes(state),
    };
    attachments::verify_all(&staging, sent, session_vision(state, session)).map_err(invalid)
}

/// `voice.transcribe`: through `[media.transcription]`.
pub async fn transcribe(
    state: Arc<DaemonState>,
    params: VoiceTranscribeParams,
) -> Result<VoiceTranscribeResult, ErrorObject> {
    let media = state
        .media_backends()
        .filter(|media| media.transcription.is_some())
        .ok_or_else(|| {
            ErrorObject::new(
                codes::INVALID_PARAMS,
                "speech-to-text is not set up: add [media.transcription] (an OpenAI-compatible \
                 /audio/transcriptions endpoint) to the configuration",
            )
        })?;
    if params.audio.len() / 4 * 3 > MAX_RECORDING_BYTES {
        return Err(ErrorObject::new(
            codes::INVALID_PARAMS,
            "the recording is too long",
        ));
    }
    let audio = base64::engine::general_purpose::STANDARD
        .decode(params.audio.trim())
        .map_err(|_| {
            ErrorObject::new(codes::INVALID_PARAMS, "the recording is not valid base64")
        })?;
    if audio.is_empty() {
        return Err(ErrorObject::new(
            codes::INVALID_PARAMS,
            "the recording is empty",
        ));
    }
    let text = media
        .transcribe_bytes(
            &reqwest::Client::new(),
            audio,
            imperium_media::recording_name(&params.mime),
        )
        .await
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?;
    Ok(VoiceTranscribeResult {
        text: text.trim().to_string(),
    })
}
