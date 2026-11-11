//! `models.config` and `models.set_config`: per-model settings the desktop app saves to
//! `<config dir>/models.toml` (the app layer, see docs/10-configuration.md#layers) and applies
//! to the running daemon.

use std::path::Path;
use std::sync::PoisonError;

use imperium_config::{
    CACHE_TYPES, Config, ContextLimit, LocalModelSettings, ModelLaunch, NO_CONTEXT_LIMIT, Settings,
    TableOverride, check_local_model, load_layers, replace_app_layer,
};
use imperium_protocol::methods::{
    LocalLaunchEffective, LocalLaunchSettings, LocalModelConfig, ModelConfigResult,
    ModelSetConfigParams, TableModelConfig, TableModelEffective, TableModelSettings,
};
use toml::{Table, Value};

use crate::state::DaemonState;

const HEADER: &str = "# Written by the Imperium desktop app (Models dialog). Settings saved there\n\
                      # win over config.toml, key by key. The app rewrites this file.\n\n";

/// Every local model's settings and every model-table row's, as saved and as in effect.
pub fn config(state: &DaemonState) -> ModelConfigResult {
    let file = state.paths().app_models_file();
    let saved = read_app_file(&file).unwrap_or_else(|error| {
        tracing::warn!("{error}");
        Table::new()
    });
    result(state, &state.settings(), &saved)
}

/// Saves the updates to the app layer and applies them; with `dry_run`, only checks them and
/// returns what the result would be.
pub fn set_config(
    state: &DaemonState,
    params: ModelSetConfigParams,
) -> Result<ModelConfigResult, String> {
    let _saving = state
        .config_saves
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let paths = state.paths();
    let file = paths.app_models_file();
    // A file that is not valid TOML is not overwritten: what it held would be lost.
    let mut saved = read_app_file(&file)?;
    let current = state.settings();
    for update in &params.local {
        let model = state
            .local_runtime()
            .and_then(|local| {
                local
                    .supervisor
                    .models()
                    .into_iter()
                    .find(|m| m.id == update.id)
            })
            .ok_or_else(|| format!("local model {}: no such model in models.folders", update.id))?;
        let settings = &update.settings;
        if model.format == imperium_local::scan::Format::Safetensors
            && (settings.cache_type_k.is_some()
                || settings.cache_type_v.is_some()
                || settings.flash_attn.is_some()
                || settings.max_gpu_layers.is_some())
        {
            return Err(format!(
                "local model {}: cache types, flash attention and GPU layers do not apply to a vLLM model",
                update.id
            ));
        }
        check_local_model(&to_local(settings))
            .map_err(|reason| format!("local model {}: {reason}", update.id))?;
        set_entry(
            &mut saved,
            &["runtime", "llamacpp", "models"],
            &update.id,
            local_table(settings),
        );
    }
    for update in &params.table {
        if !current.models.catalog.iter().any(|row| row.id == update.id) {
            return Err(format!(
                "model table row {}: no such row in models.catalog",
                update.id
            ));
        }
        let settings = &update.settings;
        if settings.input_usd_per_mtok.is_some() != settings.output_usd_per_mtok.is_some() {
            return Err(format!(
                "model table row {}: set both prices or neither",
                update.id
            ));
        }
        imperium_config::check_alias(settings.alias.as_deref())
            .map_err(|reason| format!("model table row {}: {reason}", update.id))?;
        if settings.context_window == Some(0) {
            return Err(format!(
                "model table row {}: context_window needs a value of at least 1",
                update.id
            ));
        }
        set_entry(
            &mut saved,
            &["models", "overrides"],
            &update.id,
            table_table(settings),
        );
    }
    let candidate = candidate(state, &file, &saved)?;
    if params.dry_run {
        return Ok(result(state, &candidate, &saved));
    }
    write_app_file(&file, &saved)?;
    let mut next = (*current).clone();
    next.models = candidate.models.clone();
    next.runtime.llamacpp.models = candidate.runtime.llamacpp.models.clone();
    for (id, endpoint) in &mut next.endpoints {
        if let Some(updated) = candidate.endpoints.get(id) {
            endpoint.pricing.clone_from(&updated.pricing);
        }
    }
    state.replace_settings(next);
    let applied = state.settings();
    if let Some(local) = state.local_runtime() {
        local.apply_model_settings(&applied);
    }
    Ok(result(state, &applied, &saved))
}

/// The settings the configuration layers would give with `saved` as the app layer, checked
/// strictly: any invalid value refuses the save. The other layers are the ones the live
/// settings were built from, so edits made to config.toml since the daemon started are not
/// picked up by a save (they apply on restart); without them (settings given alone) they are
/// read again.
fn candidate(state: &DaemonState, file: &Path, saved: &Table) -> Result<Settings, String> {
    let layers = match state.config_layers() {
        Some(layers) => layers,
        None => {
            load_layers(state.paths(), None, std::env::vars()).map_err(|error| error.to_string())?
        }
    };
    let layers = replace_app_layer(layers, file.to_path_buf(), saved.clone());
    Config::from_layers(layers)
        .settings()
        .map_err(|error| error.to_string())
}

/// Reads the app file; one that is missing counts as empty, one that cannot be read or is not
/// valid TOML is an error naming the file.
fn read_app_file(file: &Path) -> Result<Table, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Table::new()),
        Err(error) => return Err(format!("could not read {}: {error}", file.display())),
    };
    text.parse().map_err(|error| {
        format!(
            "{} is not valid TOML, so it was not overwritten; fix or remove it first: {error}",
            file.display()
        )
    })
}

/// Writes the app file through a temporary file, so a crash never leaves half of it.
fn write_app_file(file: &Path, saved: &Table) -> Result<(), String> {
    let body = toml::to_string(saved).map_err(|error| error.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    }
    let temp = file.with_extension("toml.tmp");
    std::fs::write(&temp, format!("{HEADER}{body}"))
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    std::fs::rename(&temp, file)
        .map_err(|error| format!("could not replace {}: {error}", file.display()))
}

/// Sets (or, when `entry` is empty, removes) `id` under the table at `path`.
fn set_entry(saved: &mut Table, path: &[&str], id: &str, entry: Table) {
    let mut table = &mut *saved;
    for key in path {
        let Some(next) = table
            .entry((*key).to_string())
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
        else {
            return;
        };
        table = next;
    }
    if entry.is_empty() {
        table.remove(id);
    } else {
        table.insert(id.to_string(), Value::Table(entry));
    }
}

fn integer(value: u32) -> Value {
    Value::Integer(i64::from(value))
}

fn strings(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}

fn local_table(settings: &LocalLaunchSettings) -> Table {
    let mut table = Table::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            table.insert(key.to_string(), value);
        }
    };
    put("alias", settings.alias.clone().map(Value::String));
    if settings.context_auto {
        put("context", Some(Value::String("auto".into())));
    } else {
        put("context", settings.context.map(integer));
    }
    put("pool_tokens", settings.pool_tokens.map(integer));
    put("slots", settings.slots.map(integer));
    put(
        "cache_type_k",
        settings.cache_type_k.clone().map(Value::String),
    );
    put(
        "cache_type_v",
        settings.cache_type_v.clone().map(Value::String),
    );
    put("flash_attn", settings.flash_attn.clone().map(Value::String));
    put("max_gpu_layers", settings.max_gpu_layers.map(integer));
    put(
        "max_wait_s",
        settings
            .max_wait_s
            .map(|value| Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))),
    );
    put("max_output_tokens", settings.max_output_tokens.map(integer));
    put("extra_args", settings.extra_args.as_deref().map(strings));
    table
}

fn table_table(settings: &TableModelSettings) -> Table {
    let mut table = Table::new();
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            table.insert(key.to_string(), value);
        }
    };
    put("alias", settings.alias.clone().map(Value::String));
    put("enabled", settings.enabled.map(Value::Boolean));
    put("tier", settings.tier.clone().map(Value::String));
    put("tags", settings.tags.as_deref().map(strings));
    put("context_window", settings.context_window.map(integer));
    put(
        "input_usd_per_mtok",
        settings.input_usd_per_mtok.map(Value::Float),
    );
    put(
        "output_usd_per_mtok",
        settings.output_usd_per_mtok.map(Value::Float),
    );
    table
}

fn to_local(settings: &LocalLaunchSettings) -> LocalModelSettings {
    LocalModelSettings {
        alias: settings.alias.clone(),
        context: if settings.context_auto {
            Some(ContextLimit::Auto)
        } else {
            settings.context.map(ContextLimit::Tokens)
        },
        pool_tokens: settings.pool_tokens,
        slots: settings.slots,
        cache_type_k: settings.cache_type_k.clone(),
        cache_type_v: settings.cache_type_v.clone(),
        flash_attn: settings.flash_attn.clone(),
        max_gpu_layers: settings.max_gpu_layers,
        max_wait_s: settings.max_wait_s,
        max_output_tokens: settings.max_output_tokens,
        extra_args: settings.extra_args.clone(),
    }
}

fn from_local(settings: LocalModelSettings) -> LocalLaunchSettings {
    LocalLaunchSettings {
        alias: settings.alias,
        context: settings.context.and_then(ContextLimit::tokens),
        context_auto: settings.context == Some(ContextLimit::Auto),
        pool_tokens: settings.pool_tokens,
        slots: settings.slots,
        cache_type_k: settings.cache_type_k,
        cache_type_v: settings.cache_type_v,
        flash_attn: settings.flash_attn,
        max_gpu_layers: settings.max_gpu_layers,
        max_wait_s: settings.max_wait_s,
        max_output_tokens: settings.max_output_tokens,
        extra_args: settings.extra_args,
    }
}

fn effective(launch: &ModelLaunch) -> LocalLaunchEffective {
    LocalLaunchEffective {
        // Unsized `auto` (the defaults) has no number of its own.
        context: if launch.context == NO_CONTEXT_LIMIT {
            0
        } else {
            launch.context
        },
        context_auto: launch.context_auto,
        pool_tokens: launch.pool_tokens,
        pool_fixed: launch.pool_fixed,
        pool_clamped: launch.pool_clamped,
        slots: launch.slots,
        cache_type_k: launch.cache_type_k.clone(),
        cache_type_v: launch.cache_type_v.clone(),
        flash_attn: launch.flash_attn.clone(),
        max_gpu_layers: launch.max_gpu_layers,
        max_wait_s: launch.max_wait_s,
        max_output_tokens: launch.max_output_tokens,
        extra_args: launch.extra_args.clone(),
    }
}

/// The saved entry for `id` under `path`, when it reads as a `T`.
fn saved_entry<T: serde::de::DeserializeOwned>(
    saved: &Table,
    path: &[&str],
    id: &str,
) -> Option<T> {
    let mut table = saved;
    for key in path {
        table = table.get(*key)?.as_table()?;
    }
    table.get(id)?.clone().try_into().ok()
}

/// The result for `settings` (live or candidate) with `saved` as the app file's content.
fn result(state: &DaemonState, settings: &Settings, saved: &Table) -> ModelConfigResult {
    let llama = &settings.runtime.llamacpp;
    let local = state.local_runtime().map_or_else(Vec::new, |runtime| {
        let supervisor = &runtime.supervisor;
        let mut models = supervisor.models();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        let loaded = supervisor.loaded();
        models
            .iter()
            .map(|model| {
                // Sized for the model like the supervisor sizes it: the context ceiling, and
                // the pool it starts with (or has grown to).
                let launch = supervisor.sized(model, &settings.launch_of(&model.id));
                let (footprint, placement) = supervisor.plan_with(model, &launch);
                let cpu_lane = settings.cpu_row(&model.id).is_some();
                let vllm = model.format == imperium_local::scan::Format::Safetensors;
                LocalModelConfig {
                    id: model.id.clone(),
                    runtime: if vllm { "vllm" } else { "llamacpp" }.into(),
                    saved: from_local(
                        saved_entry(saved, &["runtime", "llamacpp", "models"], &model.id)
                            .unwrap_or_default(),
                    ),
                    effective: effective(&launch),
                    required_bytes: footprint.required_bytes(),
                    kv_pool_bytes: footprint.kv_bytes,
                    placement: runtime.placement_label(placement.as_ref(), cpu_lane).into(),
                    loaded: loaded.iter().any(|status| status.model_id == model.id),
                    reload_pending: supervisor
                        .loaded_launch(&model.id)
                        .is_some_and(|started| started.restarts_for(&launch)),
                    cpu_lane,
                    context_length: model.info.context_length,
                }
            })
            .collect()
    });
    let table = settings
        .models
        .catalog
        .iter()
        .map(|entry| {
            let (endpoint_id, model) = entry
                .spec
                .split_once(':')
                .unwrap_or((entry.spec.as_str(), ""));
            let endpoint = settings.endpoints.get(endpoint_id);
            let pricing = endpoint.and_then(|endpoint| endpoint.pricing.get(model));
            let own: TableOverride =
                saved_entry(saved, &["models", "overrides"], &entry.id).unwrap_or_default();
            TableModelConfig {
                id: entry.id.clone(),
                spec: entry.spec.clone(),
                location: entry.location.as_str().into(),
                endpoint_configured: endpoint.is_some(),
                saved: TableModelSettings {
                    alias: own.alias,
                    enabled: own.enabled,
                    tier: own.tier.map(|tier| tier.as_str().into()),
                    tags: own.tags,
                    context_window: own.context_window,
                    input_usd_per_mtok: own.input_usd_per_mtok,
                    output_usd_per_mtok: own.output_usd_per_mtok,
                },
                effective: TableModelEffective {
                    alias: entry.alias.clone(),
                    enabled: entry.enabled,
                    tier: entry.tier.as_str().into(),
                    tags: entry.tags.clone(),
                    context_window: entry.context_window,
                    input_usd_per_mtok: pricing.map(|p| p.input_usd_per_mtok),
                    output_usd_per_mtok: pricing.map(|p| p.output_usd_per_mtok),
                },
            }
        })
        .collect();
    ModelConfigResult {
        local,
        table,
        defaults: effective(&llama.default_launch()),
        cache_types: CACHE_TYPES.iter().map(ToString::to_string).collect(),
        file: state.paths().app_models_file().display().to_string(),
    }
}
