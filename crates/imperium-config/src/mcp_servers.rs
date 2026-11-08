//! Adding, changing, and removing `[mcp.servers.<name>]` entries in the user's `config.toml`,
//! for apps that manage MCP servers (connectors). Like [`crate::set_user_setting`], edits keep
//! the file's comments, formatting, and unknown keys, and an edit that leaves the configuration
//! invalid is undone. Secrets are written only as `{ secret = "<name>" }` references.

use std::collections::BTreeMap;

use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::registry::edit_user_file;
use crate::{McpServerSettings, Paths, SecretRef, SetError};

/// Longest server name, so secret names derived from it (`mcp.<name>.<field>`) stay within the
/// 64-character limit.
pub const MAX_SERVER_NAME: usize = 40;

/// Whether `name` may name a server: 1-40 lowercase letters, digits, `_` or `-`, starting with
/// a letter or digit. Tools are offered as `mcp__<name>__<tool>`.
pub fn valid_server_name(name: &str) -> bool {
    let allowed = |c: char| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-');
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= MAX_SERVER_NAME
        && name.chars().all(allowed)
}

fn check_name(name: &str) -> Result<(), SetError> {
    if valid_server_name(name) {
        Ok(())
    } else {
        Err(SetError::InvalidValue {
            key: format!("mcp.servers.{name}"),
            message: "a server name is 1-40 lowercase letters, digits, '_' or '-', starting with \
                      a letter or digit"
                .into(),
        })
    }
}

/// Writes `server` as `[mcp.servers.<name>]`, replacing the known keys of an existing entry
/// (unknown keys and comments stay) or adding a new table. Returns the entry as loaded back.
pub fn save_mcp_server(
    paths: &Paths,
    name: &str,
    server: &McpServerSettings,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<McpServerSettings, SetError> {
    check_name(name)?;
    if server.command.is_some() == server.url.is_some() {
        return Err(SetError::InvalidValue {
            key: format!("mcp.servers.{name}"),
            message: "set exactly one of command or url".into(),
        });
    }
    let config = edit_user_file(paths, env, |document| {
        let entry = server_table(document, name, true)?.expect("created");
        write_server(entry, server);
        Ok(())
    })?;
    loaded(&config, name)
}

/// Turns a server on or off with its `enabled` key.
pub fn set_mcp_server_enabled(
    paths: &Paths,
    name: &str,
    enabled: bool,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<McpServerSettings, SetError> {
    check_name(name)?;
    let config = edit_user_file(paths, env, |document| {
        let entry = server_table(document, name, false)?
            .ok_or_else(|| format!("no [mcp.servers.{name}] in the user config"))?;
        set_key(entry, "enabled", (!enabled).then(|| false.into()));
        Ok(())
    })?;
    loaded(&config, name)
}

/// Removes `[mcp.servers.<name>]`. Returns the removed entry's secret references, so the caller
/// may forget those secrets, or `None` when there was no such entry.
pub fn remove_mcp_server(
    paths: &Paths,
    name: &str,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<Option<Vec<SecretRef>>, SetError> {
    check_name(name)?;
    let before = crate::load(paths, None, Vec::<(String, String)>::new())
        .ok()
        .and_then(|config| config.settings().ok())
        .and_then(|settings| settings.mcp.servers.get(name).cloned());
    let mut found = false;
    edit_user_file(paths, env, |document| {
        let Some(servers) = document
            .get_mut("mcp")
            .and_then(Item::as_table_like_mut)
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(Item::as_table_like_mut)
        else {
            return Ok(());
        };
        found = servers.remove(name).is_some();
        Ok(())
    })?;
    Ok(found.then(|| {
        before
            .map(|server| {
                server
                    .secret_env
                    .into_values()
                    .chain(server.bearer_token)
                    .collect()
            })
            .unwrap_or_default()
    }))
}

fn loaded(config: &crate::Config, name: &str) -> Result<McpServerSettings, SetError> {
    let settings = config.settings().map_err(SetError::Rejected)?;
    settings
        .mcp
        .servers
        .get(name)
        .cloned()
        .ok_or_else(|| SetError::InvalidValue {
            key: format!("mcp.servers.{name}"),
            message: "an environment variable removes this server".into(),
        })
}

/// The `[mcp.servers.<name>]` table, created (with implicit parents) when `create` is set.
fn server_table<'a>(
    document: &'a mut DocumentMut,
    name: &str,
    create: bool,
) -> Result<Option<&'a mut dyn TableLike>, String> {
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for (part, implicit) in [("mcp", true), ("servers", true), (name, false)] {
        if table.get(part).is_none() {
            if !create {
                return Ok(None);
            }
            let mut child = Table::new();
            child.set_implicit(implicit);
            table.insert(part, Item::Table(child));
        }
        table = table
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("{part} in the user config is not a table"))?;
    }
    Ok(Some(table))
}

/// Writes every known key of `server` into `table`; empty and default values are removed.
fn write_server(table: &mut dyn TableLike, server: &McpServerSettings) {
    let text = |value: &Option<String>| value.as_deref().map(Value::from);
    let list = |items: &[String]| {
        (!items.is_empty()).then(|| Value::Array(items.iter().map(String::as_str).collect()))
    };
    let map = |items: &BTreeMap<String, String>| {
        (!items.is_empty()).then(|| {
            Value::InlineTable(InlineTable::from_iter(
                items
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            ))
        })
    };
    let secret = |reference: &SecretRef| {
        let mut table = InlineTable::new();
        table.insert("secret", reference.secret.as_str().into());
        Value::InlineTable(table)
    };
    set_key(table, "command", text(&server.command));
    set_key(table, "args", list(&server.args));
    set_key(table, "env", map(&server.env));
    set_key(
        table,
        "secret_env",
        (!server.secret_env.is_empty()).then(|| {
            Value::InlineTable(InlineTable::from_iter(
                server
                    .secret_env
                    .iter()
                    .map(|(key, reference)| (key.as_str(), secret(reference))),
            ))
        }),
    );
    set_key(table, "url", text(&server.url));
    set_key(table, "headers", map(&server.headers));
    set_key(
        table,
        "bearer_token",
        server.bearer_token.as_ref().map(secret),
    );
    set_key(table, "allow", list(&server.allow));
    set_key(table, "enabled", (!server.enabled).then(|| false.into()));
    set_key(
        table,
        "timeout_s",
        server
            .timeout_s
            .map(|seconds| Value::from(i64::try_from(seconds).unwrap_or(i64::MAX))),
    );
    set_key(table, "catalog", text(&server.catalog));
}

/// Sets or removes one key, keeping an existing value's comments and spacing.
fn set_key(table: &mut dyn TableLike, key: &str, value: Option<Value>) {
    match value {
        Some(mut value) => match table.get_mut(key).and_then(Item::as_value_mut) {
            Some(existing) => {
                *value.decor_mut() = existing.decor().clone();
                *existing = value;
            }
            None => {
                table.insert(key, Item::Value(value));
            }
        },
        None => {
            table.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretName;

    fn home() -> (tempfile::TempDir, Paths) {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        (home, paths)
    }

    fn secret(name: &str) -> SecretRef {
        SecretRef {
            secret: name.parse::<SecretName>().unwrap(),
        }
    }

    fn read(paths: &Paths) -> String {
        std::fs::read_to_string(paths.user_config_file()).unwrap()
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_server_name("github"));
        assert!(valid_server_name("my-server_2"));
        for bad in ["", "GitHub", "-x", "a b", "a.b", &"x".repeat(41)] {
            assert!(!valid_server_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn saving_adds_a_server_with_secret_references_and_keeps_comments() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        let original = "# my settings\n[daemon]\nport = 7000 # fixed\n\n[mcp.servers.old]\n# keep me\ncommand = \"old-server\"\nfuture_key = 1\n";
        std::fs::write(paths.user_config_file(), original).unwrap();

        let server = McpServerSettings {
            command: Some("npx".into()),
            args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            env: BTreeMap::from([("GITHUB_HOST".into(), "github.com".into())]),
            secret_env: BTreeMap::from([("GITHUB_TOKEN".into(), secret("mcp.github.token"))]),
            catalog: Some("github".into()),
            ..McpServerSettings::default()
        };
        let saved = save_mcp_server(&paths, "github", &server, []).unwrap();
        assert_eq!(saved, server);
        let text = read(&paths);
        assert!(
            text.starts_with("# my settings\n[daemon]\nport = 7000 # fixed\n"),
            "{text}"
        );
        assert!(
            text.contains("# keep me\ncommand = \"old-server\"\nfuture_key = 1"),
            "{text}"
        );
        assert!(text.contains("[mcp.servers.github]"), "{text}");
        assert!(
            text.contains("secret_env = { GITHUB_TOKEN = { secret = \"mcp.github.token\" } }"),
            "{text}"
        );
        assert!(!text.contains("enabled"), "defaults are left out: {text}");

        // Updating an existing entry replaces known keys and keeps comments and unknown keys.
        let changed = McpServerSettings {
            command: Some("new-server".into()),
            allow: vec!["*".into()],
            timeout_s: Some(30),
            ..McpServerSettings::default()
        };
        save_mcp_server(&paths, "old", &changed, []).unwrap();
        let text = read(&paths);
        assert!(
            text.contains("# keep me\ncommand = \"new-server\""),
            "{text}"
        );
        assert!(text.contains("future_key = 1"), "{text}");
        assert!(text.contains("allow = [\"*\"]"), "{text}");
        assert!(text.contains("timeout_s = 30"), "{text}");
    }

    #[test]
    fn http_servers_take_a_bearer_secret() {
        let (_home, paths) = home();
        let server = McpServerSettings {
            url: Some("https://api.example.com/mcp".into()),
            headers: BTreeMap::from([("X-Team".into(), "a".into())]),
            bearer_token: Some(secret("mcp.remote.token")),
            ..McpServerSettings::default()
        };
        save_mcp_server(&paths, "remote", &server, []).unwrap();
        let text = read(&paths);
        assert!(
            text.contains("bearer_token = { secret = \"mcp.remote.token\" }"),
            "{text}"
        );
        assert!(text.starts_with("[mcp.servers.remote]\n"), "{text}");
    }

    #[test]
    fn invalid_servers_are_refused_and_the_file_is_unchanged() {
        let (_home, paths) = home();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), "[daemon]\nport = 7000\n").unwrap();
        let both = McpServerSettings {
            command: Some("x".into()),
            url: Some("https://x".into()),
            ..McpServerSettings::default()
        };
        assert!(save_mcp_server(&paths, "x", &both, []).is_err());
        let neither = McpServerSettings::default();
        assert!(save_mcp_server(&paths, "x", &neither, []).is_err());
        let fine = McpServerSettings {
            command: Some("x".into()),
            ..McpServerSettings::default()
        };
        assert!(save_mcp_server(&paths, "Bad Name", &fine, []).is_err());
        assert_eq!(read(&paths), "[daemon]\nport = 7000\n");
    }

    #[test]
    fn servers_are_disabled_enabled_and_removed() {
        let (_home, paths) = home();
        let server = McpServerSettings {
            command: Some("x".into()),
            secret_env: BTreeMap::from([("TOKEN".into(), secret("mcp.x.token"))]),
            ..McpServerSettings::default()
        };
        save_mcp_server(&paths, "x", &server, []).unwrap();
        assert!(
            !set_mcp_server_enabled(&paths, "x", false, [])
                .unwrap()
                .enabled
        );
        assert!(read(&paths).contains("enabled = false"));
        assert!(
            set_mcp_server_enabled(&paths, "x", true, [])
                .unwrap()
                .enabled
        );
        assert!(!read(&paths).contains("enabled"));
        assert!(set_mcp_server_enabled(&paths, "missing", true, []).is_err());

        let secrets = remove_mcp_server(&paths, "x", []).unwrap().unwrap();
        assert_eq!(secrets, [secret("mcp.x.token")]);
        assert!(
            !read(&paths).contains("[mcp.servers.x]"),
            "{}",
            read(&paths)
        );
        assert_eq!(remove_mcp_server(&paths, "x", []).unwrap(), None);
    }
}
