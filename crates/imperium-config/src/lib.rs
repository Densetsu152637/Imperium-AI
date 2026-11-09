//! Imperium's data locations, layered configuration, and keychain secrets.
//!
//! Configuration is assembled from layers, later ones overriding earlier ones key by key:
//! built-in defaults, the user's `config.toml`, the project's `.imperium/config.toml` and
//! `.imperium/config.local.toml`, then `IMPERIUM_<SECTION>__<KEY>` environment variables.
//! Every value remembers which layer set it, for `imperium config explain`.

mod layers;
mod mcp_servers;
mod paths;
mod registry;
mod secrets;
mod settings;

pub use layers::{Config, ConfigError, Explained, Layer, Source, load};
pub use mcp_servers::{
    MAX_SERVER_NAME, remove_mcp_server, save_mcp_server, set_mcp_server_enabled, valid_server_name,
};
pub use paths::{Paths, PathsError};
pub use registry::{
    SETTINGS, SetError, SettingDef, SettingKind, SettingState, describe, describe_key,
    set_user_setting, setting,
};
pub use secrets::{
    KeychainSecrets, MemorySecrets, SecretError, SecretName, SecretStore, delete_secret,
    load_secret, store_secret,
};
pub use settings::{
    ActionSettings, AgentSettings, AutofixSettings, CatalogEntry, DaemonSettings, EndpointSettings,
    Enforce, ExtensionSettings, GithubSettings, GithubWatch, HardwareSettings, HookSettings,
    ImageSettings, IsolationSettings, LLAMACPP_ENDPOINT, LlamaCppSettings, Location,
    McpServerSettings, McpSettings, MediaServiceSettings, MediaSettings, MemorySettings,
    MemoryWrite, ModelPlacement, ModelSettings, PetSettings, PhoneSettings, Preference,
    ProjectSettings, ResumeSettings, RoutingRule, RoutingSettings, RuntimeSettings,
    ScheduleSettings, SecretRef, SessionSettings, Settings, Tier, TriggerSettings, VLLM_ENDPOINT,
    VllmSettings, WebSettings, WorkflowSettings, WritePolicy,
};
pub use settings::{SuggestionCandidate, default_suggestions};
