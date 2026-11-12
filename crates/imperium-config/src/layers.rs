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
    /// The file the desktop app writes model settings to.
    App(PathBuf),
    Project(PathBuf),
    Environment(String),
}

impl Source {
    /// The layer's kind without its location: `default`, `user`, `app`, `project`, or `env`.
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::User(_) => "user",
            Source::App(_) => "app",
            Source::Project(_) => "project",
            Source::Environment(_) => "env",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Default => write!(f, "built-in default"),
            Source::User(path) => write!(f, "user config {}", path.display()),
            Source::App(path) => write!(f, "app settings {}", path.display()),
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
    #[error("invalid configuration: {0}")]
    Inconsistent(String),
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

    pub fn source(&self) -> &Source {
        &self.source
    }

    pub fn table(&self) -> &Table {
        &self.table
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
    warnings: Vec<String>,
    /// The layers merged, in order, so a caller can rebuild with one of them replaced.
    layers: Vec<Layer>,
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
        let layers: Vec<Layer> = layers.into_iter().collect();
        let mut config = Config {
            merged: Table::new(),
            origins: BTreeMap::new(),
            warnings: Vec::new(),
            layers: layers.clone(),
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

    /// Parts of the app's settings file that were left out because they were invalid.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The layers this configuration merged, invalid app entries already left out. With
    /// [`replace_app_layer`], a caller rebuilds these settings with another app file without
    /// reading the other files again.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn settings(&self) -> Result<Settings, ConfigError> {
        let mut settings: Settings = Value::Table(self.merged.clone())
            .try_into()
            .map_err(|error| ConfigError::Invalid(Box::new(error)))?;
        settings.apply_overrides();
        settings.validate().map_err(ConfigError::Inconsistent)?;
        Ok(settings)
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

/// The sections of the app layer that hold one entry per model.
const APP_SECTIONS: [&[&str]; 2] = [&["runtime", "llamacpp", "models"], &["models", "overrides"]];

/// Reads every layer for a user in order. A project directory's `.imperium` files are not
/// layers: they are reported in `warnings` and applied only through [`crate::EnvironmentConfig`].
/// An app settings file that is not valid TOML is skipped with a warning.
fn read_layers(
    paths: &Paths,
    project: Option<&Path>,
    env: impl IntoIterator<Item = (String, String)>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Layer>, ConfigError> {
    let mut layers = vec![Layer::defaults()];
    let user = paths.user_config_file();
    layers.extend(Layer::from_file(&user, Source::User(user.clone()))?);
    // A project's files belong to its repository, so they are never merged as settings: they
    // could name workflow packs or servers to trust, or loosen anything. They can only narrow
    // ([`crate::EnvironmentConfig`]), which is checked against the settings built here.
    if let Some(project) = project {
        for name in crate::ENVIRONMENT_CONFIG_FILES {
            let file = project.join(".imperium").join(name);
            if file.exists() {
                warnings.push(format!(
                    "{} is not merged into the settings: a project's file can only narrow your \
                     settings (see imperium env config)",
                    file.display()
                ));
            }
        }
    }
    let app = paths.app_models_file();
    match Layer::from_file(&app, Source::App(app.clone())) {
        Ok(layer) => layers.extend(layer),
        Err(error @ ConfigError::Parse { .. }) => {
            warnings.push(format!("ignored the app settings file: {error}"));
        }
        Err(error) => return Err(error),
    }
    layers.extend(Layer::from_env(env));
    Ok(layers)
}

/// Loads every layer for a user and, optionally, a project directory. Entries of the app
/// settings file that do not validate are dropped and listed in [`Config::warnings`].
pub fn load(
    paths: &Paths,
    project: Option<&Path>,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<Config, ConfigError> {
    let mut warnings = Vec::new();
    let layers = read_layers(paths, project, env, &mut warnings)?;
    let layers = drop_invalid_app_entries(layers, &mut warnings);
    let mut config = Config::from_layers(layers);
    config.warnings = warnings;
    Ok(config)
}

/// The layers [`load`] would merge, with the app layer exactly as written, so a caller can
/// validate a candidate app file strictly with [`replace_app_layer`].
pub fn load_layers(
    paths: &Paths,
    project: Option<&Path>,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<Vec<Layer>, ConfigError> {
    read_layers(paths, project, env, &mut Vec::new())
}

/// `layers` with the app layer replaced by `table` (added before the environment layers when
/// there is none).
pub fn replace_app_layer(mut layers: Vec<Layer>, file: PathBuf, table: Table) -> Vec<Layer> {
    let layer = Layer::new(Source::App(file), table);
    match layers
        .iter()
        .position(|layer| matches!(layer.source, Source::App(_)))
    {
        Some(index) => layers[index] = layer,
        None => {
            let at = layers
                .iter()
                .position(|layer| matches!(layer.source, Source::Environment(_)))
                .unwrap_or(layers.len());
            layers.insert(at, layer);
        }
    }
    layers
}

fn section<'a>(table: &'a Table, path: &[&str]) -> Option<&'a Table> {
    let mut table = table;
    for key in path {
        table = table.get(*key)?.as_table()?;
    }
    Some(table)
}

fn section_mut<'a>(table: &'a mut Table, path: &[&str]) -> Option<&'a mut Table> {
    let mut table = table;
    for key in path {
        table = table.get_mut(*key)?.as_table_mut()?;
    }
    Some(table)
}

/// A table holding only `id = value` under `path`.
fn single_entry(path: &[&str], id: &str, value: Value) -> Table {
    let mut table = Table::from_iter([(id.to_string(), value)]);
    for key in path.iter().rev() {
        table = Table::from_iter([((*key).to_string(), Value::Table(table))]);
    }
    table
}

/// Removes the entries of the app layer that fail validation on their own against the other
/// layers, so one bad entry never stops the daemon or discards the good ones.
fn drop_invalid_app_entries(layers: Vec<Layer>, warnings: &mut Vec<String>) -> Vec<Layer> {
    let Some(index) = layers
        .iter()
        .position(|layer| matches!(layer.source, Source::App(_)))
    else {
        return layers;
    };
    let mut baseline = layers.clone();
    let app = baseline.remove(index);
    // When the rest is invalid, the error belongs to the caller, not to the app's entries.
    if Config::from_layers(baseline.clone()).settings().is_err() {
        return layers;
    }
    let mut kept = app.table.clone();
    for path in APP_SECTIONS {
        let Some(entries) = section(&app.table, path) else {
            continue;
        };
        for (id, value) in entries {
            let mut test = baseline.clone();
            test.insert(
                index,
                Layer::new(app.source.clone(), single_entry(path, id, value.clone())),
            );
            if let Err(error) = Config::from_layers(test).settings() {
                warnings.push(format!(
                    "{}: ignored {}.{id:?}: {error}",
                    app.source,
                    path.join(".")
                ));
                if let Some(entries) = section_mut(&mut kept, path) {
                    entries.remove(id);
                }
            }
        }
    }
    let mut layers = layers;
    layers[index] = Layer::new(app.source, kept);
    layers
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
        assert!(settings.models.suggest_downloads);
        assert!(!settings.models.suggestions.is_empty());
    }

    #[test]
    fn model_suggestions_can_be_turned_off_and_replaced() {
        let config = Config::from_layers([
            Layer::defaults(),
            layer(
                Source::User("u".into()),
                r#"
                [models]
                suggest_downloads = false
                [[models.suggestions]]
                repo = "acme/tiny-GGUF"
                capabilities = ["chat"]
                context = 4096
                "#,
            ),
        ]);
        let models = config.settings().unwrap().models;
        assert!(!models.suggest_downloads);
        assert_eq!(models.suggestions.len(), 1);
        assert_eq!(models.suggestions[0].size, "small");
        assert!(!models.suggestions[0].mmproj);
    }

    #[test]
    fn endpoints_parse_with_secret_references_and_pricing() {
        let config = Config::from_layers([
            Layer::defaults(),
            layer(
                Source::User("u".into()),
                r#"
                [models]
                default = "openrouter:qwen/qwen3-coder"

                [endpoints.openrouter]
                base_url = "https://openrouter.ai/api/v1"
                api_key = { secret = "openrouter" }
                pricing."qwen/qwen3-coder" = { input_usd_per_mtok = 0.2, output_usd_per_mtok = 0.8 }
                "#,
            ),
        ]);
        let settings = config.settings().unwrap();
        let endpoint = &settings.endpoints["openrouter"];
        assert_eq!(
            endpoint.api_key.as_ref().unwrap().secret.as_str(),
            "openrouter"
        );
        assert_eq!(
            endpoint.pricing["qwen/qwen3-coder"].output_usd_per_mtok,
            0.8
        );
        assert_eq!(endpoint.kind, "openai");
        assert_eq!(settings.agents.max_steps, 50);

        let bad_secret = Config::from_layers([
            Layer::defaults(),
            layer(
                Source::User("u".into()),
                "[endpoints.x]
base_url = 'u'
api_key = { secret = 'Bad Name' }",
            ),
        ]);
        assert!(bad_secret.settings().is_err());
    }

    #[test]
    fn the_model_table_needs_unique_ids_and_known_rule_targets() {
        let table = r#"
            [[models.catalog]]
            id = "local-coder"
            spec = "llama:qwen"
            location = "local"
            tier = "standard"
            tags = ["code"]

            [[models.catalog]]
            id = "cloud-big"
            spec = "openrouter:big"
            location = "cloud"
            tier = "strong"

            [routing]
            prefer = "local"

            [[routing.rules]]
            when_tag = "code"
            use = ["local-coder"]
        "#;
        let settings =
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), table)])
                .settings()
                .unwrap();
        assert_eq!(settings.models.catalog[0].location, crate::Location::Local);
        assert_eq!(settings.routing.rules[0].use_models, ["local-coder"]);

        let duplicate = format!(
            "{table}\n[[models.catalog]]\nid = \"cloud-big\"\nspec = \"x:y\"\nlocation = \"cloud\"\ntier = \"basic\""
        );
        let error = Config::from_layers([
            Layer::defaults(),
            layer(Source::User("u".into()), &duplicate),
        ])
        .settings()
        .unwrap_err();
        assert!(error.to_string().contains("used twice"), "{error}");

        let unknown = format!("{table}\n[[routing.rules]]\nuse = [\"missing\"]");
        let error =
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), &unknown)])
                .settings()
                .unwrap_err();
        assert!(error.to_string().contains("missing"), "{error}");

        let cloud_llama = "[[models.catalog]]\nid = \"q\"\nspec = \"llamacpp:q\"\nlocation = \"cloud\"\ntier = \"basic\"";
        let error = Config::from_layers([
            Layer::defaults(),
            layer(Source::User("u".into()), cloud_llama),
        ])
        .settings()
        .unwrap_err();
        assert!(error.to_string().contains("must be local"), "{error}");
        let reserved = "[endpoints.llamacpp]\nbase_url = \"http://x/v1\"";
        let error =
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), reserved)])
                .settings()
                .unwrap_err();
        assert!(error.to_string().contains("reserved"), "{error}");
    }

    #[test]
    fn cpu_lane_placement_needs_a_local_llama_cpp_row() {
        let load = |table: &str| {
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), table)])
                .settings()
        };
        let row = |spec: &str, location: &str, extra: &str| {
            format!(
                "[[models.catalog]]
id = \"critic\"
spec = \"{spec}\"
location = \"{location}\"
                 tier = \"strong\"
{extra}"
            )
        };
        let settings = load(&row(
            "llamacpp:qwen3-30b",
            "local",
            "placement = \"cpu\"
parallel = 2",
        ))
        .unwrap();
        let entry = &settings.models.catalog[0];
        assert_eq!(entry.placement, crate::ModelPlacement::Cpu);
        assert_eq!(
            (entry.parallel, entry.llamacpp_model()),
            (Some(2), Some("qwen3-30b"))
        );
        // Rows without a placement are planned by the memory planner.
        let plain = load(&row("llamacpp:qwen3-30b", "local", "")).unwrap();
        assert_eq!(
            plain.models.catalog[0].placement,
            crate::ModelPlacement::Auto
        );

        let cloud = load(&row("openrouter:big", "cloud", "placement = \"cpu\"")).unwrap_err();
        assert!(cloud.to_string().contains("needs location"), "{cloud}");
        let hosted_local = load(&row("llama:qwen", "local", "placement = \"cpu\"")).unwrap_err();
        assert!(
            hosted_local.to_string().contains("needs a llama.cpp model"),
            "{hosted_local}"
        );
        let vllm = load(&row("vllm:Qwen3-8B", "local", "placement = \"cpu\"")).unwrap_err();
        assert!(
            vllm.to_string().contains("needs a llama.cpp model"),
            "{vllm}"
        );
        for key in ["parallel = 2", "context = 4096", "max_wait_s = 60"] {
            let stray = load(&row("llamacpp:q", "local", key)).unwrap_err();
            assert!(stray.to_string().contains("only applies"), "{key}: {stray}");
        }
        for key in ["context", "max_wait_s"] {
            let extra = format!("placement = \"cpu\"\n{key} = 0");
            let zero = load(&row("llamacpp:q", "local", &extra)).unwrap_err();
            assert!(zero.to_string().contains("at least 1"), "{key}: {zero}");
        }
        let lane = load(&row(
            "llamacpp:q",
            "local",
            "placement = \"cpu\"\ncontext = 32768\nmax_wait_s = 7200",
        ))
        .unwrap();
        let entry = &lane.models.catalog[0];
        assert_eq!((entry.context, entry.max_wait_s), (Some(32768), Some(7200)));
        let zero = load(&row(
            "llamacpp:q",
            "local",
            "placement = \"cpu\"
parallel = 0",
        ))
        .unwrap_err();
        assert!(zero.to_string().contains("at least 1"), "{zero}");
        let unknown = load(&row("llamacpp:q", "local", "placement = \"gpu\"")).unwrap_err();
        assert!(
            unknown.to_string().contains("placement") || unknown.to_string().contains("variant"),
            "{unknown}"
        );
        let split = format!(
            "{}
[[models.catalog]]
id = \"other\"
spec = \"llamacpp:q\"
location = \"local\"
tier = \"basic\"",
            row("llamacpp:q", "local", "placement = \"cpu\"")
        );
        let error = load(&split).unwrap_err();
        assert!(error.to_string().contains("same model"), "{error}");
        // Two lane rows for one model serve it once, so they must agree on how.
        let twin = |extra: &str| {
            format!(
                "{}
[[models.catalog]]
id = \"twin\"
spec = \"llamacpp:q\"
location = \"local\"
tier = \"basic\"
placement = \"cpu\"
{extra}",
                row(
                    "llamacpp:q",
                    "local",
                    "placement = \"cpu\"
parallel = 2"
                )
            )
        };
        for extra in [
            "parallel = 3",
            "",
            "parallel = 2
context = 4096",
            "parallel = 2
max_wait_s = 9",
        ] {
            let error = load(&twin(extra)).unwrap_err();
            assert!(
                error.to_string().contains("different parallel"),
                "{extra}: {error}"
            );
        }
        assert!(load(&twin("parallel = 2")).is_ok());
    }

    #[test]
    fn routing_reads_the_builtin_switch_and_rule_tiers() {
        let defaults = Config::from_layers([Layer::defaults()]).settings().unwrap();
        assert!(defaults.routing.builtin_rules);
        let table = r#"
            [routing]
            builtin_rules = false

            [[routing.rules]]
            when_tag = "proof"
            tier = "frontier"
        "#;
        let settings =
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), table)])
                .settings()
                .unwrap();
        assert!(!settings.routing.builtin_rules);
        assert_eq!(settings.routing.rules[0].tier, Some(crate::Tier::Frontier));
        assert!(settings.routing.rules[0].use_models.is_empty());
    }

    #[test]
    fn local_model_settings_have_defaults() {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let llamacpp = &settings.runtime.llamacpp;
        assert_eq!(llamacpp.backend, "auto");
        assert_eq!(
            (llamacpp.context, llamacpp.parallel),
            (crate::ContextLimit::Auto, 2)
        );
        assert!(settings.models.folders.is_empty());
        assert_eq!(settings.hardware.vram_mb, None);
    }

    #[test]
    fn the_swap_gain_must_be_a_number_of_at_least_one() {
        let with_gain = |gain: &str| {
            let toml = format!("[runtime.llamacpp]\nswap_gain = {gain}");
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), &toml)])
                .settings()
        };
        assert_eq!(with_gain("1.0").unwrap().runtime.llamacpp.swap_gain, 1.0);
        for bad in ["0.5", "nan", "inf"] {
            let error = with_gain(bad).unwrap_err();
            assert!(error.to_string().contains("swap_gain"), "{bad}: {error}");
        }
    }

    #[test]
    fn downloads_go_to_the_configured_folder_or_the_first_model_folder() {
        let data = std::path::Path::new("/data");
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        assert_eq!(
            settings.models.download_dir(data),
            data.join("models"),
            "with no folders, downloads go under the data folder"
        );
        let absolute = if cfg!(windows) {
            "D:/Models"
        } else {
            "/models"
        };
        let folders = format!(
            "[models]
folders = [{absolute:?}, \"{absolute}/more\"]"
        );
        let settings =
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), &folders)])
                .settings()
                .unwrap();
        assert_eq!(
            settings.models.download_dir(data),
            std::path::Path::new(absolute)
        );
        let explicit = format!(
            "{folders}
download_dir = \"{absolute}/hf\""
        );
        let settings = Config::from_layers([
            Layer::defaults(),
            layer(Source::User("u".into()), &explicit),
        ])
        .settings()
        .unwrap();
        assert_eq!(
            settings.models.download_dir(data),
            std::path::Path::new(&format!("{absolute}/hf"))
        );
        let relative = "[models]
download_dir = \"models\"";
        let error =
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), relative)])
                .settings()
                .unwrap_err();
        assert!(error.to_string().contains("absolute"), "{error}");
    }

    #[test]
    fn schedules_and_projects_attach_to_a_folder_or_an_environment() {
        let settings = |text: &str| {
            Config::from_layers([Layer::defaults(), layer(Source::User("u".into()), text)])
                .settings()
        };
        let schedule = |action: &str| {
            format!(
                "[[schedules]]\nid = \"s\"\ntrigger = {{ every = \"1h\" }}\naction = {{ {action} }}"
            )
        };
        let ok = settings(&schedule("environment = \"Work\", prompt = \"Look.\"")).unwrap();
        let action = &ok.schedules[0].action;
        assert_eq!(action.environment.as_deref(), Some("Work"));
        assert!(action.project.as_os_str().is_empty());
        let told = settings(&schedule(
            "environment = \"Work\", agent = \"hi\", to_agent = \"Helper\"",
        ))
        .unwrap();
        assert_eq!(told.schedules[0].action.to_agent.as_deref(), Some("Helper"));
        // A folder still works as before.
        assert!(settings(&schedule("project = \"/p\", command = \"x\"")).is_ok());

        for (action, needle) in [
            ("prompt = \"Look.\"", "action.project or action.environment"),
            (
                "project = \"/p\", environment = \"Work\", prompt = \"Look.\"",
                "action.project or action.environment",
            ),
            (
                "project = \"/p\", agent = \"hi\", to_agent = \"Helper\"",
                "to_agent needs",
            ),
            (
                "environment = \"Work\", prompt = \"Look.\", to_agent = \"Helper\"",
                "to_agent needs",
            ),
        ] {
            let error = settings(&schedule(action)).unwrap_err().to_string();
            assert!(error.contains(needle), "{action}: {error}");
        }

        let project = |entry: &str| settings(&format!("[[projects]]\n{entry}"));
        let by_environment = project("environment = \"Work\"\nagent = true").unwrap();
        assert_eq!(
            by_environment.projects[0].environment.as_deref(),
            Some("Work")
        );
        assert!(project("path = \"/p\"\nagent = true").is_ok());
        for entry in ["agent = true", "path = \"/p\"\nenvironment = \"Work\""] {
            let error = project(entry).unwrap_err().to_string();
            assert!(error.contains("path or environment"), "{entry}: {error}");
        }
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

        // The project's file is not a layer: it cannot set anything, and is reported.
        let with_project = load(&paths, Some(project.path()), []).unwrap();
        let port = with_project.explain("daemon.port").unwrap();
        assert_eq!(port.value, &Value::Integer(7000));
        assert_eq!(port.source, Some(&Source::User(paths.user_config_file())));
        assert_eq!(with_project.warnings().len(), 1);
        assert!(
            with_project.warnings()[0].contains("config.local.toml"),
            "{:?}",
            with_project.warnings()
        );
        assert!(local.exists());
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
