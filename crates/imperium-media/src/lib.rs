//! Media tools for agents: images through an OpenAI-compatible images API or a ComfyUI
//! workflow, speech through an OpenAI-compatible speech API, and transcription through an
//! OpenAI-compatible transcription API (LocalAI, whisper.cpp front-ends, or hosted services).
//! Results are files in the project's `media/` folder with a provenance file beside each.

pub mod image;
mod tools;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use imperium_api::ToolSpec;
use serde_json::{Value, json};

pub use image::{
    ComfyUi, ImageConfig, ImageFuture, ImageGenerator, ImageGeneratorBuilder, ImageGenerators,
    OpenAiImages,
};
pub use tools::MediaTools;

const TIMEOUT: Duration = Duration::from_secs(600);
const COMFY_POLL: Duration = Duration::from_millis(500);

/// An OpenAI-compatible service: base URL (ending before the API path) and optional key.
#[derive(Clone, PartialEq, Eq)]
pub struct Service {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

// The key stays out of logs and error messages.
impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Media {
    pub image: Option<Arc<dyn ImageGenerator>>,
    pub speech: Option<(Service, String)>,
    pub transcription: Option<Service>,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("{0} is not configured")]
    NotConfigured(&'static str),
    #[error("{0}")]
    Service(String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("{0} must be a path inside the project")]
    Outside(String),
}

fn service_error(error: reqwest::Error) -> MediaError {
    MediaError::Service(error.to_string())
}

impl Media {
    pub fn is_empty(&self) -> bool {
        self.image.is_none() && self.speech.is_none() && self.transcription.is_none()
    }

    /// Tool definitions for the configured backends.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        if self.image.is_some() {
            specs.push(ToolSpec {
                name: "media_image".into(),
                description: "Generate an image from a text prompt; it is saved in the project's media folder.".into(),
                parameters: json!({"type": "object", "properties": {
                    "prompt": {"type": "string"},
                    "width": {"type": "integer"}, "height": {"type": "integer"},
                    "name": {"type": "string", "description": "Short file name, without extension"}
                }, "required": ["prompt"]}),
            });
        }
        if self.speech.is_some() {
            specs.push(ToolSpec {
                name: "media_speak".into(),
                description: "Turn text into speech; the audio is saved in the project's media folder.".into(),
                parameters: json!({"type": "object", "properties": {
                    "text": {"type": "string"}, "voice": {"type": "string"}, "name": {"type": "string"}
                }, "required": ["text"]}),
            });
        }
        if self.transcription.is_some() {
            specs.push(ToolSpec {
                name: "media_transcribe".into(),
                description: "Transcribe an audio file in the project to text.".into(),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            });
        }
        specs
    }

    /// Generates an image into `<root>/media/`; returns the saved path relative to `root`.
    pub async fn image(
        &self,
        http: &reqwest::Client,
        root: &Path,
        prompt: &str,
        size: (u32, u32),
        name: Option<&str>,
    ) -> Result<String, MediaError> {
        let generator = self
            .image
            .as_ref()
            .ok_or(MediaError::NotConfigured("image generation"))?;
        let bytes = generator.generate(http, prompt, size).await?;
        let model = generator.model();
        save(
            root,
            name.unwrap_or(prompt),
            "png",
            &bytes,
            json!({
                "kind": "image", "prompt": prompt, "model": model, "width": size.0, "height": size.1,
            }),
        )
    }

    /// Speaks `text` into `<root>/media/`; returns the saved path relative to `root`.
    pub async fn speak(
        &self,
        http: &reqwest::Client,
        root: &Path,
        text: &str,
        voice: Option<&str>,
        name: Option<&str>,
    ) -> Result<String, MediaError> {
        let (service, default_voice) = self
            .speech
            .as_ref()
            .ok_or(MediaError::NotConfigured("speech"))?;
        let voice = voice.unwrap_or(default_voice);
        let response = authorized(http.post(format!("{}/audio/speech", service.base_url)), service)
            .json(&json!({"model": service.model, "input": text, "voice": voice, "response_format": "mp3"}))
            .timeout(TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(service_error)?;
        let bytes = response.bytes().await.map_err(service_error)?;
        save(
            root,
            name.unwrap_or(text),
            "mp3",
            &bytes,
            json!({
                "kind": "speech", "text": text, "voice": voice, "model": service.model,
            }),
        )
    }

    /// Transcribes an audio file inside `root`.
    pub async fn transcribe(
        &self,
        http: &reqwest::Client,
        root: &Path,
        path: &str,
    ) -> Result<String, MediaError> {
        let service = self
            .transcription
            .as_ref()
            .ok_or(MediaError::NotConfigured("transcription"))?;
        let relative = Path::new(path);
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(MediaError::Outside(path.into()));
        }
        let file = root.join(relative);
        let bytes = std::fs::read(&file).map_err(|error| MediaError::Io(path.into(), error))?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name(
            relative
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "audio".into()),
        );
        let form = reqwest::multipart::Form::new()
            .text("model", service.model.clone())
            .part("file", part);
        let body: Value = authorized(
            http.post(format!("{}/audio/transcriptions", service.base_url)),
            service,
        )
        .multipart(form)
        .timeout(TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(service_error)?
        .json()
        .await
        .map_err(service_error)?;
        body.get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| MediaError::Service("the transcription had no text".into()))
    }
}

fn authorized(request: reqwest::RequestBuilder, service: &Service) -> reqwest::RequestBuilder {
    match &service.api_key {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

/// Writes `bytes` to `<root>/media/<slug>-<time>.<ext>` with `<file>.json` provenance.
fn save(
    root: &Path,
    name: &str,
    extension: &str,
    bytes: &[u8],
    provenance: Value,
) -> Result<String, MediaError> {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let dir = root.join("media");
    std::fs::create_dir_all(&dir)
        .map_err(|error| MediaError::Io(dir.display().to_string(), error))?;
    let file_name = format!(
        "{}-{stamp}.{extension}",
        if slug.is_empty() { "media" } else { &slug }
    );
    let path: PathBuf = dir.join(&file_name);
    std::fs::write(&path, bytes)
        .map_err(|error| MediaError::Io(path.display().to_string(), error))?;
    let mut provenance = provenance;
    provenance["created_unix_ms"] = json!(stamp as u64);
    provenance["bytes"] = json!(bytes.len());
    let meta = dir.join(format!("{file_name}.json"));
    std::fs::write(
        &meta,
        serde_json::to_string_pretty(&provenance).unwrap_or_default(),
    )
    .map_err(|error| MediaError::Io(meta.display().to_string(), error))?;
    Ok(format!("media/{file_name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::{get, post};
    use base64::Engine;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        url
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake";

    #[tokio::test]
    async fn openai_compatible_images_speech_and_transcription() {
        let base = serve(Router::new()
            .route("/v1/images/generations", post(|axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(body["size"], "512x256");
                axum::Json(json!({"data": [{"b64_json": base64::engine::general_purpose::STANDARD.encode(PNG)}]}))
            }))
            .route("/v1/audio/speech", post(|| async { vec![1u8, 2, 3] }))
            .route("/v1/audio/transcriptions", post(|| async { axum::Json(json!({"text": "hello world"})) })))
        .await;
        let service = |model: &str| Service {
            base_url: format!("{base}/v1"),
            api_key: None,
            model: model.into(),
        };
        let media = Media {
            image: Some(Arc::new(OpenAiImages {
                service: service("sdxl"),
            })),
            speech: Some((service("tts-1"), "alloy".into())),
            transcription: Some(service("whisper-1")),
        };
        assert_eq!(media.tool_specs().len(), 3);
        let root = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        let image = media
            .image(&http, root.path(), "A red fox, at dawn!", (512, 256), None)
            .await
            .unwrap();
        assert!(
            image.starts_with("media/a-red-fox-at-dawn-") && image.ends_with(".png"),
            "{image}"
        );
        assert_eq!(std::fs::read(root.path().join(&image)).unwrap(), PNG);
        let provenance: Value = serde_json::from_str(
            &std::fs::read_to_string(root.path().join(format!("{image}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(provenance["prompt"], "A red fox, at dawn!");
        assert_eq!(provenance["model"], "sdxl");
        let audio = media
            .speak(&http, root.path(), "Hello", None, Some("greeting"))
            .await
            .unwrap();
        assert_eq!(std::fs::read(root.path().join(&audio)).unwrap(), [1, 2, 3]);
        assert_eq!(
            media.transcribe(&http, root.path(), &audio).await.unwrap(),
            "hello world"
        );
        assert!(matches!(
            media.transcribe(&http, root.path(), "../x.mp3").await,
            Err(MediaError::Outside(_))
        ));
    }

    #[tokio::test]
    async fn comfyui_workflows_get_the_prompt_and_return_the_first_image() {
        let base = serve(Router::new()
            .route("/prompt", post(|axum::Json(body): axum::Json<Value>| async move {
                assert_eq!(body["prompt"]["6"]["inputs"]["text"], "a \"quoted\" castle");
                assert_eq!(body["prompt"]["5"]["inputs"]["width"], 640);
                axum::Json(json!({"prompt_id": "p1"}))
            }))
            .route("/history/p1", get(|| async {
                axum::Json(json!({"p1": {"outputs": {"9": {"images": [{"filename": "out.png", "subfolder": "", "type": "output"}]}}}}))
            }))
            .route("/view", get(|| async { PNG.to_vec() })))
        .await;
        let workflow = r#"{"5": {"inputs": {"width": "{{width}}", "height": "{{height}}"}},
                          "6": {"inputs": {"text": "{{prompt}}", "seed": "{{seed}}"}}}"#;
        let media = Media {
            image: Some(Arc::new(ComfyUi {
                url: base,
                workflow: workflow.into(),
            })),
            ..Media::default()
        };
        let root = tempfile::tempdir().unwrap();
        let path = media
            .image(
                &reqwest::Client::new(),
                root.path(),
                "a \"quoted\" castle",
                (640, 480),
                Some("castle"),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(root.path().join(&path)).unwrap(), PNG);
        let provenance: Value = serde_json::from_str(
            &std::fs::read_to_string(root.path().join(format!("{path}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(provenance["model"], "comfyui workflow");
    }

    /// Returns the prompt's bytes as the "image".
    #[derive(Debug)]
    struct Echo;

    impl ImageGenerator for Echo {
        fn model(&self) -> &str {
            "echo"
        }

        fn generate<'a>(
            &'a self,
            _http: &'a reqwest::Client,
            prompt: &'a str,
            size: (u32, u32),
        ) -> ImageFuture<'a> {
            Box::pin(async move { Ok(format!("{prompt} {}x{}", size.0, size.1).into_bytes()) })
        }
    }

    fn no_service() -> Option<Service> {
        None
    }

    #[tokio::test]
    async fn custom_generators_plug_into_the_registry_and_media() {
        let mut generators = ImageGenerators::default();
        generators.register("echo", |_| Some(Arc::new(Echo) as Arc<dyn ImageGenerator>));
        let config = ImageConfig {
            comfyui_url: None,
            workflow: None,
            service: &no_service,
        };
        let media = Media {
            image: generators.build("echo", &config),
            ..Media::default()
        };
        assert_eq!(media.tool_specs()[0].name, "media_image");
        let root = tempfile::tempdir().unwrap();
        let path = media
            .image(&reqwest::Client::new(), root.path(), "a fox", (8, 4), None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(root.path().join(&path)).unwrap(),
            b"a fox 8x4"
        );
        let provenance: Value = serde_json::from_str(
            &std::fs::read_to_string(root.path().join(format!("{path}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(provenance["model"], "echo");
    }

    #[test]
    fn built_in_generators_need_their_settings_and_unknown_names_use_openai() {
        let generators = ImageGenerators::default();
        let resolved = std::sync::atomic::AtomicUsize::new(0);
        let missing = || {
            resolved.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        };
        let empty = ImageConfig {
            comfyui_url: None,
            workflow: None,
            service: &missing,
        };
        for name in ["comfyui", "openai", "anything"] {
            assert!(generators.build(name, &empty).is_none(), "{name}");
        }
        assert_eq!(
            resolved.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "comfyui does not resolve the endpoint"
        );

        let dir = tempfile::tempdir().unwrap();
        let workflow = dir.path().join("workflow.json");
        std::fs::write(&workflow, r#"{"6": {"inputs": {"text": "{{prompt}}"}}}"#).unwrap();
        let service = || {
            Some(Service {
                base_url: "http://127.0.0.1:8080/v1".into(),
                api_key: None,
                model: "sdxl".into(),
            })
        };
        let configured = ImageConfig {
            comfyui_url: Some("http://127.0.0.1:9000/"),
            workflow: Some(&workflow),
            service: &service,
        };
        assert_eq!(
            generators.build("comfyui", &configured).unwrap().model(),
            "comfyui workflow"
        );
        assert_eq!(
            generators.build("anything", &configured).unwrap().model(),
            "sdxl"
        );
    }
}
