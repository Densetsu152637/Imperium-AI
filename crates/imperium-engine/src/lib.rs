//! Sessions and the agent loop.
//!
//! Every agent (a session's root and each subagent) is its own task on the daemon's
//! multi-threaded Tokio runtime, so agents run in parallel across cores and idle agents use no
//! CPU. Callers talk to a [`Session`] over channels and receive its
//! [`AgentEvent`](imperium_protocol::session::AgentEvent)s by subscribing.

mod agent;
mod agents;
mod artefacts;
mod branch;
mod breakdown;
mod budget_gate;
mod checkpoints;
mod compact;
mod instructions;
mod journal;
mod launcher;
mod model;
mod queue;
mod quiet;
mod session;
mod status;
mod title;

pub use agents::agents_dir;
pub use branch::{BranchPlan, mark_turn, plan_branch, plan_branch_at_end, turns_path};
pub use budget_gate::{BudgetGate, BudgetVerdict};
pub use checkpoints::TurnCheckpoints;
pub use compact::SwitchError;
pub use imperium_api::{
    AcquireError, AcquireFuture, Availability, Load, LoadView, LoadedFuture, LocalLease,
    LocalModels, LocalPresence, LocalRequest, LocalWait, Priority, ResumeFuture, SuspendFuture,
    ToolCheck, ToolContext, ToolEvent, ToolFuture, ToolGroup, ToolProvider,
};
pub use imperium_models::{ChatClient, ChatClients, ChatRequest, Endpoint};
pub use journal::{journalled_model, messages_path, read_events as journal_events};
pub use launcher::{LaunchFuture, LaunchRequest, SessionLauncher};
pub use model::{
    CatalogModel, ModelError, ResolvedModel, Routed, TaskRequest, escalate, resolve_model, route,
    route_noted,
};
pub use queue::QueueError;
pub use quiet::{Background, RunNow, Waited, wait_for_quiet};
pub use session::{Session, SessionClosed, SessionConfig, SessionMemory, new_session_id};
pub use status::next_attempt;
pub use title::task_title;

/// The resume policy used when nothing is configured: wait for the provider's reset, retrying
/// every five minutes, for up to twelve hours.
pub fn default_resume_policy() -> imperium_protocol::session::ResumePolicy {
    imperium_protocol::session::ResumePolicy {
        mode: "wait_for_reset".into(),
        at_time: None,
        retry_every_minutes: 5,
        max_wait_hours: 12,
    }
}
