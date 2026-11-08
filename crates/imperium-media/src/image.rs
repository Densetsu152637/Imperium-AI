//! Image generators behind one trait, and the registry that builds them from `media.image`.

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{Value, json};

use crate::{COMFY_POLL, MediaError, Service, TIMEOUT, authorized, service_error};

pub type ImageFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, MediaError>> + Send + 'a>>;

/// Makes images for `media_image`; [`OpenAiImages`] and [`ComfyUi`] are built in.
pub trait ImageGenerator: Send + Sync + fmt::Debug {
    /// The model recorded in each image's provenance.
    fn model(&self) -> &str;
    /// The image's bytes (PNG) for `prompt` at `size` (width, height).
    fn generate<'a>(
        &'a self,
        http: &'a reqwest::Client,
        prompt: &'a str,
        size: (u32, u32),
    ) -> ImageFuture<'a>;
}

/// What generators may use from `[media.image]`.
pub struct ImageConfig<'a> {
    /// `comfyui_url`.
    pub comfyui_url: Option<&'a str>,
    /// `workflow`, a file path.
    pub workflow: Option<&'a Path>,
    /// The `endpoint` service with `model`; resolved (with its key) only by generators that use it.
    pub service: &'a dyn Fn() -> Option<Service>,
}

/// Builds a generator from `[media.image]`, or `None` when its settings are incomplete.
pub type ImageGeneratorBuilder =
    Box<dyn Fn(&ImageConfig<'_>) -> Option<Arc<dyn ImageGenerator>> + Send + Sync>;

/// Generator builders by `media.image.backend` name. The default has `openai` and `comfyui`;
/// any name not registered uses `openai`.
pub struct ImageGenerators {
    builders: Vec<(String, ImageGeneratorBuilder)>,
}

const FALLBACK: &str = "openai";

impl Default for ImageGenerators {
    fn default() -> Self {
        let mut generators = ImageGenerators {
            builders: Vec::new(),
        };
        generators.register(FALLBACK, |config| {
            (config.service)()
                .map(|service| Arc::new(OpenAiImages { service }) as Arc<dyn ImageGenerator>)
        });
        generators.register("comfyui", |config| {
            let workflow = std::fs::read_to_string(config.workflow?).ok()?;
            Some(Arc::new(ComfyUi {
                url: config
                    .comfyui_url
                    .unwrap_or("http://127.0.0.1:8188")
                    .trim_end_matches('/')
                    .to_string(),
                workflow,
            }))
        });
        generators
    }
}

impl ImageGenerators {
    /// Adds a generator under `name`, replacing any generator already called that.
    pub fn register(
        &mut self,
        name: &str,
        builder: impl Fn(&ImageConfig<'_>) -> Option<Arc<dyn ImageGenerator>> + Send + Sync + 'static,
    ) {
        self.builders.retain(|(existing, _)| existing != name);
        self.builders.push((name.to_string(), Box::new(builder)));
    }

    /// The generator called `name` (or `openai` when no generator has that name), or `None`
    /// when its settings are incomplete.
    pub fn build(&self, name: &str, config: &ImageConfig<'_>) -> Option<Arc<dyn ImageGenerator>> {
        let find = |name: &str| {
            self.builders
                .iter()
                .find(|(existing, _)| existing == name)
                .map(|(_, builder)| builder)
        };
        find(name).or_else(|| find(FALLBACK))?(config)
    }
}

/// An OpenAI-compatible images API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiImages {
    pub service: Service,
}

/// A ComfyUI server and an API-format workflow whose text contains `{{prompt}}` (and
/// optionally `{{seed}}`, `{{width}}`, `{{height}}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComfyUi {
    pub url: String,
    pub workflow: String,
}

impl ImageGenerator for OpenAiImages {
    fn model(&self) -> &str {
        &self.service.model
    }

    fn generate<'a>(
        &'a self,
        http: &'a reqwest::Client,
        prompt: &'a str,
        size: (u32, u32),
    ) -> ImageFuture<'a> {
        Box::pin(openai_image(http, &self.service, prompt, size))
    }
}

impl ImageGenerator for ComfyUi {
    fn model(&self) -> &str {
        "comfyui workflow"
    }

    fn generate<'a>(
        &'a self,
        http: &'a reqwest::Client,
        prompt: &'a str,
        size: (u32, u32),
    ) -> ImageFuture<'a> {
        Box::pin(comfyui_image(http, &self.url, &self.workflow, prompt, size))
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
