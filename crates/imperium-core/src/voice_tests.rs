//! Local push-to-talk transcription through a whisper-server that runs inside the test: the
//! speech server's launcher is replaced, and the OpenAI-compatible route is served in this
//! process. Nothing starts a real server, reads a GPU or downloads a model.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use base64::Engine;
use imperium_config::{Config, Layer, Paths, Settings, Source};
use imperium_local::speech::{
    LaunchFuture, SpeechConfig, SpeechError, SpeechLaunch, SpeechLauncher, SpeechProcess,
    SpeechServer, StopFuture,
};
use imperium_protocol::methods::VoiceTranscribeParams;

use crate::state::DaemonState;

/// What the fake whisper-server saw and how it answers.
#[derive(Default)]
struct Fake {
    /// The bodies of the transcription requests, in order.
    requests: Mutex<Vec<Vec<u8>>>,
    /// Answer with an error instead of a transcription.
    broken: AtomicBool,
    launches: AtomicU32,
    stops: AtomicU32,
    /// Launches fail with this message.
    launch_error: Mutex<Option<String>>,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fake")
    }
}

async fn transcriptions(State(fake): State<Arc<Fake>>, body: Bytes) -> (StatusCode, String) {
    fake.requests.lock().unwrap().push(body.to_vec());
    if fake.broken.load(Ordering::SeqCst) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "{\"error\":\"boom\"}".into(),
        );
    }
    (StatusCode::OK, "{\"text\":\"  hello there  \"}".into())
}

/// Serves the OpenAI-compatible route in this process, as whisper-server would.
async fn serve(fake: &Arc<Fake>) -> String {
    let app = Router::new()
        .route("/v1/audio/transcriptions", post(transcriptions))
        .with_state(Arc::clone(fake));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    root
}

#[derive(Debug)]
struct Launcher {
    fake: Arc<Fake>,
    root: String,
}

struct Process {
    fake: Arc<Fake>,
    base_url: String,
}

impl SpeechProcess for Process {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn pid(&self) -> Option<u32> {
        None
    }

    fn exited(&self) -> Option<String> {
        None
    }

    fn stop(&self) -> StopFuture<'_> {
        Box::pin(async move {
            self.fake.stops.fetch_add(1, Ordering::SeqCst);
        })
    }
}

impl SpeechLauncher for Launcher {
    fn launch<'a>(&'a self, _spec: &'a SpeechLaunch) -> LaunchFuture<'a> {
        Box::pin(async move {
            self.fake.launches.fetch_add(1, Ordering::SeqCst);
            let failure = self.fake.launch_error.lock().unwrap().clone();
            if let Some(message) = failure {
                return Err(SpeechError::Start(message));
            }
            let process: Box<dyn SpeechProcess> = Box::new(Process {
                fake: Arc::clone(&self.fake),
                base_url: format!("{}/v1", self.root),
            });
            Ok(process)
        })
    }
}

fn settings(extra: &str) -> Settings {
    let config = format!("[voice]\ntranscription = \"local\"\n{extra}");
    Config::from_layers([
        Layer::defaults(),
        Layer::new(Source::User("test".into()), config.parse().unwrap()),
    ])
    .settings()
    .unwrap()
}

struct Setup {
    state: Arc<DaemonState>,
    fake: Arc<Fake>,
    _home: tempfile::TempDir,
}

/// A daemon set to transcribe locally, with a whisper-server that runs in this process.
async fn setup() -> Setup {
    let home = tempfile::tempdir().unwrap();
    let model = home.path().join("ggml-test.bin");
    std::fs::write(&model, vec![0u8; 1024]).unwrap();
    let settings = settings("");
    let state = DaemonState::new(Paths::under(home.path()), settings, None);
    let fake = Arc::new(Fake::default());
    let root = serve(&fake).await;
    let config = SpeechConfig {
        binary: PathBuf::from("whisper-server"),
        model,
        gpu: true,
        threads: 0,
        language: "auto".into(),
        convert: true,
        extra_args: Vec::new(),
        idle_stop: Duration::ZERO,
        start_timeout: Duration::from_secs(5),
        log: home.path().join("whisper.log"),
        watch: Duration::from_millis(50),
    };
    let launcher = Arc::new(Launcher {
        fake: Arc::clone(&fake),
        root,
    });
    assert!(state.use_speech_server(SpeechServer::new(config, launcher)));
    Setup {
        state,
        fake,
        _home: home,
    }
}

fn recording(bytes: &[u8]) -> VoiceTranscribeParams {
    VoiceTranscribeParams {
        audio: base64::engine::general_purpose::STANDARD.encode(bytes),
        mime: "audio/webm;codecs=opus".into(),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[tokio::test]
async fn a_recording_is_transcribed_by_the_local_server() {
    let setup = setup().await;
    let audio = b"opus-bytes-of-a-recording";
    let result = crate::attachments::transcribe(Arc::clone(&setup.state), recording(audio))
        .await
        .unwrap();
    assert_eq!(result.text, "hello there", "trimmed");
    {
        let requests = setup.fake.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(
            contains(&requests[0], audio),
            "the audio reached the server"
        );
        assert!(
            contains(&requests[0], b"speech.webm"),
            "named for its format"
        );
    }
    // A second recording reuses the running server.
    crate::attachments::transcribe(Arc::clone(&setup.state), recording(audio))
        .await
        .unwrap();
    assert_eq!(setup.fake.launches.load(Ordering::SeqCst), 1);
    assert!(setup.state.speech_server().unwrap().status().running);
}

#[tokio::test]
async fn a_server_that_will_not_start_is_reported_to_the_user() {
    let setup = setup().await;
    *setup.fake.launch_error.lock().unwrap() =
        Some("whisper-server exited while loading (exit code: 1); see whisper.log".into());
    let error = crate::attachments::transcribe(Arc::clone(&setup.state), recording(b"abc"))
        .await
        .unwrap_err();
    assert!(
        error.message.contains("local speech-to-text")
            && error.message.contains("exited while loading"),
        "{}",
        error.message
    );
    assert!(setup.fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_transcription_is_reported_and_the_server_is_restarted() {
    let setup = setup().await;
    setup.fake.broken.store(true, Ordering::SeqCst);
    let error = crate::attachments::transcribe(Arc::clone(&setup.state), recording(b"abc"))
        .await
        .unwrap_err();
    assert!(
        error.message.contains("local speech-to-text failed"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("ffmpeg"),
        "points at the usual cause: {}",
        error.message
    );
    // The next recording starts a fresh server, which works.
    setup.fake.broken.store(false, Ordering::SeqCst);
    let result = crate::attachments::transcribe(Arc::clone(&setup.state), recording(b"abc"))
        .await
        .unwrap();
    assert_eq!(result.text, "hello there");
    assert_eq!(setup.fake.launches.load(Ordering::SeqCst), 2);
    assert_eq!(setup.fake.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn local_transcription_without_a_model_says_what_to_set() {
    let home = tempfile::tempdir().unwrap();
    let state = DaemonState::new(Paths::under(home.path()), settings(""), None);
    let error = crate::attachments::transcribe(state, recording(b"abc"))
        .await
        .unwrap_err();
    assert!(
        error.message.contains("voice.local.model")
            && error.message.contains("voice.local.download"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn hosted_stays_the_default_and_points_at_the_local_option() {
    let home = tempfile::tempdir().unwrap();
    let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
    assert_eq!(
        settings.voice.transcription,
        imperium_config::VoiceTranscription::Hosted
    );
    let state = DaemonState::new(Paths::under(home.path()), settings, None);
    let error = crate::attachments::transcribe(state, recording(b"abc"))
        .await
        .unwrap_err();
    assert!(
        error.message.contains("[media.transcription]")
            && error.message.contains("voice.transcription"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn a_recording_that_is_not_base64_is_refused_before_anything_starts() {
    let setup = setup().await;
    let error = crate::attachments::transcribe(
        Arc::clone(&setup.state),
        VoiceTranscribeParams {
            audio: "not base64!".into(),
            mime: "audio/webm".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("base64"), "{}", error.message);
    assert_eq!(setup.fake.launches.load(Ordering::SeqCst), 0);
}
