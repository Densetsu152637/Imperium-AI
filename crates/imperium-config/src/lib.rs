//! Imperium's data locations, layered configuration, and keychain secrets.
//!
//! Configuration is assembled from layers, later ones overriding earlier ones key by key:
//! built-in defaults, the user's `config.toml`, the project's `.imperium/config.toml` and
//! `.imperium/config.local.toml`, then `IMPERIUM_<SECTION>__<KEY>` environment variables.
//! Every value remembers which layer set it, for `imperium config explain`.

mod layers;
mod paths;
mod secrets;
mod settings;

pub use layers::{Config, ConfigError, Explained, Layer, Source, load};
pub use paths::{Paths, PathsError};
pub use secrets::{SecretError, SecretName, delete_secret, load_secret, store_secret};
pub use settings::{
    AgentSettings, CatalogEntry, DaemonSettings, EndpointSettings, ExtensionSettings,
    HardwareSettings, HookSettings, LLAMACPP_ENDPOINT, LlamaCppSettings, Location,
    McpServerSettings, McpSettings, MemorySettings, MemoryWrite, ModelSettings, Preference,
    RoutingRule, RoutingSettings, RuntimeSettings, SecretRef, Settings, Tier, WritePolicy,
};
