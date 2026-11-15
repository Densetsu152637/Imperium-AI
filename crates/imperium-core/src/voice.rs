//! Push-to-talk transcription on this machine: a whisper.cpp `whisper-server` the daemon
//! supervises ([`imperium_local::speech`]), used by `voice.transcribe` when
//! `[voice] transcription = "local"`.
//!
//! The server starts on the first recording, keeps running while recordings come, and stops
//! when idle or when a language model needs its memory. Its memory is counted beside the
//! language models' when they are supervised too. A failure (no binary, no model, no room, a
//! server that did not start or answer) reaches the user as the push-to-talk error.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use imperium_config::LocalWhisperSettings;
use imperium_local::speech::{
    ProcessSpeechLauncher, SpeechConfig, SpeechError, SpeechServer, WATCH, find_binary,
};
use imperium_media::{Media, Service};
use imperium_protocol::jsonrpc::{ErrorObject, codes};

use crate::state::DaemonState;

#[cfg(test)]
#[path = "voice_tests.rs"]
mod tests;

/// The model name sent to the server: it has one model loaded and ignores the name.
const MODEL_NAME: &str = "whisper";

/// Transcribes a recording with the local whisper, starting it first if it is not running.
pub(crate) async fn transcribe(
    state: &Arc<DaemonState>,
    audio: Vec<u8>,
    file_name: &str,
) -> Result<String, ErrorObject> {
    let server = server(state).await?;
    // The runtime of language models may have appeared since the last recording.
    if let Some(supervisor) = state.local_supervisor() {
        server.attach(&supervisor);
    }
    let lease = server.acquire().await.map_err(speech_error)?;
    let media = Media {
        transcription: Some(Service {
            base_url: lease.base_url().to_string(),
            api_key: None,
            model: MODEL_NAME.into(),
        }),
        ..Media::default()
    };
    match media
        .transcribe_bytes(&reqwest::Client::new(), audio, file_name)
        .await
    {
        Ok(text) => Ok(text),
        Err(error) => {
            // A refused request (4xx) is the recording's fault, not the server's.
            if !error.to_string().contains("client error") {
                lease.failed();
            }
            Err(ErrorObject::new(
                codes::INTERNAL_ERROR,
                format!(
                    "local speech-to-text failed: {error} (see {}; with voice.local.convert on, \
                     recordings are decoded by ffmpeg, which must be installed)",
                    server.config().log.display()
                ),
            ))
        }
    }
}

/// The supervised whisper, built from `[voice.local]` the first time it is needed.
async fn server(state: &Arc<DaemonState>) -> Result<Arc<SpeechServer>, ErrorObject> {
    if let Some(server) = state.speech_server() {
        return Ok(server);
    }
    let settings = state.settings();
    let local = &settings.voice.local;
    let model = model_path(state, local).await?;
    let config = SpeechConfig {
        binary: find_binary(local.binary.as_deref()).map_err(speech_error)?,
        model,
        gpu: local.gpu != "off",
        threads: local.threads,
        language: local.language.clone(),
        convert: local.convert,
        extra_args: local.extra_args.clone(),
        idle_stop: Duration::from_secs(local.idle_stop_s),
        start_timeout: Duration::from_secs(local.start_timeout_s),
        log: state.data_dir().join("logs").join("whisper.log"),
        watch: WATCH,
    };
    let built = SpeechServer::new(config, Arc::new(ProcessSpeechLauncher::new()));
    // Two recordings may arrive together: the first server built is the one used.
    Ok(Arc::clone(state.speech_slot().get_or_init(|| built)))
}

/// Downloads run one at a time, so two recordings do not write the same file.
static DOWNLOAD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The model file: `voice.local.model`, or downloaded through the Hugging Face path when
/// `voice.local.download` is on.
async fn model_path(
    state: &Arc<DaemonState>,
    local: &LocalWhisperSettings,
) -> Result<PathBuf, ErrorObject> {
    if let Some(model) = local
        .model
        .as_ref()
        .filter(|model| !model.as_os_str().is_empty())
    {
        return Ok(model.clone());
    }
    if !local.download {
        return Err(ErrorObject::new(
            codes::INVALID_PARAMS,
            "local speech-to-text needs a whisper model: set voice.local.model to a ggml model \
             file, or turn on voice.local.download to fetch one",
        ));
    }
    let _one_at_a_time = DOWNLOAD.lock().await;
    let hub = state.hub();
    let failed = |error: imperium_local::hf::HfError| {
        ErrorObject::new(
            codes::INTERNAL_ERROR,
            format!("could not download the whisper model: {error}"),
        )
    };
    let revision = "main";
    let files = hub
        .files(&local.model_repo, revision)
        .await
        .map_err(failed)?;
    let file = files
        .into_iter()
        .find(|file| file.path == local.model_file)
        .ok_or_else(|| {
            ErrorObject::new(
                codes::INVALID_PARAMS,
                format!(
                    "{} has no file {} (voice.local.model_file)",
                    local.model_repo, local.model_file
                ),
            )
        })?;
    let dir = crate::downloads::destination(state, &local.model_repo);
    let paths = hub
        .download(
            &local.model_repo,
            revision,
            std::slice::from_ref(&file),
            &dir,
            |_| {},
        )
        .await
        .map_err(failed)?;
    paths.into_iter().next().ok_or_else(|| {
        ErrorObject::new(
            codes::INTERNAL_ERROR,
            "the whisper model download returned no file",
        )
    })
}

fn speech_error(error: SpeechError) -> ErrorObject {
    let code = match error {
        SpeechError::Setup(_) | SpeechError::NoRoom { .. } => codes::INVALID_PARAMS,
        SpeechError::Start(_) | SpeechError::TimedOut(_) => codes::INTERNAL_ERROR,
    };
    ErrorObject::new(code, format!("local speech-to-text: {error}"))
}
