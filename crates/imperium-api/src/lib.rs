//! Types and traits shared between Imperium crates, so a crate can use another subsystem's
//! vocabulary without depending on its implementation. This crate depends on no other
//! Imperium crate; everything else may depend on it.

mod chat;
mod effort;
mod load;
mod local;
pub mod markdown;
mod permissions;
mod pricing;
mod tools;

pub use chat::{ChatMessage, FunctionCall, ToolCall, ToolSpec, Usage};
pub use effort::{Effort, UnknownEffort, reasoning_by_name, reasoning_from_tags, template_reasons};
pub use load::{Load, LoadView};
pub use local::{
    AcquireFuture, Availability, LocalLease, LocalModels, LocalPresence, LocalRequest, LocalWait,
    Priority, ResumeFuture, SuspendFuture, holder, holder_session,
};
pub use permissions::{PathGrant, PathMode, PermissionRule, RuleAction};
pub use pricing::Pricing;
pub use tools::{
    StartFuture, ToolCheck, ToolContext, ToolEvent, ToolFuture, ToolGroup, ToolProvider,
    parse_arguments, truncate_chars,
};
