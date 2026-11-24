//! The Imperium daemon: the single process that owns all state. Clients (the CLI, the desktop
//! app, remote hosts) talk to it over JSON-RPC on a local, token-protected WebSocket.

#![warn(unreachable_pub)]

mod app_identity;
mod approvals;
mod attachments;
mod autofix;
mod bench;
mod browser_control;
mod budgets;
pub use budgets::{CallGate, CallUsage};
mod checkpoints;
mod config;
mod config_changes;
mod connectors;
mod downloads;
mod endpoint;
mod environments;
mod eval;
mod events;
mod export;
mod extension_install;
mod github_token;
mod initiative;
mod knowledge;
mod lifetime;
mod llama_runtime;
mod load;
mod local;
mod marketplaces;
mod mcp;
mod mod_install;
mod model_config;
mod mods;
mod onboarding;
mod openai;
mod peer;
mod permissions;
mod pets;
mod phone;
mod plugin_install;
mod plugin_records;
mod plugin_suggestions;
mod project_agent;
pub mod residency;
mod resources;
mod review;
mod rpc;
mod runtimes;
mod safe_git;
mod scheduler;
mod server;
mod state;
mod suggestions;
mod token;
mod voice;
mod workspaces;
mod worktrees;
mod writers;

pub use approvals::Approvals;
pub use autofix::{
    FIX_TOOLS, ci_config_paths, ci_prompt, fix_ci, push_approval, watch_pull_requests,
};
pub use bench::Bench;
pub use downloads::Downloads;
pub use endpoint::EndpointFile;
pub use environments::{EnvironmentError, Environments};
pub use github_token::{EnvToken, GhThenEnv, daemon_github};
pub use initiative::{InitiativeBook, Outcome as InitiativeOutcome};
pub use load::{LoadMonitor, ManualLoad, Sample, classify};
pub use local::LocalRuntime;
pub use openai::OpenAiApi;
pub use pets::PetEngine;
pub use phone::Devices;
pub use project_agent::ProjectAgents;
pub use resources::{ResourceMonitor, Watch as ResourceWatch};
pub use rpc::{Connection, Deferred, Handled, Subscription, next_event};
pub use scheduler::Scheduler;
pub use server::{serve, serve_ipc};
pub use state::{CreateError, DaemonState, SessionOptions};
pub use token::AccessToken;
pub use writers::GitWriterTrees;
