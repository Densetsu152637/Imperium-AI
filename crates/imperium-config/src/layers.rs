use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::Paths;
use crate::settings::{DEFAULTS, Settings};

const ENV_PREFIX: &str = "IMPERIUM_";
const ENV_SEPARATOR: &str = "__";

/// Which layer set a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    User(PathBuf),
    Project(PathBuf),
    Environment(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Default => write!(f, "built-in default"),
            Source::User(path) => write!(f, "user config {}", path.display()),
            Source::Project(path) => write!(f, "project config {}", path.display()),
            Source::Environment(var) => write!(f, "environment variable {var}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{origin} is not valid TOML: {source}")]
    Parse {
        origin: Source,
        source: Box<toml::de::Error>,
    },
    #[error("invalid configuration: {0}")]
    Invalid(Box<toml::de::Error>),
}

/// One configuration layer and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    source: Source,
    table: Table,
}

impl Layer {
    pub fn new(source: Source, table: Table) -> Self {
        Layer { source, table }
    }

    pub fn defaults() -> Self {
        let table = DEFAULTS
            .parse()
            .expect("built-in defaults are valid TOML (covered by tests)");
        Layer::new(Source::Default, table)
    }

    /// Reads a TOML file as a layer. A missing file is not an error: it yields `None`.
    pub fn from_file(path: &Path, source: Source) -> Result<Option<Self>, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let table = text.parse().map_err(|error| ConfigError::Parse {
            origin: source.clone(),
            source: Box::new(error),
        })?;
        Ok(Some(Layer::new(source, table)))
    }

    /// Turns `IMPERIUM_<SECTION>__<KEY>=value` variables into one layer each, in name order.
    /// Values are read as TOML (`8080`, `true`, `["a"]`) and fall back to plain strings.
    /// Variables without the `__` separator, such as `IMPERIUM_HOME`, are not configuration.
    pub fn from_env(vars: impl IntoIterator<Item = (String, String)>) -> Vec<Self> {
        let mut vars: Vec<_> = vars
            .into_iter()
            .filter_map(|(name, raw)| {
                let path = name.strip_prefix(ENV_PREFIX)?;
                if !path.contains(ENV_SEPARATOR) {
                    return None;
                }
                let keys: Vec<String> = path
                    .split(ENV_SEPARATOR)
                    .map(str::to_ascii_lowercase)
                    .collect();
                Some((name, keys, raw))
            })
            .collect();
        vars.sort_by(|a, b| a.0.cmp(&b.0));
        vars.into_iter()
            .map(|(name, keys, raw)| {
                Layer::new(
                    Source::Environment(name),
                    nested_table(&keys, parse_env_value(&raw)),
                )
            })
            .collect()
    }
}

fn parse_env_value(raw: &str) -> Value {
    format!("value = {raw}")
        .parse::<Table>()
        .ok()
        .and_then(|mut table| table.remove("value"))
        .unwrap_or_else(|| Value::String(raw.to_string()))
}

fn nested_table(keys: &[String], leaf: Value) -> Table {
    let (last, parents) = keys.split_last().expect("split on a separator yields keys");
    let mut table = Table::from_iter([(last.clone(), leaf)]);
    for key in parents.iter().rev() {
        table = Table::from_iter([(key.clone(), Value::Table(table))]);
    }
    table
}

/// The merged configuration with the origin of every leaf value.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    merged: Table,
    origins: BTreeMap<String, Source>,
}

/// A value and, for a leaf, the layer that set it. Tables mix origins and have none.
#[derive(Debug, Clone, PartialEq)]
pub struct Explained<'a> {
    pub value: &'a Value,
    pub source: Option<&'a Source>,
}

impl Config {
    /// Merges layers in order: later layers override earlier ones key by key; arrays and
    /// other non-table values are replaced whole.
    pub fn from_layers(layers: impl IntoIterator<Item = Layer>) -> Self {
        let mut config = Config {
            merged: Table::new(),
            origins: BTreeMap::new(),
        };
        for layer in layers {
            merge_into(
                &mut config.merged,
                layer.table,
                "",
                &layer.source,
                &mut config.origins,
            );
        }
        config
    }

    /// Looks up a dotted key such as `daemon.port`.
    pub fn explain(&self, key: &str) -> Option<Explained<'_>> {
        let mut parts = key.split('.');
        let mut value = self.merged.get(parts.next()?)?;
        for part in parts {
            value = value.as_table()?.get(part)?;
        }
        Some(Explained {
            value,
            source: self.origins.get(key),
        })
    }

    pub fn settings(&self) -> Result<Settings, ConfigError> {
        Value::Table(self.merged.clone())
            .try_into()
            .map_err(|error| ConfigError::Invalid(Box::new(error)))
    }
}

fn merge_into(
    target: &mut Table,
    layer: Table,
    prefix: &str,
    source: &Source,
    origins: &mut BTreeMap<String, Source>,
) {
    for (key, incoming) in layer {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (target.get_mut(&key), incoming) {
            (Some(Value::Table(existing)), Value::Table(incoming)) => {
                merge_into(existing, incoming, &path, source, origins);
            }
            (_, incoming) => {
                origins.retain(|existing, _| !is_at_or_below(existing, &path));
                record_leaves(&incoming, &path, source, origins);
                target.insert(key, incoming);
            }
        }
    }
}

fn is_at_or_below(key: &str, path: &str) -> bool {
    key == path
        || key
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('.'))
}

fn record_leaves(
    value: &Value,
    path: &str,
    source: &Source,
    origins: &mut BTreeMap<String, Source>,
) {
    match value {
        Value::Table(table) => {
            for (key, child) in table {
                record_leaves(child, &format!("{path}.{key}"), source, origins);
            }
        }
        _ => {
            origins.insert(path.to_string(), source.clone());
        }
    }
}

/// Loads every layer for a user and, optionally, a project directory.
pub fn load(
    paths: &Paths,
    project: Option<&Path>,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<Config, ConfigError> {
    let mut layers = vec![Layer::defaults()];
    let user = paths.user_config_file();
    layers.extend(Layer::from_file(&user, Source::User(user.clone()))?);
    if let Some(project) = project {
        for name in ["config.toml", "config.local.toml"] {
            let file = project.join(".imperium").join(name);
            layers.extend(Layer::from_file(&file, Source::Project(file.clone()))?);
        }
    }
    layers.extend(Layer::from_env(env));
    Ok(Config::from_layers(layers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(source: Source, text: &str) -> Layer {
        Layer::new(source, text.parse().unwrap())
    }

    #[test]
    fn later_layers_override_earlier_ones_key_by_key() {
        let user = Source::User("u.toml".into());
        let config = Config::from_layers([
            layer(Source::Default, "[daemon]\nport = 0\nhost = 'local'"),
            layer(user.clone(), "[daemon]\nport = 9000"),
        ]);

        let port = config.explain("daemon.port").unwrap();
        assert_eq!(port.value, &Value::Integer(9000));
        assert_eq!(port.source, Some(&user));

        let host = config.explain("daemon.host").unwrap();
        assert_eq!(host.source, Some(&Source::Default));

        let section = config.explain("daemon").unwrap();
        assert!(section.value.is_table());
        assert_eq!(section.source, None);
        assert!(config.explain("daemon.missing").is_none());
    }

    #[test]
    fn replacing_a_table_with_a_value_drops_the_old_leaf_origins() {
        let config = Config::from_layers([
            layer(Source::Default, "[ui]\nscale = 1.0"),
            layer(Source::User("u".into()), "ui = 'compact'"),
        ]);
        assert!(config.explain("ui.scale").is_none());
        assert_eq!(
            config.explain("ui").unwrap().source,
            Some(&Source::User("u".into()))
        );
    }

    #[test]
    fn environment_variables_become_typed_nested_values() {
        let layers = Layer::from_env([
            ("IMPERIUM_DAEMON__PORT".to_string(), "8080".to_string()),
            ("IMPERIUM_UI__THEME".to_string(), "dark mode".to_string()),
            ("IMPERIUM_HOME".to_string(), "/somewhere".to_string()),
            ("PATH".to_string(), "/bin".to_string()),
        ]);
        let config = Config::from_layers(layers);

        let port = config.explain("daemon.port").unwrap();
        assert_eq!(port.value, &Value::Integer(8080));
        assert_eq!(
            port.source,
            Some(&Source::Environment("IMPERIUM_DAEMON__PORT".into()))
        );
        assert_eq!(
            config.explain("ui.theme").unwrap().value,
            &Value::String("dark mode".into())
        );
        assert!(config.explain("home").is_none());
    }

    #[test]
    fn defaults_parse_into_settings() {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        assert_eq!(settings.daemon.port, 0);
    }

    #[test]
    fn settings_report_values_of_the_wrong_type() {
        let config = Config::from_layers([
            Layer::defaults(),
            layer(Source::User("u".into()), "[daemon]\nport = 'high'"),
        ]);
        assert!(matches!(config.settings(), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn load_reads_user_and_project_files_and_skips_missing_ones() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), "[daemon]\nport = 7000").unwrap();

        let project = tempfile::tempdir().unwrap();
        let project_dir = project.path().join(".imperium");
        std::fs::create_dir_all(&project_dir).unwrap();
        let local = project_dir.join("config.local.toml");
        std::fs::write(&local, "[daemon]\nport = 7100").unwrap();

        let user_only = load(&paths, None, []).unwrap();
        assert_eq!(user_only.settings().unwrap().daemon.port, 7000);

        let with_project = load(&paths, Some(project.path()), []).unwrap();
        let port = with_project.explain("daemon.port").unwrap();
        assert_eq!(port.value, &Value::Integer(7100));
        assert_eq!(port.source, Some(&Source::Project(local)));
    }

    #[test]
    fn invalid_toml_names_the_file() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::under(home.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.user_config_file(), "[daemon\nport = 1").unwrap();

        let error = load(&paths, None, []).unwrap_err().to_string();
        assert!(error.contains("config.toml"), "{error}");
    }
}
