//! Imperium's data locations, layered configuration, and keychain secrets.
//!
//! Configuration is assembled from layers, later ones overriding earlier ones key by key:
//! built-in defaults, the user's `config.toml`, the project's `.imperium/config.toml` and
//! `.imperium/config.local.toml`, then `IMPERIUM_<SECTION>__<KEY>` environment variables.
//! Every value remembers which layer set it, for `imperium config explain`.

mod layers;
mod local_models;
mod paths;
mod secrets;
mod settings;

pub use layers::{
    Config, ConfigError, Explained, Layer, Source, load, load_layers, replace_app_layer,
};
pub use local_models::{
    CACHE_TYPES, CPU_LANE_MAX_WAIT_S, CPU_LANE_SLOTS, DEFAULT_SLOTS, LaneRow, LocalModelSettings,
    MAX_SLOTS, ModelLaunch, TableOverride, check_launch, check_local_model,
};
pub use paths::{Paths, PathsError};
pub use secrets::{
    KeychainSecrets, MemorySecrets, SecretError, SecretName, SecretStore, delete_secret,
    load_secret, store_secret,
};
pub use settings::{
    ActionSettings, AgentSettings, AutofixSettings, CatalogEntry, ConditionSettings,
    DaemonSettings, EndpointSettings, Enforce, ExtensionSettings, GithubSettings, GithubWatch,
    HardwareSettings, HookSettings, ImageSettings, IsolationSettings, LLAMACPP_ENDPOINT,
    LlamaCppSettings, LoadSettings, Location, McpServerSettings, McpSettings, MediaServiceSettings,
    MediaSettings, MemorySettings, MemoryWrite, ModelPlacement, ModelSettings, PetSettings,
    PhoneSettings, Preference, ProjectSettings, ResumeSettings, RoutingRule, RoutingSettings,
    RuntimeSettings, ScheduleSettings, SecretRef, Settings, Tier, TriggerSettings, VLLM_ENDPOINT,
    VllmSettings, WebSettings, WorkflowSettings, WritePolicy,
};
