use serde::Deserialize;

/// Built-in defaults: the first configuration layer.
pub(crate) const DEFAULTS: &str = r#"
[daemon]
# Port for the local WebSocket listener; 0 picks a free port at each start.
port = 0
"#;

/// Typed view of the merged configuration. Keys this build does not know are ignored, so a
/// config written for a newer version still loads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Settings {
    pub daemon: DaemonSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DaemonSettings {
    pub port: u16,
}
