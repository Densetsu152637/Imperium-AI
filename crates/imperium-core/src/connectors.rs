//! Connectors: the `mcp.*` methods behind the app's connector browser. MCP servers are
//! `[mcp.servers.<name>]` entries in the user's `config.toml`, written with `imperium-config`
//! (comments kept, secrets only as references); secret values go to the daemon's secret store.
//! The catalogue of servers to browse is data: `catalog/connectors.json`, built in, extended or
//! overridden by id from `<config>/connectors.json`.

use imperium_api::time::unix_ms;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use imperium_config::{
    McpServerSettings, Paths, SecretName, SecretRef, SecretStore, SetError, Settings,
};
use imperium_mcp::oauth::{CredentialStore, OAuthSource};
use imperium_mcp::{McpServers, NEEDS_SIGN_IN};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    AgentFileInfo, CatalogServer, McpCatalogResult, McpInstallParams, McpListResult,
    McpLoginParams, McpLoginResult, McpLogoutParams, McpLogoutResult, McpRemoveParams,
    McpRemoveResult, McpSaveParams, McpSaveResult, McpServerConfig, McpServerInfo,
    McpSetEnabledParams, McpStatus, McpTestParams, PluginInfo, SkillInfo,
};
use serde::Deserialize;

use crate::state::DaemonState;

/// The built-in catalogue.
const BUILT_IN: &str = include_str!("../catalog/connectors.json");

/// How long a connection test may take, including a first `npx` or `uvx` download.
const TEST_TIMEOUT: Duration = Duration::from_secs(90);

/// A sign-in waiting for the user's browser.
#[derive(Debug)]
struct SignIn {
    id: u64,
    task: tokio::task::JoinHandle<()>,
}

/// The last test result for each server, by name, and the OAuth sign-ins in progress.
#[derive(Debug, Default)]
pub struct StatusCache {
    statuses: Mutex<HashMap<String, McpStatus>>,
    sign_ins: Mutex<HashMap<String, SignIn>>,
    next_sign_in: AtomicU64,
    /// One token source per server for the life of the daemon, so a refresh in one session
    /// cannot race a refresh in another over a rotating refresh token.
    sources: Mutex<HashMap<String, Arc<OAuthSource>>>,
    /// Whether a server has a sign-in stored, kept for a few seconds: the list is polled while a
    /// sign-in waits and each answer would otherwise read the keychain several times per server.
    signed_in: Mutex<HashMap<String, (std::time::Instant, bool)>>,
}

/// How long a "signed in" answer for `mcp.list` is reused.
const SIGNED_IN_TTL: Duration = Duration::from_secs(5);

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
        self.statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sign_ins(&self) -> std::sync::MutexGuard<'_, HashMap<String, SignIn>> {
        self.sign_ins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The shared token source of `name`.
    pub(crate) fn oauth_source(
        &self,
        name: &str,
        secrets: &Arc<dyn SecretStore>,
    ) -> Arc<OAuthSource> {
        let mut sources = self
            .sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(sources.entry(name.to_string()).or_insert_with(|| {
            let store: Arc<dyn CredentialStore> =
                Arc::new(crate::mcp::KeychainCredentials(Arc::clone(secrets)));
            Arc::new(OAuthSource::new(name, store))
        }))
    }

    fn drop_source(&self, name: &str) {
        self.sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(name);
    }

    /// `check()`'s answer for `name`, reused for [`SIGNED_IN_TTL`].
    fn signed_in(&self, name: &str, check: impl FnOnce() -> bool) -> bool {
        let mut cache = self
            .signed_in
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, answer)) = cache.get(name)
            && at.elapsed() < SIGNED_IN_TTL
        {
            return *answer;
        }
        let answer = check();
        cache.insert(name.to_string(), (std::time::Instant::now(), answer));
        answer
    }

    fn forget_signed_in(&self, name: &str) {
        self.signed_in
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(name);
    }

    fn signing_in(&self, name: &str) -> bool {
        self.sign_ins().contains_key(name)
    }

    /// Runs `work` (a sign-in waiting for the browser) as the server's sign-in, replacing and
    /// cancelling an earlier one. `work` is given the id to pass to [`StatusCache::end_sign_in`].
    fn start_sign_in<F>(&self, name: &str, work: impl FnOnce(u64) -> F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let id = self.next_sign_in.fetch_add(1, Ordering::Relaxed);
        // The map stays locked until the task is registered, so a task that ends at once finds
        // its own entry to remove.
        let mut sign_ins = self.sign_ins();
        let task = tokio::spawn(work(id));
        if let Some(replaced) = sign_ins.insert(name.to_string(), SignIn { id, task }) {
            replaced.task.abort();
        }
    }

    /// Removes the sign-in `id` of `name`, unless another has replaced it.
    fn end_sign_in(&self, name: &str, id: u64) {
        let mut sign_ins = self.sign_ins();
        if sign_ins.get(name).is_some_and(|current| current.id == id) {
            sign_ins.remove(name);
        }
    }

    /// Cancels a sign-in waiting for the browser; whether there was one.
    fn cancel_sign_in(&self, name: &str) -> bool {
        match self.sign_ins().remove(name) {
            Some(sign_in) => {
                sign_in.task.abort();
                true
            }
            None => false,
        }
    }
}

/// The settings in the user's config file now (with the environment, no project), or `None`
/// when there is no user file or it does not load.
pub(crate) fn saved_settings(paths: &Paths) -> Option<Settings> {
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

// ---- mcp.list ----

/// `mcp.list`.
pub(crate) fn list(state: &DaemonState) -> McpListResult {
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
    let (plugins, mut plugin_problems) = imperium_extensions::plugins::discover(
        &plugin_dirs,
        Duration::from_secs(60),
        &imperium_extensions::plugins::agent_installed_folders(&plugin_dirs),
    );
    problems.append(&mut plugin_problems);

    let mut skill_dirs = vec![(config_dir.join("skills"), "user".to_string())];
    skill_dirs.extend(
        plugins
            .iter()
            .map(|plugin| (plugin.skills_dir(), plugin.origin())),
    );
    let (skills, mut skill_problems) = imperium_extensions::skills::discover(&skill_dirs);
    problems.append(&mut skill_problems);

    // Named agents of plugins and the user (a project's are loaded per session, for trusted
    // projects only).
    let agent_folders = agent_folders(&plugins, &settings.extensions.disabled_plugins, config_dir);
    let (agents, mut agent_problems) = imperium_extensions::agents::discover(&agent_folders);
    problems.append(&mut agent_problems);

    let gate =
        crate::plugin_records::gate(state.paths(), crate::plugin_records::production_verifier());
    let plugins = plugins
        .iter()
        .map(|plugin| {
            let record = crate::plugin_records::record_for(state.paths(), &plugin.root);
            let withheld = gate
                .withheld
                .iter()
                .find(|withheld| withheld.folder == plugin.root);
            PluginInfo {
                id: record.as_ref().and_then(|record| record.id.clone()),
                origin: record.as_ref().map(|record| record.origin.as_str().into()),
                withheld: withheld.is_some(),
                withheld_reason: withheld.map(|withheld| withheld.reason.clone()),
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
                installed_from: crate::extension_install::read_source(&plugin.root),
            }
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
                installed_from: crate::extension_install::read_source(&skill.dir),
                origin: skill.origin,
                dir: skill.dir.display().to_string(),
            })
            .collect(),
        agents: agents
            .into_iter()
            .map(|agent| AgentFileInfo {
                name: agent.name,
                description: agent.description,
                origin: agent.origin,
                path: agent.path.display().to_string(),
                tools: agent.tools,
                model: agent.model,
            })
            .collect(),
        user_file: state.paths().user_config_file().display().to_string(),
        problems,
    }
}

/// Where named agents are read from, lowest precedence first: enabled plugins' `agents/`, then the
/// user's.
fn agent_folders(
    plugins: &[imperium_extensions::plugins::Plugin],
    disabled: &[String],
    config_dir: &Path,
) -> Vec<imperium_extensions::AgentFolder> {
    let mut folders: Vec<imperium_extensions::AgentFolder> = plugins
        .iter()
        .filter(|plugin| !disabled.contains(&plugin.name))
        .map(|plugin| imperium_extensions::AgentFolder::new(plugin.agents_dir(), plugin.origin()))
        .collect();
    folders.push(imperium_extensions::AgentFolder::new(
        config_dir.join("agents"),
        "user",
    ));
    folders
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
            sign_in: None,
        });
    if !server.enabled {
        status.state = "disabled".into();
    }
    status.missing_secrets = missing_secrets;
    status.sign_in = sign_in_state(state, name, server, &status);
    McpServerInfo {
        name: name.to_string(),
        config: to_wire(server),
        status,
    }
}

/// Where a remote server's OAuth sign-in stands, or `None` for a server that takes none.
fn sign_in_state(
    state: &DaemonState,
    name: &str,
    server: &McpServerSettings,
    status: &McpStatus,
) -> Option<String> {
    let url = server.url.as_deref()?;
    if server.bearer_token.is_some()
        || server
            .headers
            .keys()
            .any(|header| header.eq_ignore_ascii_case("authorization"))
    {
        return None;
    }
    if state.connector_status().signing_in(name) {
        return Some("signing_in".into());
    }
    let store = crate::mcp::KeychainCredentials(Arc::clone(state.secrets()));
    let signed_in = state.connector_status().signed_in(name, || {
        imperium_mcp::oauth::is_signed_in(&store, name, url)
    });
    if signed_in {
        return Some("signed_in".into());
    }
    if status
        .error
        .as_deref()
        .is_some_and(|error| error.contains(NEEDS_SIGN_IN))
    {
        return Some("needs_sign_in".into());
    }
    None
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
        // Not a connector form field: `save` keeps what the config file says.
        sampling: false,
        catalog: blank(&config.catalog),
    })
}

// ---- mcp.save, mcp.install, mcp.set_enabled, mcp.remove ----

/// `mcp.save`.
pub(crate) fn save(
    state: &DaemonState,
    params: McpSaveParams,
) -> Result<McpSaveResult, ErrorObject> {
    if !imperium_config::valid_server_name(&params.name) {
        return Err(invalid(format!(
            "{:?} cannot name a server: use up to {} lowercase letters, digits, '_' or '-'",
            params.name,
            imperium_config::MAX_SERVER_NAME
        )));
    }
    let mut server = from_wire(&params.config)?;
    let existing = current(state)?.mcp.servers.get(&params.name).cloned();
    if params.create && existing.is_some() {
        return Err(invalid(format!(
            "a connector named {} already exists",
            params.name
        )));
    }
    // Sampling is set in the config file only; saving from the form keeps it.
    server.sampling = existing.is_some_and(|existing| existing.sampling);
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
pub(crate) fn install(
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
pub(crate) fn set_enabled(
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
pub(crate) fn remove(
    state: &DaemonState,
    params: McpRemoveParams,
) -> Result<McpRemoveResult, ErrorObject> {
    let removed = imperium_config::remove_mcp_server(state.paths(), &params.name, std::env::vars())
        .map_err(set_error)?;
    state.connector_status().forget(&params.name);
    let Some(secrets) = removed else {
        return Ok(McpRemoveResult { removed: false });
    };
    // A removed server's sign-in goes with it: its tokens are not kept for a server that is gone.
    state.connector_status().cancel_sign_in(&params.name);
    let source = state
        .connector_status()
        .oauth_source(&params.name, state.secrets());
    state.connector_status().drop_source(&params.name);
    state.connector_status().forget_signed_in(&params.name);
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        // Under the source's refresh lock, so a refresh in flight cannot store tokens for a
        // server that is gone; then the token is revoked where the authorization server allows.
        let name = params.name.clone();
        runtime.spawn(async move {
            match source.forget_for_removal().await {
                Ok(Some(revocation)) => revocation.send().await,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(server = %name, %error, "could not delete a connector's sign-in");
                }
            }
        });
    } else {
        let store = crate::mcp::KeychainCredentials(Arc::clone(state.secrets()));
        if let Err(error) = imperium_mcp::oauth::forget_for_removal(&store, &params.name) {
            tracing::warn!(server = %params.name, %error, "could not delete a connector's sign-in");
        }
    }
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
pub(crate) async fn test(
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
                let auths = crate::mcp::oauth_sources(
                    std::slice::from_ref(&spec),
                    state.secrets(),
                    state.connector_status(),
                );
                match tokio::time::timeout(
                    TEST_TIMEOUT,
                    McpServers::connect_with(vec![spec], auths),
                )
                .await
                {
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
                checked_at: Some(unix_ms()),
                missing_secrets: Vec::new(),
                sign_in: None,
            },
            Err(error) => McpStatus {
                state: "error".into(),
                tools: Vec::new(),
                error: Some(error),
                checked_at: Some(unix_ms()),
                missing_secrets: Vec::new(),
                sign_in: None,
            },
        };
        state.connector_status().set(&params.name, status);
        Ok(info(&state, &params.name, server).status)
    }
}

// ---- mcp.login, mcp.logout ----

/// The URL of a server that can be signed in to: a remote server of the user's config without a
/// token of its own.
fn sign_in_url(state: &DaemonState, name: &str) -> Result<String, ErrorObject> {
    let settings = current(state)?;
    let server = settings
        .mcp
        .servers
        .get(name)
        .ok_or_else(|| invalid(format!("no connector named {name}")))?;
    let Some(url) = server.url.clone() else {
        return Err(invalid(format!(
            "{name} is a local command; only remote servers take a sign-in"
        )));
    };
    if server.bearer_token.is_some()
        || server
            .headers
            .keys()
            .any(|header| header.eq_ignore_ascii_case("authorization"))
    {
        return Err(invalid(format!(
            "{name} sends a fixed token or Authorization header; remove it to sign in with OAuth"
        )));
    }
    if !imperium_config::valid_server_name(name) {
        return Err(invalid(format!("{name} cannot be signed in to")));
    }
    Ok(url)
}

/// `mcp.login`: discovers the server's authorization server, registers Imperium, listens on
/// `127.0.0.1` for the redirect, and returns the URL for the user to open in their own browser.
/// The sign-in then finishes in the background; it shows as `signing_in` until it ends.
pub(crate) async fn login(
    state: Arc<DaemonState>,
    params: McpLoginParams,
) -> Result<McpLoginResult, ErrorObject> {
    let url = sign_in_url(&state, &params.name)?;
    let store: Arc<dyn CredentialStore> =
        Arc::new(crate::mcp::KeychainCredentials(Arc::clone(state.secrets())));
    let pending = imperium_mcp::oauth::begin(&params.name, &url, store)
        .await
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?;
    let authorization_url = pending.authorization_url().to_string();
    let name = params.name;
    let task_name = name.clone();
    let background = Arc::clone(&state);
    state
        .connector_status()
        .start_sign_in(&name, move |id| async move {
            let status = background.connector_status();
            match pending.finish().await {
                Ok(()) => {
                    tracing::info!(server = %task_name, "signed in to an MCP server");
                    // Whatever the shared source kept in memory is older than this sign-in.
                    status
                        .oauth_source(&task_name, background.secrets())
                        .reset()
                        .await;
                    status.forget(&task_name);
                    status.forget_signed_in(&task_name);
                }
                Err(error) => {
                    tracing::info!(server = %task_name, %error, "an MCP sign-in did not finish");
                    status.set(
                        &task_name,
                        McpStatus {
                            state: "error".into(),
                            tools: Vec::new(),
                            error: Some(format!("sign-in failed: {error}")),
                            checked_at: Some(unix_ms()),
                            missing_secrets: Vec::new(),
                            sign_in: None,
                        },
                    );
                }
            }
            status.forget_signed_in(&task_name);
            status.end_sign_in(&task_name, id);
        });
    Ok(McpLoginResult { authorization_url })
}

/// `mcp.logout`: revokes the server's token where it offers revocation, forgets the sign-in,
/// and cancels one that is still waiting for the browser.
pub(crate) async fn logout(
    state: Arc<DaemonState>,
    params: McpLogoutParams,
) -> Result<McpLogoutResult, ErrorObject> {
    if !imperium_config::valid_server_name(&params.name) {
        return Err(invalid(format!("{:?} is not a server name", params.name)));
    }
    let cancelled = state.connector_status().cancel_sign_in(&params.name);
    // The same source the sessions use, so a refresh in progress finishes before the tokens go.
    let source = state
        .connector_status()
        .oauth_source(&params.name, state.secrets());
    let signed_out = source
        .logout()
        .await
        .map_err(|error| ErrorObject::new(codes::INTERNAL_ERROR, error.to_string()))?;
    state.connector_status().forget(&params.name);
    state.connector_status().forget_signed_in(&params.name);
    tracing::info!(server = %params.name, "signed out of an MCP server");
    Ok(McpLogoutResult {
        signed_out: signed_out || cancelled,
    })
}

// ---- mcp.catalog ----

#[derive(Deserialize)]
struct CatalogFile {
    entries: Vec<CatalogServer>,
}

/// `mcp.catalog`: the built-in entries, then the user's file (same id replaces, new ids add).
pub(crate) fn catalog(state: &DaemonState) -> McpCatalogResult {
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
    fn agents_of_disabled_plugins_are_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["on", "off"] {
            let root = dir.path().join(name);
            std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
            std::fs::write(
                root.join(".claude-plugin/plugin.json"),
                format!(r#"{{"name": "{name}"}}"#),
            )
            .unwrap();
        }
        let (plugins, problems) = imperium_extensions::plugins::discover(
            &[dir.path().to_path_buf()],
            Duration::from_secs(5),
            &[],
        );
        assert!(problems.is_empty(), "{problems:?}");
        let folders = agent_folders(&plugins, &["off".to_string()], Path::new("config"));
        let origins: Vec<&str> = folders
            .iter()
            .map(|folder| folder.origin.as_str())
            .collect();
        assert_eq!(origins, ["plugin:on", "user"]);
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

    #[test]
    fn saving_from_the_form_keeps_the_files_sampling_and_new_servers_start_without_it() {
        use imperium_config::{Config, Layer};
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let home = tempfile::tempdir().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let by_hand = McpServerSettings {
            command: Some("docs".into()),
            sampling: true,
            ..McpServerSettings::default()
        };
        imperium_config::save_mcp_server(&Paths::under(home.path()), "docs", &by_hand, []).unwrap();
        let params = |name: &str, create: bool| McpSaveParams {
            name: name.into(),
            config: McpServerConfig {
                command: Some("docs-v2".into()),
                args: Vec::new(),
                env: BTreeMap::new(),
                secret_env: BTreeMap::new(),
                url: None,
                headers: BTreeMap::new(),
                bearer_secret: None,
                allow: Vec::new(),
                enabled: true,
                timeout_s: None,
                catalog: None,
            },
            secrets: BTreeMap::new(),
            create,
        };
        save(&state, params("docs", false)).unwrap();
        let servers = current(&state).unwrap().mcp.servers;
        assert_eq!(servers["docs"].command.as_deref(), Some("docs-v2"));
        assert!(servers["docs"].sampling, "the form does not switch it off");
        save(&state, params("fresh", true)).unwrap();
        assert!(!current(&state).unwrap().mcp.servers["fresh"].sampling);
    }
}
