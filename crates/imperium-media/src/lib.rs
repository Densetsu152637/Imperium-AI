//! Media tools for agents: images through an OpenAI-compatible images API or a ComfyUI
//! workflow, speech through an OpenAI-compatible speech API, and transcription through an
//! OpenAI-compatible transcription API (LocalAI, whisper.cpp front-ends, or hosted services).
//! Results are files in the project's `media/` folder with a provenance file beside each.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use imperium_api::ToolSpec;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(600);
const COMFY_POLL: Duration = Duration::from_millis(500);

/// An OpenAI-compatible service: base URL (ending before the API path) and optional key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

/// How images are made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageBackend {
    OpenAi(Service),
    /// A ComfyUI server and an API-format workflow whose text contains `{{prompt}}` (and
    /// optionally `{{seed}}`, `{{width}}`, `{{height}}`).
    ComfyUi {
        url: String,
        workflow: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct Media {
    pub image: Option<ImageBackend>,
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
        let backend = self
            .image
            .as_ref()
            .ok_or(MediaError::NotConfigured("image generation"))?;
        let (bytes, model) = match backend {
            ImageBackend::OpenAi(service) => (
                openai_image(http, service, prompt, size).await?,
                service.model.clone(),
            ),
            ImageBackend::ComfyUi { url, workflow } => (
                comfyui_image(http, url, workflow, prompt, size).await?,
                "comfyui workflow".to_string(),
            ),
        };
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

async fn openai_image(
    http: &reqwest::Client,
    service: &Service,
    prompt: &str,
    (width, height): (u32, u32),
) -> Result<Vec<u8>, MediaError> {
    let body: Value = authorized(
        http.post(format!("{}/images/generations", service.base_url)),
        service,
    )
    .json(&json!({
        "model": service.model, "prompt": prompt, "n": 1,
        "size": format!("{width}x{height}"), "response_format": "b64_json",
    }))
    .timeout(TIMEOUT)
    .send()
    .await
    .and_then(reqwest::Response::error_for_status)
    .map_err(service_error)?
    .json()
    .await
    .map_err(service_error)?;
    let image = &body["data"][0];
    if let Some(encoded) = image["b64_json"].as_str() {
        return base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| {
                MediaError::Service(format!("the image was not valid base64: {error}"))
            });
    }
    let url = image["url"]
        .as_str()
        .ok_or_else(|| MediaError::Service("the response had no image".into()))?;
    Ok(http
        .get(url)
        .send()
        .await
        .map_err(service_error)?
        .bytes()
        .await
        .map_err(service_error)?
        .to_vec())
}

/// Queues the workflow with the prompt filled in, waits for it, and downloads the first image.
async fn comfyui_image(
    http: &reqwest::Client,
    url: &str,
    workflow: &str,
    prompt: &str,
    (width, height): (u32, u32),
) -> Result<Vec<u8>, MediaError> {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() % 1_000_000_007);
    let quoted =
        serde_json::to_string(prompt).map_err(|error| MediaError::Service(error.to_string()))?;
    let filled = workflow
        .replace("\"{{prompt}}\"", &quoted)
        .replace("{{prompt}}", quoted.trim_matches('"'))
        .replace("\"{{seed}}\"", &seed.to_string())
        .replace("\"{{width}}\"", &width.to_string())
        .replace("\"{{height}}\"", &height.to_string());
    let graph: Value = serde_json::from_str(&filled).map_err(|error| {
        MediaError::Service(format!("the ComfyUI workflow is not valid JSON: {error}"))
    })?;
    let queued: Value = http
        .post(format!("{url}/prompt"))
        .json(&json!({"prompt": graph, "client_id": "imperium"}))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(service_error)?
        .json()
        .await
        .map_err(service_error)?;
    let id = queued["prompt_id"]
        .as_str()
        .ok_or_else(|| MediaError::Service("ComfyUI did not queue the workflow".into()))?;
    let started = std::time::Instant::now();
    let image = loop {
        let history: Value = http
            .get(format!("{url}/history/{id}"))
            .send()
            .await
            .map_err(service_error)?
            .json()
            .await
            .map_err(service_error)?;
        let outputs = history[id]["outputs"].as_object();
        if let Some(image) = outputs
            .into_iter()
            .flat_map(|outputs| outputs.values())
            .find_map(|node| node["images"].get(0).cloned())
        {
            break image;
        }
        if started.elapsed() > TIMEOUT {
            return Err(MediaError::Service("ComfyUI did not finish in time".into()));
        }
        tokio::time::sleep(COMFY_POLL).await;
    };
    let query = [
        ("filename", image["filename"].as_str().unwrap_or_default()),
        ("subfolder", image["subfolder"].as_str().unwrap_or_default()),
        ("type", image["type"].as_str().unwrap_or("output")),
    ];
    let bytes = http
        .get(format!("{url}/view"))
        .query(&query)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(service_error)?
        .bytes()
        .await
        .map_err(service_error)?;
    Ok(bytes.to_vec())
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
            image: Some(ImageBackend::OpenAi(service("sdxl"))),
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
            image: Some(ImageBackend::ComfyUi {
                url: base,
                workflow: workflow.into(),
            }),
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
        assert_eq!(std::fs::read(root.path().join(path)).unwrap(), PNG);
    }
}
