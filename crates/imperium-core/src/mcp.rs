//! Which MCP servers a session starts: `[mcp.servers]` from the user's configuration, plus the
//! project's `.mcp.json` when the project is in `mcp.trusted_projects`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use imperium_config::{McpServerSettings, SecretError, SecretName, SecretStore, Settings};
use imperium_mcp::oauth::{CredentialStore, OAuthSource};
use imperium_mcp::{Launch, ServerSpec, TokenSource, mcp_json};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The servers to start, and notices for the user about servers left out.
pub fn session_servers(
    settings: &Settings,
    project_dir: &Path,
    load_secret: impl Fn(&SecretName) -> Result<Option<String>, SecretError>,
) -> (Vec<ServerSpec>, Vec<String>) {
    let mut specs = Vec::new();
    let mut notices = Vec::new();
    for (name, server) in &settings.mcp.servers {
        if !server.enabled {
            continue;
        }
        match configured(name, server, &load_secret) {
            Ok(spec) => specs.push(spec),
            Err(reason) => notices.push(format!("MCP server {name} is unavailable: {reason}")),
        }
    }
    let file = project_dir.join(".mcp.json");
    let Ok(text) = std::fs::read_to_string(&file) else {
        return (specs, notices);
    };
    if !settings.mcp.trusts(project_dir) {
        notices.push(format!(
            "{} defines MCP servers that were not started. To run them, add this project to \
             mcp.trusted_projects in your user config.",
            file.display()
        ));
        return (specs, notices);
    }
    match mcp_json::parse(&text, project_dir, DEFAULT_TIMEOUT, |name| {
        std::env::var(name).ok()
    }) {
        Ok(project) => {
            for spec in project {
                if specs.iter().any(|existing| existing.name == spec.name) {
                    notices.push(format!(
                        "MCP server {} in .mcp.json is ignored: [mcp.servers.{}] has the same name",
                        spec.name, spec.name
                    ));
                } else {
                    specs.push(spec);
                }
            }
        }
        Err(error) => notices.push(format!("{} is invalid: {error}", file.display())),
    }
    (specs, notices)
}

/// OAuth sign-in state in the secret store (the OS keychain): one set of entries per server,
/// named `mcp.<server>.oauth.*`. Only the daemon's own code reads them; config never refers to
/// them, and they are never written to a file.
#[derive(Debug)]
pub struct KeychainCredentials(pub Arc<dyn SecretStore>);

impl KeychainCredentials {
    fn name(key: &str) -> Result<SecretName, String> {
        key.parse::<SecretName>().map_err(|error| error.to_string())
    }
}

impl CredentialStore for KeychainCredentials {
    fn load(&self, key: &str) -> Result<Option<String>, String> {
        self.0
            .load(&Self::name(key)?)
            .map_err(|error| error.to_string())
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.0
            .store(&Self::name(key)?, value)
            .map_err(|error| error.to_string())
    }

    fn delete(&self, key: &str) -> Result<bool, String> {
        self.0
            .delete(&Self::name(key)?)
            .map_err(|error| error.to_string())
    }
}

/// Whether a remote server takes an OAuth sign-in: an HTTP server with no `Authorization` header
/// of its own (a configured bearer token is one) and a name the keychain entries can carry.
pub fn takes_sign_in(name: &str, launch: &Launch) -> bool {
    match launch {
        Launch::Http { headers, .. } => {
            imperium_config::valid_server_name(name)
                && !headers
                    .iter()
                    .any(|(header, _)| header.eq_ignore_ascii_case("authorization"))
        }
        Launch::Stdio { .. } => false,
    }
}

/// The OAuth token sources for the servers a session starts, by server name. A server nobody
/// signed in to simply has no token to offer; starting a session never starts a sign-in.
pub fn oauth_sources(
    specs: &[ServerSpec],
    secrets: &Arc<dyn SecretStore>,
) -> HashMap<String, Arc<dyn TokenSource>> {
    let store: Arc<dyn CredentialStore> = Arc::new(KeychainCredentials(Arc::clone(secrets)));
    specs
        .iter()
        .filter(|spec| takes_sign_in(&spec.name, &spec.launch))
        .map(|spec| {
            let source: Arc<dyn TokenSource> =
                Arc::new(OAuthSource::new(&spec.name, Arc::clone(&store)));
            (spec.name.clone(), source)
        })
        .collect()
}

/// How to start one `[mcp.servers.<name>]` entry, with its secrets read; the reason when it
/// cannot be started.
pub(crate) fn configured(
    name: &str,
    server: &McpServerSettings,
    load_secret: &impl Fn(&SecretName) -> Result<Option<String>, SecretError>,
) -> Result<ServerSpec, String> {
    let secret = |name: &SecretName| {
        load_secret(name)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!("secret {name} is not in the keychain; run: imperium secret set {name}")
            })
    };
    let launch = match (&server.command, &server.url) {
        (Some(command), _) => {
            let mut env: Vec<(String, String)> = server
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            for (key, reference) in &server.secret_env {
                env.push((key.clone(), secret(&reference.secret)?));
            }
            Launch::Stdio {
                command: command.clone(),
                args: server.args.clone(),
                env,
                cwd: None,
            }
        }
        (None, Some(url)) => {
            let mut headers: Vec<(String, String)> = server
                .headers
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            if let Some(reference) = &server.bearer_token {
                let token = secret(&reference.secret)?;
                headers.push(("Authorization".into(), format!("Bearer {token}")));
            }
            Launch::Http {
                url: url.clone(),
                headers,
            }
        }
        (None, None) => return Err("set command or url".into()),
    };
    Ok(ServerSpec {
        name: name.to_string(),
        launch,
        allow: server.allow.clone(),
        timeout: server
            .timeout_s
            .map_or(DEFAULT_TIMEOUT, Duration::from_secs),
        sampling: server.sampling,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_config::{Config, Layer, Source};

    fn settings(text: &str) -> Settings {
        Config::from_layers([
            Layer::defaults(),
            Layer::new(Source::User("u".into()), text.parse().unwrap()),
        ])
        .settings()
        .unwrap()
    }

    #[test]
    fn configured_servers_resolve_secrets_and_untrusted_projects_are_reported() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(".mcp.json"),
            r#"{"mcpServers": {"proj": {"command": "server"}}}"#,
        )
        .unwrap();
        let config = r#"
            [mcp.servers.files]
            command = "files-server"
            args = ["--root", "."]
            allow = ["read"]
            secret_env = { FILES_TOKEN = { secret = "files" } }
            [mcp.servers.remote]
            url = "https://example.com/mcp"
            bearer_token = { secret = "remote" }
            [mcp.servers.off]
            command = "x"
            enabled = false
        "#;
        let (specs, notices) =
            session_servers(&settings(config), project.path(), |_| Ok(Some("t".into())));
        let names: Vec<_> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, ["files", "remote"]);
        assert!(matches!(&specs[0].launch, Launch::Stdio { env, .. }
            if env.contains(&("FILES_TOKEN".into(), "t".into()))));
        assert!(matches!(&specs[1].launch, Launch::Http { headers, .. }
            if headers.contains(&("Authorization".into(), "Bearer t".into()))));
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("trusted_projects"), "{}", notices[0]);

        let (_, notices) = session_servers(&settings(config), project.path(), |_| Ok(None));
        assert!(
            notices
                .iter()
                .any(|n| n.contains("imperium secret set remote"))
        );
    }

    #[test]
    fn trusted_projects_add_their_servers() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(".mcp.json"),
            r#"{"mcpServers": {"proj": {"command": "server"}}}"#,
        )
        .unwrap();
        let path = project.path().display().to_string().replace('\\', "/");
        let config = format!("[mcp]\ntrusted_projects = [\"{path}\"]");
        let (specs, notices) = session_servers(&settings(&config), project.path(), |_| Ok(None));
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(specs[0].name, "proj");
    }
}
