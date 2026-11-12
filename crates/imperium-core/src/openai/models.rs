//! Which models the API serves. Every local model the runtime found is served; a hosted model
//! (anything reached over an endpoint, which spends the user's keys and money) only when its
//! spec is listed in `api.openai_compatible.expose` and is a row of the model table.

use imperium_config::{LLAMACPP_ENDPOINT, Settings, VLLM_ENDPOINT};

use crate::DaemonState;

/// A model the API serves, or could serve once its owner opts in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    /// The model's Imperium spec, `<endpoint>:<model>`: the id other tools use.
    pub id: String,
    /// Its alias, else its name.
    pub name: String,
    /// The endpoint, which names who serves it (`llamacpp`, `vllm`, or a hosted endpoint).
    pub endpoint: String,
    pub hosted: bool,
    pub exposed: bool,
}

/// Every model the API serves or could serve with an opt-in: local models first, then the
/// enabled rows of the model table that are hosted.
pub(crate) fn candidates(state: &DaemonState) -> Vec<Candidate> {
    let settings = state.settings();
    let config = &settings.api.openai_compatible;
    let mut found: Vec<Candidate> = Vec::new();
    for row in state.local_models().models {
        if row.problem.is_some() {
            continue;
        }
        let id = format!("{}:{}", row.runtime, row.id);
        if settings.models.hidden.contains(&id) {
            continue;
        }
        let name = catalog_alias(&settings, &id)
            .or(row.alias)
            .unwrap_or_else(|| row.id.clone());
        found.push(Candidate {
            id,
            name,
            endpoint: row.runtime,
            hosted: false,
            exposed: true,
        });
    }
    for entry in settings.models.catalog.iter().filter(|entry| entry.enabled) {
        let (endpoint, _) = entry.spec.split_once(':').unwrap_or((&entry.spec, ""));
        if is_local_endpoint(endpoint) || found.iter().any(|known| known.id == entry.spec) {
            continue;
        }
        found.push(Candidate {
            id: entry.spec.clone(),
            name: entry.alias.clone().unwrap_or_else(|| entry.id.clone()),
            endpoint: endpoint.to_string(),
            hosted: true,
            exposed: config.expose.contains(&entry.spec)
                && settings.endpoints.contains_key(endpoint),
        });
    }
    found
}

/// The model the API serves under `id`, or why it does not.
pub(crate) fn find(state: &DaemonState, id: &str) -> Result<Candidate, String> {
    let all = candidates(state);
    match all.into_iter().find(|candidate| candidate.id == id) {
        Some(candidate) if candidate.exposed => Ok(candidate),
        Some(_) => Err(
            "It is a hosted model, and hosted models are served only when you turn them on \
             for the API in Settings → API."
                .into(),
        ),
        None => Err("GET /v1/models lists the models it serves.".into()),
    }
}

pub(crate) fn is_local_endpoint(endpoint: &str) -> bool {
    endpoint == LLAMACPP_ENDPOINT || endpoint == VLLM_ENDPOINT
}

/// Whether the model's server takes `tools`. llama.cpp is always started with `--jinja`, which
/// renders them through the model's chat template; vLLM needs `--enable-auto-tool-choice`
/// among its extra arguments; a hosted endpoint is assumed to speak the OpenAI API.
pub(crate) fn supports_tools(settings: &Settings, spec: &str) -> bool {
    let endpoint = spec.split_once(':').map_or(spec, |(endpoint, _)| endpoint);
    match endpoint {
        VLLM_ENDPOINT => settings
            .runtime
            .vllm
            .extra_args
            .iter()
            .any(|arg| arg == "--enable-auto-tool-choice"),
        _ => true,
    }
}

fn catalog_alias(settings: &Settings, spec: &str) -> Option<String> {
    settings
        .models
        .catalog
        .iter()
        .find(|entry| entry.spec == spec)
        .and_then(|entry| entry.alias.clone())
}
