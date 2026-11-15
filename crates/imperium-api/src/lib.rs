//! Types and traits shared between Imperium crates, so a crate can use another subsystem's
//! vocabulary without depending on its implementation. This crate depends on no other
//! Imperium crate; everything else may depend on it.

#![warn(unreachable_pub)]

mod chat;
mod effort;
pub mod encoding;
pub mod framing;
pub mod fs;
mod load;
mod local;
pub mod markdown;
mod permissions;
mod pricing;
pub mod time;
mod tools;

pub use chat::{ChatMessage, FunctionCall, ToolCall, ToolSpec, Usage};
pub use effort::{Effort, UnknownEffort, reasoning_by_name, reasoning_from_tags, template_reasons};
pub use load::{Load, LoadView};
pub use local::{
    AcquireError, AcquireFuture, Availability, LoadedFuture, LocalLease, LocalModels,
    LocalPresence, LocalRequest, LocalWait, Priority, ResumeFuture, SuspendFuture, holder,
    holder_session,
};
pub use permissions::{
    PathGrant, PathMode, PermissionMode, PermissionRule, RuleAction, UnknownMode,
};
pub use pricing::Pricing;
pub use tools::{
    BrowserTabView, Sampler, SamplingFuture, SamplingMessage, SamplingReply, SamplingRequest,
    StartFuture, ToolCheck, ToolContext, ToolEvent, ToolFuture, ToolGroup, ToolProvider,
    parse_arguments, truncate_chars,
};
