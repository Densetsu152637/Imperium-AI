//! Setting up models from the app without editing files: model folders (`models.add_folder`,
//! `models.remove_folder`), the default model (`models.use`), a test prompt (`models.test`),
//! and the llama.cpp runtime (`runtime.status`, `runtime.set_server`, `runtime.install`; the
//! automatic install and package-manager updates are in [`crate::llama_runtime`]).
//!
//! Each change is written to the user's `config.toml` through the settings table (as
//! `config.set` does) and applied to the running daemon at once: the live settings take the new
//! value, the configuration layers kept for later model-settings saves are read again (so such a
//! save does not bring the old value back), and the model folders are scanned again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use imperium_api::{LocalModels, LocalRequest};
use imperium_config::{SetError, Settings};
use imperium_local::hardware::{Hardware, probe};
use imperium_local::runtime::{
    GITHUB_API, build_number, install, llama_cpp_ref, pack_for, packs_for, pinned_build,
    remove_earlier_releases, resolve,
};
use imperium_local::runtime_check::{
    self, find_existing, find_unverified, has_cuda_backend, home_dir,
};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    LocalModelListResult, ModelAddEndpointParams, ModelAddEndpointResult, ModelFolderParams,
    ModelTestParams, ModelTestResult, ModelUseParams, ModelUseResult, RUNTIME_INSTALL_PROGRESS,
    RuntimeDevice, RuntimeInstallJob, RuntimeSetServerParams, RuntimeStatusResult,
};
use serde_json::{Value, json};

use crate::DaemonState;

/// The prompt `models.test` sends when given none.
const TEST_PROMPT: &str = "Say hello in one short sentence.";
/// Most tokens a test reply may use.
const TEST_MAX_TOKENS: u32 = 96;
/// How long a test may take, loading included.
const TEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Install progress is announced at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

fn invalid(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message.into())
}

fn internal(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INTERNAL_ERROR, message.into())
}

/// Writes `key` to the user's config file and applies the setting to the running daemon with
/// `apply`, which copies the new value from freshly loaded settings into the live ones.
fn save(
    state: &DaemonState,
    key: &str,
    value: &Value,
    apply: impl FnOnce(&mut Settings, &Settings),
) -> Result<(), ErrorObject> {
    imperium_config::set_user_setting(state.paths(), key, value, std::env::vars())
        .map_err(set_error)?;
    tracing::info!(%key, "the app changed a setting in the user config");
    apply_saved(state, apply)
}

fn set_error(error: SetError) -> ErrorObject {
    match error {
        SetError::Write { .. } => internal(error.to_string()),
        _ => invalid(error.to_string()),
    }
}

/// Reads the configuration again after the user file changed and applies part of it to the
/// running daemon with `apply`.
fn apply_saved(
    state: &DaemonState,
    apply: impl FnOnce(&mut Settings, &Settings),
) -> Result<(), ErrorObject> {
    let config = imperium_config::load(state.paths(), None, std::env::vars())
        .map_err(|error| internal(error.to_string()))?;
    let fresh = config
        .settings()
        .map_err(|error| internal(error.to_string()))?;
    let _saving = state
        .config_saves
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    state.remember_config(&config);
    let mut next = (*state.settings()).clone();
    apply(&mut next, &fresh);
    state.replace_settings(next);
    Ok(())
}

/// Folders compare without trailing separators, and without regard to case on Windows.
fn same_folder(a: &Path, b: &Path) -> bool {
    let key = |path: &Path| {
        let text = path.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/').to_string();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    key(a) == key(b)
}

fn folder_list(folders: &[PathBuf]) -> Value {
    Value::Array(
        folders
            .iter()
            .map(|folder| Value::String(folder.display().to_string()))
            .collect(),
    )
}

/// Scans the model folders again (starting the local runtime when this is the first folder)
/// and returns the listing.
async fn rescan(state: Arc<DaemonState>) -> Result<LocalModelListResult, ErrorObject> {
    let scanning = Arc::clone(&state);
    tokio::task::spawn_blocking(move || scanning.rescan_local_models())
        .await
        .map_err(|error| internal(format!("the rescan failed: {error}")))?;
    Ok(state.local_models())
}

/// `models.add_folder`.
pub async fn add_folder(
    state: Arc<DaemonState>,
    params: ModelFolderParams,
) -> Result<LocalModelListResult, ErrorObject> {
    let folder = PathBuf::from(params.path.trim());
    if !folder.is_absolute() {
        return Err(invalid(format!(
            "{} is not an absolute path; choose a folder with the folder picker",
            folder.display()
        )));
    }
    if !folder.is_dir() {
        return Err(invalid(format!(
            "{} is not a folder on this machine",
            folder.display()
        )));
    }
    let mut folders = state.settings().models.folders.clone();
    if !folders.iter().any(|known| same_folder(known, &folder)) {
        folders.push(folder);
        save(
            &state,
            "models.folders",
            &folder_list(&folders),
            |next, fresh| {
                next.models.folders.clone_from(&fresh.models.folders);
            },
        )?;
    }
    rescan(state).await
}

/// `models.remove_folder`.
pub async fn remove_folder(
    state: Arc<DaemonState>,
    params: ModelFolderParams,
) -> Result<LocalModelListResult, ErrorObject> {
    let folder = PathBuf::from(params.path.trim());
    let current = state.settings().models.folders.clone();
    let kept: Vec<PathBuf> = current
        .iter()
        .filter(|known| !same_folder(known, &folder))
        .cloned()
        .collect();
    if kept.len() == current.len() {
        return Err(invalid(format!(
            "{} is not one of your model folders (models.folders)",
            folder.display()
        )));
    }
    save(
        &state,
        "models.folders",
        &folder_list(&kept),
        |next, fresh| {
            next.models.folders.clone_from(&fresh.models.folders);
        },
    )?;
    rescan(state).await
}

/// The spec a model is used by: a spec as given, a model-table id's spec, or a local model's
/// `<runtime>:<id>`.
fn spec_of(state: &DaemonState, model: &str) -> Result<String, ErrorObject> {
    let model = model.trim();
    if model.is_empty() {
        return Err(invalid("name a model"));
    }
    let settings = state.settings();
    if let Some(row) = settings.models.catalog.iter().find(|row| row.id == model) {
        return Ok(row.spec.clone());
    }
    let local = state.local_models();
    if let Some(row) = local.models.iter().find(|row| row.id == model) {
        return Ok(format!("{}:{}", row.runtime, row.id));
    }
    let Some((endpoint, name)) = model.split_once(':') else {
        return Err(invalid(format!(
            "no local model or model-table row is called {model:?}; see models.local"
        )));
    };
    match endpoint {
        imperium_config::LLAMACPP_ENDPOINT | "vllm" => {
            if !local.models.iter().any(|row| row.id == name) {
                return Err(invalid(format!(
                    "no local model {name:?} in the model folders"
                )));
            }
        }
        _ if !settings.endpoints.contains_key(endpoint) => {
            return Err(invalid(format!(
                "endpoint {endpoint:?} is not configured under [endpoints]"
            )));
        }
        _ => {}
    }
    Ok(model.to_string())
}

/// `models.use`.
pub fn use_model(
    state: &DaemonState,
    params: ModelUseParams,
) -> Result<ModelUseResult, ErrorObject> {
    let spec = spec_of(state, &params.model)?;
    save(
        state,
        "models.default",
        &Value::String(spec.clone()),
        |next, fresh| {
            next.models.default.clone_from(&fresh.models.default);
        },
    )?;
    Ok(ModelUseResult { default: spec })
}

/// `models.add_endpoint`: saves `[endpoints.<id>]` (its API key to the keychain), and makes
/// `<id>:<model>` the default when asked.
pub fn add_endpoint(
    state: &DaemonState,
    params: ModelAddEndpointParams,
) -> Result<ModelAddEndpointResult, ErrorObject> {
    let id = params.id.trim().to_ascii_lowercase();
    if !imperium_config::valid_endpoint_id(&id) {
        return Err(invalid(
            "an endpoint name is 1-40 lowercase letters, digits, '_' or '-' (not llamacpp or vllm)",
        ));
    }
    let secret = match params.api_key.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(key) => {
            let name: imperium_config::SecretName = format!("{id}-api-key")
                .parse()
                .map_err(|error: imperium_config::SecretError| invalid(error.to_string()))?;
            state
                .secrets()
                .store(&name, key)
                .map_err(|error| internal(format!("could not store the API key: {error}")))?;
            Some(name)
        }
    };
    imperium_config::save_endpoint(
        state.paths(),
        &id,
        &params.base_url,
        secret.as_ref(),
        std::env::vars(),
    )
    .map_err(set_error)?;
    tracing::info!(endpoint = %id, "the app added an endpoint to the user config");
    apply_saved(state, |next, fresh| {
        next.endpoints.clone_from(&fresh.endpoints);
    })?;
    let model = params.model.as_deref().map(str::trim).unwrap_or_default();
    let default = if params.make_default && !model.is_empty() {
        Some(
            use_model(
                state,
                ModelUseParams {
                    model: format!("{id}:{model}"),
                },
            )?
            .default,
        )
    } else {
        None
    };
    Ok(ModelAddEndpointResult { id, default })
}

/// What a chat completion said: the reply, completion tokens, and the server's own speed.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Answer {
    pub reply: String,
    pub completion_tokens: u32,
    pub tokens_per_s: Option<f64>,
}

/// Reads an OpenAI-compatible chat completion; a reasoning model's thinking stands in for an
/// empty reply. llama.cpp adds `timings.predicted_per_second`.
pub(crate) fn read_answer(body: &Value) -> Answer {
    let message = &body["choices"][0]["message"];
    let content = message["content"].as_str().unwrap_or_default().trim();
    let reply = if content.is_empty() {
        message["reasoning_content"]
            .as_str()
            .map(|thinking| format!("(thinking) {}", thinking.trim()))
            .unwrap_or_default()
    } else {
        content.to_string()
    };
    Answer {
        reply,
        completion_tokens: body["usage"]["completion_tokens"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or_default(),
        tokens_per_s: body["timings"]["predicted_per_second"]
            .as_f64()
            .filter(|rate| rate.is_finite() && *rate > 0.0),
    }
}

/// `models.test`.
pub async fn test_model(
    state: Arc<DaemonState>,
    params: ModelTestParams,
) -> Result<ModelTestResult, ErrorObject> {
    let spec = spec_of(&state, &params.model)?;
    let resolved = state
        .resolve_model(Some(&spec))
        .map_err(|error| invalid(error.to_string()))?;
    let prompt = params
        .prompt
        .filter(|prompt| !prompt.trim().is_empty())
        .unwrap_or_else(|| TEST_PROMPT.to_string());
    let mut result = ModelTestResult {
        model: spec.clone(),
        ok: false,
        reply: String::new(),
        message: None,
        load_seconds: 0.0,
        seconds: 0.0,
        completion_tokens: 0,
        tokens_per_s: None,
    };
    let started = Instant::now();
    let mut body = json!({
        "model": resolved.model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": TEST_MAX_TOKENS,
        "stream": false,
    });
    let mut lease = None;
    let (base_url, api_key) = if resolved.local {
        let Some(local) = state.local_runtime() else {
            result.message = Some("no local models are set up; add a model folder first".into());
            return Ok(result);
        };
        let supervisor = Arc::clone(&local.supervisor);
        let request = LocalRequest {
            prompt_tokens: Some(64),
            ..LocalRequest::default()
        };
        let acquired = tokio::time::timeout(
            TEST_TIMEOUT,
            supervisor.acquire_with(&resolved.model, "models.test", request),
        )
        .await;
        let held = match acquired {
            Err(_) => {
                result.message = Some(format!(
                    "the model did not load within {} s",
                    TEST_TIMEOUT.as_secs()
                ));
                return Ok(result);
            }
            Ok(Err(error)) => {
                result.message = Some(format!("the model could not be loaded: {error}"));
                return Ok(result);
            }
            Ok(Ok(held)) => held,
        };
        if let Some(cap) = held.max_tokens {
            body["max_tokens"] = json!(cap.clamp(1, TEST_MAX_TOKENS));
        }
        if resolved.kind == "llamacpp" {
            if let Some(slot) = held.slot {
                body["id_slot"] = json!(slot);
            }
            // A thinking model answers at once instead of spending the test's tokens on
            // reasoning (templates without the switch ignore it).
            body["chat_template_kwargs"] = json!({"enable_thinking": false});
        }
        let base = held.base_url.clone();
        lease = Some((supervisor, held));
        (base, None)
    } else {
        (
            resolved.endpoint.base_url.clone(),
            resolved.endpoint.api_key.clone(),
        )
    };
    result.load_seconds = started.elapsed().as_secs_f64();
    let sent = Instant::now();
    let http = reqwest::Client::new();
    let mut request = http
        .post(format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .json(&body)
        .timeout(TEST_TIMEOUT);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await;
    result.seconds = sent.elapsed().as_secs_f64();
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            result.message = Some(format!("the request failed: {error}"));
            return Ok(result);
        }
    };
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail: String = text.chars().take(300).collect();
        result.message = Some(format!("the server answered {status}: {detail}"));
        return Ok(result);
    }
    let answer = match serde_json::from_str::<Value>(&text) {
        Ok(body) => read_answer(&body),
        Err(error) => {
            result.message = Some(format!("the reply was not JSON: {error}"));
            return Ok(result);
        }
    };
    result.tokens_per_s = answer.tokens_per_s.or_else(|| {
        (answer.completion_tokens > 0 && result.seconds > 0.0)
            .then(|| f64::from(answer.completion_tokens) / result.seconds)
    });
    if let Some((supervisor, held)) = lease {
        supervisor.record_throughput(
            &resolved.model,
            u64::from(answer.completion_tokens),
            sent.elapsed(),
        );
        drop(held);
    }
    result.ok = !answer.reply.is_empty();
    if !result.ok {
        result.message = Some("the model answered with an empty reply".into());
    }
    result.reply = answer.reply;
    result.completion_tokens = answer.completion_tokens;
    Ok(result)
}

/// The hardware the local runtime planned against, or a fresh probe when it is not running.
pub(crate) async fn hardware(state: &DaemonState) -> Hardware {
    if let Some(local) = state.local_runtime() {
        return local.hardware().clone();
    }
    let settings = state.settings().hardware.clone();
    tokio::task::spawn_blocking(move || probe(&settings))
        .await
        .unwrap_or_else(|_| probe(&imperium_config::HardwareSettings::default()))
}

/// `runtime.status`.
pub async fn runtime_status(state: Arc<DaemonState>) -> RuntimeStatusResult {
    let settings = state.settings();
    let llamacpp = &settings.runtime.llamacpp;
    let hardware = hardware(&state).await;
    let suggested = pack_for(&llamacpp.backend, &hardware);
    let home = home_dir();
    let resolved = resolve(llamacpp, state.data_dir(), &hardware, home.as_deref()).await;
    // A runtime installed or found since the local runtime chose its server is used from now
    // on, such as one `imperium runtime install` put in place.
    if let Some(local) = state.local_runtime()
        && local.server() != (resolved.label(), resolved.server.clone())
    {
        local.set_server(resolved.label(), resolved.server.clone());
    }
    let server = resolved.server.clone();
    // Opening the panel is a first need of a runtime: one is installed now when none is usable.
    crate::llama_runtime::install_if_needed(&state, &resolved, &hardware).await;
    let check = match resolved.check.clone() {
        Some(check) => check,
        None => {
            let outcome = runtime_check::probe(&server).await;
            runtime_check::explain(
                &server,
                &outcome,
                &hardware,
                has_cuda_backend(&server),
                &suggested,
            )
        }
    };
    let flags_ok = if resolved.source == "custom" && check.runs {
        runtime_check::missing_flags(&server)
            .await
            .is_none_or(|missing| missing.is_empty())
    } else {
        true
    };
    let (kind, _, update) =
        crate::llama_runtime::offer(&state, &resolved, check.version.as_deref(), flags_ok);
    let device_ids: Vec<(String, bool)> = check
        .devices
        .iter()
        .map(|device| (device.id.clone(), device.is_gpu()))
        .collect();
    let backend = crate::llama_runtime::backend_label(&resolved, &device_ids);
    let build = build_number(check.version.as_deref());
    // Unverified downloads are listed last and only run once the user picks one.
    let candidates = find_existing(home.as_deref())
        .into_iter()
        .chain(find_unverified(home.as_deref()))
        .filter(|candidate| candidate != &server)
        .map(|candidate| candidate.display().to_string())
        .collect();
    RuntimeStatusResult {
        source: resolved.source.into(),
        pack: resolved.pack,
        suggested_pack: suggested,
        server_path: server.display().to_string(),
        installed: server.is_file(),
        runs: check.runs,
        version: check.version,
        devices: check
            .devices
            .iter()
            .map(|device| RuntimeDevice {
                id: device.id.clone(),
                description: device.description.clone(),
                gpu: device.is_gpu(),
            })
            .collect(),
        gpu_visible: check.gpu_visible,
        problem: check.problem,
        summary: check.summary,
        llama_cpp_ref: llama_cpp_ref().into(),
        candidates,
        install: state
            .runtime_install
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
        backend,
        auto_install: llamacpp.auto_install,
        build,
        pinned_build: pinned_build(),
        install_kind: kind.name().into(),
        update,
        update_job: state
            .runtime_update
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    }
}

/// Applies a setting `config.set` just saved, for the settings this module applies live
/// (`models.default`, `models.folders`, `runtime.llamacpp.custom_path`, `policy.personality`); others are left for
/// a restart. New folders are scanned in the background.
pub(crate) fn apply_setting(state: &DaemonState, key: &str) {
    let applied = match key {
        "models.default" => apply_saved(state, |next, fresh| {
            next.models.default.clone_from(&fresh.models.default);
        }),
        "models.folders" => apply_saved(state, |next, fresh| {
            next.models.folders.clone_from(&fresh.models.folders);
        }),
        // Read when a session starts, so new sessions use it at once.
        "policy.personality" => apply_saved(state, |next, fresh| {
            next.policy.clone_from(&fresh.policy);
        }),
        "runtime.llamacpp.custom_path" => apply_saved(state, |next, fresh| {
            next.runtime
                .llamacpp
                .custom_path
                .clone_from(&fresh.runtime.llamacpp.custom_path);
        })
        .map(|()| apply_server(state)),
        _ => return,
    };
    if let Err(error) = applied {
        tracing::warn!(%key, error = %error.message, "the saved setting could not be applied");
        return;
    }
    if key == "models.folders"
        && let Some(state) = state.this()
        && tokio::runtime::Handle::try_current().is_ok()
    {
        tokio::task::spawn_blocking(move || state.rescan_local_models());
    }
}

/// Points the running local runtime at the `llama-server` the live settings name.
pub(crate) fn apply_server(state: &DaemonState) {
    if let Some(local) = state.local_runtime() {
        local.use_server(state.paths(), &state.settings());
    }
}

/// `runtime.set_server`.
pub async fn set_server(
    state: Arc<DaemonState>,
    params: RuntimeSetServerParams,
) -> Result<RuntimeStatusResult, ErrorObject> {
    let value = match params.path.as_deref().map(str::trim) {
        None | Some("") => Value::Null,
        Some(path) => {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !path.is_file() {
                return Err(invalid(format!(
                    "{} is not a file on this machine; choose the llama-server executable",
                    path.display()
                )));
            }
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !name.contains("llama-server") && !name.contains("server") {
                return Err(invalid(format!(
                    "{} does not look like llama-server; choose the llama-server executable \
                     from a llama.cpp build",
                    path.display()
                )));
            }
            Value::String(path.display().to_string())
        }
    };
    save(
        &state,
        "runtime.llamacpp.custom_path",
        &value,
        |next, fresh| {
            next.runtime
                .llamacpp
                .custom_path
                .clone_from(&fresh.runtime.llamacpp.custom_path);
        },
    )?;
    apply_server(&state);
    Ok(runtime_status(state).await)
}

fn update_install(state: &DaemonState, change: impl FnOnce(&mut RuntimeInstallJob)) {
    let job = {
        let mut slot = state
            .runtime_install
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(job) = slot.as_mut() else {
            return;
        };
        change(job);
        job.clone()
    };
    state.announce(RUNTIME_INSTALL_PROGRESS, job);
}

/// `runtime.install`: starts installing llama.cpp's build for this machine (or for
/// `runtime.llamacpp.backend`) from its GitHub release, falling back to the next build when one
/// does not run here, unless an installation is running.
pub async fn install_runtime(state: Arc<DaemonState>) -> Result<RuntimeInstallJob, ErrorObject> {
    let hardware = hardware(&state).await;
    let packs = packs_for(&state.settings().runtime.llamacpp.backend, &hardware);
    let pack = packs[0].clone();
    let job = {
        let mut slot = state
            .runtime_install
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = slot.as_ref().filter(|job| job.state == "running") {
            return Ok(running.clone());
        }
        let job = RuntimeInstallJob {
            pack: pack.clone(),
            state: "running".into(),
            done_bytes: 0,
            total_bytes: 0,
            message: None,
        };
        *slot = Some(job.clone());
        job
    };
    state.announce(RUNTIME_INSTALL_PROGRESS, &job);
    let installing = Arc::clone(&state);
    tokio::spawn(async move {
        let state = installing;
        let last = std::sync::Mutex::new(
            Instant::now()
                .checked_sub(PROGRESS_INTERVAL)
                .unwrap_or_else(Instant::now),
        );
        let progress = |installing: &str, done: u64, total: u64| {
            let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
            if last.elapsed() < PROGRESS_INTERVAL && done != 0 {
                return;
            }
            *last = Instant::now();
            update_install(&state, |job| {
                installing.clone_into(&mut job.pack);
                job.done_bytes = done;
                job.total_bytes = total;
            });
        };
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let result = install(
            &http,
            GITHUB_API,
            state.data_dir(),
            &packs,
            &hardware,
            &progress,
        )
        .await;
        match result {
            Ok(installed) => {
                tracing::info!(
                    pack = %installed.pack,
                    server = %installed.server.display(),
                    skipped = ?installed.skipped,
                    "llama.cpp runtime installed"
                );
                apply_server(&state);
                // One earlier release stays until the new build has loaded a model.
                let data_dir = state.data_dir().to_path_buf();
                let _ = tokio::task::spawn_blocking(move || {
                    remove_earlier_releases(&data_dir, false);
                })
                .await;
                update_install(&state, |job| {
                    job.state = "done".into();
                    job.pack.clone_from(&installed.pack);
                    job.done_bytes = job.total_bytes.max(job.done_bytes);
                    job.message = Some(installed.server.display().to_string());
                });
            }
            Err(error) => {
                tracing::warn!(%pack, %error, "runtime pack installation failed");
                update_install(&state, |job| {
                    job.state = "failed".into();
                    job.message = Some(error.to_string());
                });
            }
        }
    });
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_with_the_servers_speed_and_thinking_as_a_fallback() {
        let llama = json!({
            "choices": [{"message": {"role": "assistant", "content": " Hello there! "}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 4},
            "timings": {"predicted_per_second": 87.5}
        });
        assert_eq!(
            read_answer(&llama),
            Answer {
                reply: "Hello there!".into(),
                completion_tokens: 4,
                tokens_per_s: Some(87.5)
            }
        );
        let thinking = json!({
            "choices": [{"message": {"content": "", "reasoning_content": "The user wants"}}],
            "usage": {"completion_tokens": 96}
        });
        let answer = read_answer(&thinking);
        assert_eq!(answer.reply, "(thinking) The user wants");
        assert_eq!(answer.tokens_per_s, None);
        assert_eq!(read_answer(&json!({})), Answer::default());
    }

    #[test]
    fn folders_compare_without_trailing_separators() {
        assert!(same_folder(Path::new("/m/models/"), Path::new("/m/models")));
        assert!(!same_folder(Path::new("/m/models"), Path::new("/m/other")));
        if cfg!(windows) {
            assert!(same_folder(
                Path::new("D:\\AI-Models"),
                Path::new("d:/ai-models")
            ));
        }
    }
}
