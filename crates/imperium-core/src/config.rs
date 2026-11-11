//! `config.describe` and `config.set`: the settings table from `imperium-config`, read from and
//! written to the user's `config.toml`. The running daemon keeps the settings it started with;
//! each entry's `restart` flag says when a restart is needed for a change to apply.

use imperium_config::{SetError, SettingState};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    ConfigDescribeResult, ConfigEntry, ConfigSetParams, ConfigSetResult, PersonalitiesListResult,
    PersonalitySummary,
};
use imperium_workflow::DEFAULT_PERSONALITY;
use imperium_workflow::personality::Origin;

use crate::state::DaemonState;

/// Every setting as the daemon would load it now (user file and environment, no project).
pub fn describe(state: &DaemonState) -> Result<ConfigDescribeResult, ErrorObject> {
    let paths = state.paths();
    let config = imperium_config::load(paths, None, std::env::vars())
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?;
    Ok(ConfigDescribeResult {
        entries: imperium_config::describe(&config)
            .iter()
            .map(entry)
            .collect(),
        user_file: paths.user_config_file().display().to_string(),
    })
}

/// Writes one setting to the user file; an invalid change is refused and nothing is saved.
pub fn set(state: &DaemonState, params: ConfigSetParams) -> Result<ConfigSetResult, ErrorObject> {
    let updated = imperium_config::set_user_setting(
        state.paths(),
        &params.key,
        &params.value,
        std::env::vars(),
    )
    .map_err(|error| {
        let code = match error {
            SetError::Write { .. } => codes::INTERNAL_ERROR,
            _ => codes::INVALID_PARAMS,
        };
        ErrorObject::new(code, error.to_string())
    })?;
    tracing::info!(key = %params.key, "a client changed a setting in the user config");
    if params.key == crate::suggestions::SETTING {
        state
            .suggestions()
            .set_enabled(updated.value.as_bool().unwrap_or(true));
    }
    crate::onboarding::apply_setting(state, &params.key);
    Ok(ConfigSetResult {
        restart: updated.def.restart,
        overridden: updated.overridden(),
        entry: entry(&updated),
    })
}

/// `personalities.list`: what `policy.personality` may name, each id as the file that wins.
pub fn personalities(state: &DaemonState) -> PersonalitiesListResult {
    PersonalitiesListResult {
        personalities: state
            .personality_sources()
            .list()
            .into_iter()
            .map(|info| PersonalitySummary {
                source: info.origin.kind().into(),
                pack: match info.origin {
                    Origin::Pack(name) => Some(name),
                    Origin::Local | Origin::BuiltIn => None,
                },
                id: info.id,
                name: info.name,
                overrides: info.overrides,
            })
            .collect(),
        default: DEFAULT_PERSONALITY.into(),
    }
}

fn entry(state: &SettingState) -> ConfigEntry {
    let def = state.def;
    ConfigEntry {
        key: def.key.into(),
        section: def.section().into(),
        label: def.label.into(),
        description: def.description.into(),
        kind: def.kind.as_str().into(),
        options: def
            .options
            .iter()
            .map(|option| option.to_string())
            .collect(),
        default: state.default.clone(),
        value: state.value.clone(),
        source: state.source.kind().into(),
        restart: def.restart,
    }
}
