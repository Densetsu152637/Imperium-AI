//! Imperium's data locations, layered configuration, and keychain secrets.
//!
//! Configuration is assembled from layers, later ones overriding earlier ones key by key:
//! built-in defaults, the user's `config.toml`, the project's `.imperium/config.toml` and
//! `.imperium/config.local.toml`, then `IMPERIUM_<SECTION>__<KEY>` environment variables.
//! Every value remembers which layer set it, for `imperium config explain`.

mod budgets;
mod endpoints;
mod layers;
mod local_models;
mod mcp_servers;
mod paths;
mod permissions;
mod registry;
mod secrets;
mod settings;

pub use budgets::{
    Budget, BudgetAction, BudgetScope, BudgetSettings, BudgetWindow, DEFAULT_THRESHOLDS, SOURCES,
    WeekStart, save_budgets,
};
pub use endpoints::{save_endpoint, valid_endpoint_id};
pub use layers::{
    Config, ConfigError, Explained, Layer, Source, load, load_layers, replace_app_layer,
};
pub use local_models::{
    CACHE_TYPES, CPU_LANE_MAX_WAIT_S, CPU_LANE_SLOTS, ContextLimit, DEFAULT_SLOTS, DraftLaunch,
    INITIAL_POOL_TOKENS, LaneRow, LocalModelSettings, MAX_ALIAS_CHARS, MAX_CARD_CHARS, MAX_SLOTS,
    ModelLaunch, NO_CONTEXT_LIMIT, TableOverride, check_alias, check_card, check_launch,
    check_local_model,
};
pub use mcp_servers::{
    MAX_SERVER_NAME, remove_mcp_server, save_mcp_server, set_mcp_server_enabled, valid_server_name,
};
pub use paths::{Paths, PathsError};
pub use permissions::{PermissionSettings, save_permissions};
pub use registry::{
    SETTINGS, SetError, SettingDef, SettingKind, SettingState, describe, describe_key,
    set_user_setting, setting,
};
pub use secrets::{
    KeychainSecrets, MemorySecrets, SecretError, SecretName, SecretStore, delete_secret,
    load_secret, store_secret,
};
pub use settings::{
    ActionSettings, AgentSettings, ApiSettings, AutofixSettings, Capability, CatalogEntry,
    ConditionSettings, ContextSettings, DaemonSettings, EndpointSettings, Enforce,
    ExtensionSettings, GithubSettings, GithubWatch, HardwareSettings, HookSettings, ImageSettings,
    IsolationSettings, LLAMACPP_ENDPOINT, ListModelsScope, LlamaCppSettings, LoadSettings,
    Location, McpServerSettings, McpSettings, MediaServiceSettings, MediaSettings, MemorySettings,
    MemoryWrite, ModelPlacement, ModelSettings, OpenAiApiSettings, PetSettings, PhoneSettings,
    PolicySettings, Preference, ProjectSettings, ResumeSettings, RoutingRule, RoutingSettings,
    RuntimeSettings, ScheduleSettings, SecretRef, SessionSearchScope, SessionSettings, Settings,
    Tier, TriggerSettings, VLLM_ENDPOINT, VllmSettings, WebSettings, WorkflowSettings,
    WorkspacesSettings, WritePolicy, default_workspaces_root,
};
pub use settings::{SuggestionCandidate, default_suggestions};
