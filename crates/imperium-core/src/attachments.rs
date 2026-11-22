//! `session.attach`, the attachments `session.send` carries, and `voice.transcribe`.
//!
//! Each session has an attachments folder, `<data>/attachments/<session id>/`, granted to its
//! tools read-only (local workspaces). A file inside the session's folders or path grants is
//! referenced where it is; a pasted, uploaded, or outside file is copied into the folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use imperium_config::VoiceTranscription;
use imperium_engine::Session;
use imperium_media::attachments::{self, AttachmentError, Source, Staging};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    SessionAttachParams, SessionAttachResult, SessionFile, SessionFilesParams, SessionFilesResult,
    VoiceTranscribeParams, VoiceTranscribeResult,
};
use imperium_protocol::session::Attachment;

use crate::state::DaemonState;

/// The longest push-to-talk recording accepted, in bytes (about 25 minutes of Opus speech).
const MAX_RECORDING_BYTES: usize = 25 * 1024 * 1024;

/// A session's attachments folder.
pub(crate) fn store_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join("attachments").join(session_id)
}

/// A fresh attachment id, unique in the daemon's lifetime and across restarts.
fn next_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = imperium_api::time::unix_ms();
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
pub(crate) async fn attach(
    state: &DaemonState,
    session: &Session,
    params: SessionAttachParams,
) -> Result<SessionAttachResult, ErrorObject> {
    let max = max_bytes(state);
    let source = match (&params.path, &params.data) {
        (Some(_), None) | (None, Some(_)) => {
            if params.mention == Some(true) {
                mention_source(session, &params, max).await?
            } else if let Some(path) = &params.path {
                Source::Path(PathBuf::from(path))
            } else {
                Source::Base64(params.data.clone().unwrap_or_default())
            }
        }
        _ => return Err(invalid(AttachmentError::NoSource)),
    };
    let store = store_dir(state.data_dir(), session.id());
    let readable = readable(session);
    let staging = Staging {
        store: &store,
        readable: &readable,
        max_bytes: max,
    };
    // A mention read in a remote runtime is named after its file.
    let name = match (&params.path, params.name.trim().is_empty()) {
        (Some(path), true) if params.mention == Some(true) => path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_string(),
        _ => params.name.clone(),
    };
    let attachment = attachments::stage(&staging, &next_id(), &name, source).map_err(invalid)?;
    let image_data = attachments::image_data(&attachment);
    Ok(SessionAttachResult {
        vision: session_vision(state, session),
        attachment,
        image_data,
    })
}

/// Where an `@` mention's file comes from. A mention is held to what the file tools allow: the
/// path must be inside the session's folders or path grants and no permission rule may ask
/// about or deny reading it. In a local workspace the file is then referenced where it is; one
/// in a remote runtime is read there and copied into the attachments folder.
async fn mention_source(
    session: &Session,
    params: &SessionAttachParams,
    max_bytes: u64,
) -> Result<Source, ErrorObject> {
    let Some(path) = &params.path else {
        return Err(ErrorObject::new(
            codes::INVALID_PARAMS,
            "a mention names a path",
        ));
    };
    let workspace = session.workspace();
    let resolved = workspace
        .resolve(path)
        .map_err(|error| ErrorObject::new(codes::INVALID_PARAMS, error.to_string()))?;
    if imperium_tools::mention_withheld(workspace, path) {
        return Err(ErrorObject::new(
            codes::INVALID_PARAMS,
            format!(
                "{path}: a permission rule asks about or denies reading it; ask the agent to read it so the rule applies"
            ),
        ));
    }
    if workspace.runtime().is_none() {
        return Ok(Source::Path(resolved));
    }
    let bytes = imperium_tools::read_remote_file(workspace, path, max_bytes)
        .await
        .map_err(|error| ErrorObject::new(codes::INVALID_PARAMS, error.to_string()))?;
    Ok(Source::Base64(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    ))
}

/// `session.files`: the session's files and directories that fuzzily match the query, for the
/// `@` picker. The search honours `.gitignore` and the permission rules.
pub(crate) async fn files(
    session: &Session,
    params: SessionFilesParams,
) -> Result<SessionFilesResult, ErrorObject> {
    let limit = params.limit.map_or(50, |limit| limit as usize);
    let found = imperium_tools::find_files(session.workspace(), &params.query, limit)
        .await
        .map_err(|error| ErrorObject::new(codes::INVALID_PARAMS, error.to_string()))?;
    Ok(SessionFilesResult {
        files: found
            .matches
            .into_iter()
            .map(|entry| SessionFile {
                path: entry.path,
                folder: entry.folder,
                relative: entry.relative,
                directory: entry.directory,
            })
            .collect(),
        truncated: found.truncated,
    })
}

/// The attachments of a `session.send`, checked again and marked with whether the session's
/// model sees images.
pub(crate) fn verify(
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

/// `voice.transcribe`: through `[media.transcription]`, or, with `[voice] transcription =
/// "local"`, the whisper.cpp server the daemon supervises ([`crate::voice`]).
pub(crate) async fn transcribe(
    state: Arc<DaemonState>,
    params: VoiceTranscribeParams,
) -> Result<VoiceTranscribeResult, ErrorObject> {
    let local = state.settings().voice.transcription == VoiceTranscription::Local;
    let media = if local {
        None
    } else {
        Some(
            state
                .media_backends()
                .filter(|media| media.transcription.is_some())
                .ok_or_else(|| {
                    ErrorObject::new(
                        codes::INVALID_PARAMS,
                        "speech-to-text is not set up: add [media.transcription] (an \
                         OpenAI-compatible /audio/transcriptions endpoint) to the \
                         configuration, or set voice.transcription = \"local\"",
                    )
                })?,
        )
    };
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
    let name = imperium_media::recording_name(&params.mime);
    let text = match media {
        Some(media) => media
            .transcribe_bytes(&reqwest::Client::new(), audio, name)
            .await
            .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?,
        None => crate::voice::transcribe(&state, audio, name).await?,
    };
    Ok(VoiceTranscribeResult {
        text: text.trim().to_string(),
    })
}
