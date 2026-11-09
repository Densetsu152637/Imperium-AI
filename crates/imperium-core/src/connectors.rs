//! Connectors: the `mcp.*` methods behind the app's connector browser. MCP servers are
//! `[mcp.servers.<name>]` entries in the user's `config.toml`, written with `imperium-config`
//! (comments kept, secrets only as references); secret values go to the daemon's secret store.
//! The catalogue of servers to browse is data: `catalog/connectors.json`, built in, extended or
//! overridden by id from `<config>/connectors.json`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use imperium_config::{McpServerSettings, Paths, SecretName, SecretRef, SetError, Settings};
use imperium_mcp::McpServers;
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    CatalogServer, McpCatalogResult, McpInstallParams, McpListResult, McpRemoveParams,
    McpRemoveResult, McpSaveParams, McpSaveResult, McpServerConfig, McpServerInfo,
    McpSetEnabledParams, McpStatus, McpTestParams, PluginInfo, SkillInfo,
};
use serde::Deserialize;

use crate::state::DaemonState;

/// The built-in catalogue.
const BUILT_IN: &str = include_str!("../catalog/connectors.json");

/// How long a connection test may take, including a first `npx` or `uvx` download.
const TEST_TIMEOUT: Duration = Duration::from_secs(90);

/// The last test result for each server, by name.
#[derive(Debug, Default)]
pub struct StatusCache(Mutex<HashMap<String, McpStatus>>);

impl StatusCache {
    fn get(&self, name: &str) -> Option<McpStatus> {
        self.lock().get(name).cloned()
    }

    fn set(&self, name: &str, status: McpStatus) {
        self.lock().insert(name.to_string(), status);
    }

    fn forget(&self, name: &str) {
        self.lock().remove(name);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, McpStatus>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The settings in the user's config file now (with the environment, no project), or `None`
/// when there is no user file or it does not load.
pub fn saved_settings(paths: &Paths) -> Option<Settings> {
    if !paths.user_config_file().exists() {
        return None;
    }
    imperium_config::load(paths, None, std::env::vars())
        .and_then(|config| config.settings())
        .ok()
}

fn current(state: &DaemonState) -> Result<Settings, ErrorObject> {
    imperium_config::load(state.paths(), None, std::env::vars())
        .and_then(|config| config.settings())
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))
}

fn invalid(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message.into())
}

fn set_error(error: SetError) -> ErrorObject {
    let code = match error {
        SetError::Write { .. } => codes::INTERNAL_ERROR,
        _ => codes::INVALID_PARAMS,
    };
    ErrorObject::new(code, error.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

// ---- mcp.list ----

/// `mcp.list`.
pub fn list(state: &DaemonState) -> McpListResult {
    let mut problems = Vec::new();
    let settings = current(state).unwrap_or_else(|error| {
        problems.push(error.message);
        (*state.settings()).clone()
    });
    let servers = settings
        .mcp
        .servers
        .iter()
        .map(|(name, server)| info(state, name, server))
        .collect();

    let config_dir = state.paths().config_dir();
    let mut plugin_dirs = vec![config_dir.join("plugins")];
    plugin_dirs.extend(settings.extensions.plugin_dirs.iter().cloned());
    let (plugins, mut plugin_problems) =
        imperium_extensions::plugins::discover(&plugin_dirs, Duration::from_secs(60));
    problems.append(&mut plugin_problems);

    let mut skill_dirs = vec![(config_dir.join("skills"), "user".to_string())];
    skill_dirs.extend(
        plugins
            .iter()
            .map(|plugin| (plugin.skills_dir(), plugin.origin())),
    );
    let (skills, mut skill_problems) = imperium_extensions::skills::discover(&skill_dirs);
    problems.append(&mut skill_problems);

    let plugins = plugins
        .iter()
        .map(|plugin| PluginInfo {
            name: plugin.name.clone(),
            root: plugin.root.display().to_string(),
            enabled: !settings.extensions.disabled_plugins.contains(&plugin.name),
            skills: count(&plugin.skills_dir(), |path| path.join("SKILL.md").is_file()),
            commands: count(&plugin.commands_dir(), |path| {
                path.extension().is_some_and(|extension| extension == "md")
            }),
            hooks: u32::try_from(plugin.hooks.len()).unwrap_or(u32::MAX),
            servers: plugin
                .servers
                .iter()
                .map(|server| server.name.clone())
                .collect(),
        })
        .collect();
    McpListResult {
        servers,
        plugins,
        skills: skills
            .into_iter()
            .map(|skill| SkillInfo {
                name: skill.name,
                description: skill.description,
                origin: skill.origin,
                dir: skill.dir.display().to_string(),
            })
            .collect(),
        user_file: state.paths().user_config_file().display().to_string(),
        problems,
    }
}

/// Entries of `dir` that `keep` accepts.
fn count(dir: &Path, keep: impl Fn(&PathBuf) -> bool) -> u32 {
    let found = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| keep(&entry.path()))
                .count()
        })
        .unwrap_or(0);
    u32::try_from(found).unwrap_or(u32::MAX)
}

fn secret_refs(server: &McpServerSettings) -> impl Iterator<Item = &SecretRef> {
    server
        .secret_env
        .values()
        .chain(server.bearer_token.as_ref())
}

fn info(state: &DaemonState, name: &str, server: &McpServerSettings) -> McpServerInfo {
    let missing_secrets: Vec<String> = secret_refs(server)
        .filter(|reference| matches!(state.secrets().load(&reference.secret), Ok(None)))
        .map(|reference| reference.secret.to_string())
        .collect();
    let mut status = state
        .connector_status()
        .get(name)
        .unwrap_or_else(|| McpStatus {
            state: "untested".into(),
            tools: Vec::new(),
            error: None,
            checked_at: None,
            missing_secrets: Vec::new(),
        });
    if !server.enabled {
        status.state = "disabled".into();
    }
    status.missing_secrets = missing_secrets;
    McpServerInfo {
        name: name.to_string(),
        config: to_wire(server),
        status,
    }
}

fn to_wire(server: &McpServerSettings) -> McpServerConfig {
    McpServerConfig {
        command: server.command.clone(),
        args: server.args.clone(),
        env: server.env.clone(),
        secret_env: server
            .secret_env
            .iter()
            .map(|(key, reference)| (key.clone(), reference.secret.to_string()))
            .collect(),
        url: server.url.clone(),
        headers: server.headers.clone(),
        bearer_secret: server
            .bearer_token
            .as_ref()
            .map(|reference| reference.secret.to_string()),
        allow: server.allow.clone(),
        enabled: server.enabled,
        timeout_s: server.timeout_s,
        catalog: server.catalog.clone(),
    }
}

fn secret_ref(name: &str) -> Result<SecretRef, ErrorObject> {
    let secret = name
        .parse::<SecretName>()
        .map_err(|error| invalid(error.to_string()))?;
    Ok(SecretRef { secret })
}

fn from_wire(config: &McpServerConfig) -> Result<McpServerSettings, ErrorObject> {
    let blank = |text: &Option<String>| {
        text.as_ref()
            .filter(|text| !text.trim().is_empty())
            .cloned()
    };
    Ok(McpServerSettings {
        command: blank(&config.command),
        args: config.args.clone(),
        env: config.env.clone(),
        secret_env: config
            .secret_env
            .iter()
            .map(|(key, name)| Ok((key.clone(), secret_ref(name)?)))
            .collect::<Result<_, ErrorObject>>()?,
        url: blank(&config.url),
        headers: config.headers.clone(),
        bearer_token: blank(&config.bearer_secret)
            .map(|name| secret_ref(&name))
            .transpose()?,
        allow: config.allow.clone(),
        enabled: config.enabled,
        timeout_s: config.timeout_s,
        catalog: blank(&config.catalog),
    })
}

// ---- mcp.save, mcp.install, mcp.set_enabled, mcp.remove ----

/// `mcp.save`.
pub fn save(state: &DaemonState, params: McpSaveParams) -> Result<McpSaveResult, ErrorObject> {
    if !imperium_config::valid_server_name(&params.name) {
        return Err(invalid(format!(
            "{:?} cannot name a server: use up to {} lowercase letters, digits, '_' or '-'",
            params.name,
            imperium_config::MAX_SERVER_NAME
        )));
    }
    let server = from_wire(&params.config)?;
    if params.create && current(state)?.mcp.servers.contains_key(&params.name) {
        return Err(invalid(format!(
            "a connector named {} already exists",
            params.name
        )));
    }
    // Secrets are stored first, so the saved server never refers to a secret that was refused.
    let mut secrets = Vec::new();
    for (name, value) in &params.secrets {
        let reference = secret_ref(name)?;
        if value.is_empty() {
            return Err(invalid(format!("secret {name} is empty")));
        }
        secrets.push((reference.secret, value));
    }
    for (name, value) in secrets {
        state
            .secrets()
            .store(&name, value)
            .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?;
    }
    let saved =
        imperium_config::save_mcp_server(state.paths(), &params.name, &server, std::env::vars())
            .map_err(set_error)?;
    state.connector_status().forget(&params.name);
    tracing::info!(server = %params.name, "a client saved an MCP server in the user config");
    Ok(McpSaveResult {
        server: info(state, &params.name, &saved),
    })
}

/// `mcp.install`: a catalogue entry, filled in and saved under a new name.
pub fn install(
    state: &DaemonState,
    params: McpInstallParams,
) -> Result<McpSaveResult, ErrorObject> {
    let catalog = catalog(state);
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == params.catalog_id)
        .ok_or_else(|| {
            invalid(format!(
                "no connector {:?} in the catalogue",
                params.catalog_id
            ))
        })?;
    let name = params
        .name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| entry.id.clone());
    let (config, secrets) = expand(entry, &name, &params.values).map_err(invalid)?;
    save(
        state,
        McpSaveParams {
            name,
            config,
            secrets,
            create: true,
        },
    )
}

/// `mcp.set_enabled`.
pub fn set_enabled(
    state: &DaemonState,
    params: McpSetEnabledParams,
) -> Result<McpSaveResult, ErrorObject> {
    let saved = imperium_config::set_mcp_server_enabled(
        state.paths(),
        &params.name,
        params.enabled,
        std::env::vars(),
    )
    .map_err(set_error)?;
    Ok(McpSaveResult {
        server: info(state, &params.name, &saved),
    })
}

/// `mcp.remove`.
pub fn remove(
    state: &DaemonState,
    params: McpRemoveParams,
) -> Result<McpRemoveResult, ErrorObject> {
    let removed = imperium_config::remove_mcp_server(state.paths(), &params.name, std::env::vars())
        .map_err(set_error)?;
    state.connector_status().forget(&params.name);
    let Some(secrets) = removed else {
        return Ok(McpRemoveResult { removed: false });
    };
    if params.forget_secrets {
        for reference in secrets {
            if let Err(error) = state.secrets().delete(&reference.secret) {
                tracing::warn!(secret = %reference.secret, %error, "could not delete a connector's secret");
            }
        }
    }
    tracing::info!(server = %params.name, "a client removed an MCP server from the user config");
    Ok(McpRemoveResult { removed: true })
}

// ---- mcp.test ----

/// `mcp.test`: connects to the server as a session would, lists its tools, then stops it.
pub async fn test(
    state: Arc<DaemonState>,
    params: McpTestParams,
) -> Result<McpStatus, ErrorObject> {
    {
        let settings = current(&state)?;
        let server = settings
            .mcp
            .servers
            .get(&params.name)
            .ok_or_else(|| invalid(format!("no connector named {}", params.name)))?;
        let spec = crate::mcp::configured(&params.name, server, &|name: &SecretName| {
            state.secrets().load(name)
        });
        let outcome = match spec {
            Err(reason) => Err(reason),
            Ok(spec) => {
                match tokio::time::timeout(TEST_TIMEOUT, McpServers::connect(vec![spec])).await {
                    Err(_) => Err(format!(
                        "no answer within {} seconds",
                        TEST_TIMEOUT.as_secs()
                    )),
                    Ok((servers, failed)) => match failed.into_iter().next() {
                        Some(failed) => Err(failed.reason),
                        None => Ok(servers.tools().map(|tool| tool.name.clone()).collect()),
                    },
                }
            }
        };
        let status = match outcome {
            Ok(tools) => McpStatus {
                state: "connected".into(),
                tools,
                error: None,
                checked_at: Some(now_ms()),
                missing_secrets: Vec::new(),
            },
            Err(error) => McpStatus {
                state: "error".into(),
                tools: Vec::new(),
                error: Some(error),
                checked_at: Some(now_ms()),
                missing_secrets: Vec::new(),
            },
        };
        state.connector_status().set(&params.name, status);
        Ok(info(&state, &params.name, server).status)
    }
}

// ---- mcp.catalog ----

#[derive(Deserialize)]
struct CatalogFile {
    entries: Vec<CatalogServer>,
}

/// `mcp.catalog`: the built-in entries, then the user's file (same id replaces, new ids add).
pub fn catalog(state: &DaemonState) -> McpCatalogResult {
    let file = state.paths().config_dir().join("connectors.json");
    let mut problems = Vec::new();
    let mut entries = serde_json::from_str::<CatalogFile>(BUILT_IN)
        .expect("the built-in catalogue is valid (covered by tests)")
        .entries;
    match std::fs::read_to_string(&file) {
        Ok(text) => match serde_json::from_str::<CatalogFile>(&text) {
            Ok(extra) => {
                for entry in extra.entries {
                    match entries.iter_mut().find(|existing| existing.id == entry.id) {
                        Some(existing) => *existing = entry,
                        None => entries.push(entry),
                    }
                }
            }
            Err(error) => problems.push(format!("{}: {error}", file.display())),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => problems.push(format!("{}: {error}", file.display())),
    }
    McpCatalogResult {
        entries,
        override_file: file.display().to_string(),
        problems,
    }
}

/// Fills a catalogue entry's templates with `values`, giving the server's settings and the
/// secrets to store (secret name to value).
fn expand(
    entry: &CatalogServer,
    name: &str,
    values: &BTreeMap<String, String>,
) -> Result<(McpServerConfig, BTreeMap<String, String>), String> {
    let mut filled = BTreeMap::new();
    for field in &entry.fields {
        let value = values
            .get(&field.id)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| field.default.clone())
            .unwrap_or_default();
        if field.required && value.is_empty() {
            return Err(format!("{} is required", field.label));
        }
        filled.insert(field.id.as_str(), (field.kind.as_str(), value));
    }
    // A whole-value `{{field}}` reference, if `text` is one.
    let whole = |text: &str| {
        let id = text.strip_prefix("{{")?.strip_suffix("}}")?.trim();
        filled.get_key_value(id).map(|(id, field)| (*id, field))
    };
    let substitute = |text: &str| -> Result<String, String> {
        let mut out = text.to_string();
        for (id, (kind, value)) in &filled {
            let pattern = format!("{{{{{id}}}}}");
            if out.contains(&pattern) {
                if *kind == "secret" {
                    return Err(format!(
                        "catalogue entry {}: secret field {id} must be a whole env value or bearer",
                        entry.id
                    ));
                }
                out = out.replace(&pattern, value);
            }
        }
        Ok(out)
    };
    let mut secrets = BTreeMap::new();
    let mut secret_name = |id: &str, value: &str| -> Result<String, String> {
        let secret = format!("mcp.{name}.{id}").replace('_', "-");
        secret
            .parse::<SecretName>()
            .map_err(|error| error.to_string())?;
        secrets.insert(secret.clone(), value.to_string());
        Ok(secret)
    };

    let mut args = Vec::new();
    for arg in &entry.args {
        match whole(arg) {
            Some((_, ("path_list", value))) => {
                args.extend(
                    value
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_string),
                );
            }
            Some((id, ("secret", _))) => {
                return Err(format!(
                    "catalogue entry {}: secret field {id} cannot be an argument",
                    entry.id
                ));
            }
            Some((_, (_, value))) if value.is_empty() => {}
            _ => args.push(substitute(arg)?),
        }
    }
    let mut env = BTreeMap::new();
    let mut secret_env = BTreeMap::new();
    for (key, template) in &entry.env {
        match whole(template) {
            Some((id, ("secret", value))) => {
                if !value.is_empty() {
                    secret_env.insert(key.clone(), secret_name(id, value)?);
                }
            }
            _ => {
                let value = substitute(template)?;
                if !value.is_empty() {
                    env.insert(key.clone(), value);
                }
            }
        }
    }
    let mut headers = BTreeMap::new();
    for (key, template) in &entry.headers {
        let value = substitute(template)?;
        if !value.is_empty() {
            headers.insert(key.clone(), value);
        }
    }
    let bearer_secret = match entry
        .bearer
        .as_deref()
        .map(|bearer| (bearer, whole(bearer)))
    {
        None => None,
        Some((_, Some((id, ("secret", value))))) => (!value.is_empty())
            .then(|| secret_name(id, value))
            .transpose()?,
        Some((bearer, _)) => {
            return Err(format!(
                "catalogue entry {}: bearer {bearer:?} must name a secret field",
                entry.id
            ));
        }
    };
    let url = entry.url.as_deref().map(substitute).transpose()?;
    Ok((
        McpServerConfig {
            command: entry.command.clone(),
            args,
            env,
            secret_env,
            url,
            headers,
            bearer_secret,
            allow: Vec::new(),
            enabled: true,
            timeout_s: None,
            catalog: Some(entry.id.clone()),
        },
        secrets,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built_in() -> Vec<CatalogServer> {
        serde_json::from_str::<CatalogFile>(BUILT_IN)
            .unwrap()
            .entries
    }

    #[test]
    fn the_built_in_catalogue_is_valid() {
        let entries = built_in();
        assert!(entries.len() >= 8);
        let mut ids = std::collections::BTreeSet::new();
        for entry in &entries {
            assert!(ids.insert(&entry.id), "{} twice", entry.id);
            assert!(
                imperium_config::valid_server_name(&entry.id),
                "{}",
                entry.id
            );
            assert!(
                entry.command.is_some() != entry.url.is_some(),
                "{}: one of command or url",
                entry.id
            );
            for field in &entry.fields {
                assert!(
                    ["text", "secret", "path", "path_list", "url"].contains(&field.kind.as_str()),
                    "{}.{}",
                    entry.id,
                    field.id
                );
            }
            // Every entry expands with placeholder values for its required fields.
            let values = entry
                .fields
                .iter()
                .map(|field| (field.id.clone(), "x".to_string()))
                .collect();
            expand(entry, &entry.id, &values).unwrap_or_else(|error| panic!("{error}"));
        }
        for wanted in [
            "filesystem",
            "git",
            "github",
            "fetch",
            "memory",
            "postgres",
            "slack",
            "brave-search",
        ] {
            assert!(ids.contains(&wanted.to_string()), "{wanted}");
        }
    }

    #[test]
    fn expansion_fills_templates_and_moves_secrets_to_references() {
        let entries = built_in();
        let get = |id: &str| entries.iter().find(|entry| entry.id == id).unwrap();

        let values = BTreeMap::from([("folders".to_string(), "C:/a\n\n C:/b ".to_string())]);
        let (config, secrets) = expand(get("filesystem"), "files", &values).unwrap();
        assert_eq!(
            config.args,
            [
                "-y",
                "@modelcontextprotocol/server-filesystem",
                "C:/a",
                "C:/b"
            ]
        );
        assert!(secrets.is_empty());
        assert_eq!(config.catalog.as_deref(), Some("filesystem"));

        let values = BTreeMap::from([
            ("bot_token".to_string(), "xoxb-1".to_string()),
            ("team_id".to_string(), "T1".to_string()),
        ]);
        let (config, secrets) = expand(get("slack"), "slack", &values).unwrap();
        assert_eq!(
            config.env,
            BTreeMap::from([("SLACK_TEAM_ID".into(), "T1".into())])
        );
        assert_eq!(
            config.secret_env,
            BTreeMap::from([("SLACK_BOT_TOKEN".into(), "mcp.slack.bot-token".into())])
        );
        assert_eq!(
            secrets,
            BTreeMap::from([("mcp.slack.bot-token".into(), "xoxb-1".into())])
        );

        let values = BTreeMap::from([("token".to_string(), "pat".to_string())]);
        let (config, secrets) = expand(get("github"), "gh", &values).unwrap();
        assert_eq!(config.bearer_secret.as_deref(), Some("mcp.gh.token"));
        assert!(config.url.is_some() && config.command.is_none());
        assert_eq!(secrets["mcp.gh.token"], "pat");

        // Optional empty fields are left out; required ones are refused.
        let (config, _) = expand(get("memory"), "memory", &BTreeMap::new()).unwrap();
        assert!(config.env.is_empty());
        let error = expand(get("slack"), "slack", &BTreeMap::new()).unwrap_err();
        assert!(error.contains("required"), "{error}");
    }
}
