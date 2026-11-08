//! Types and traits shared between Imperium crates, so a crate can use another subsystem's
//! vocabulary without depending on its implementation. This crate depends on no other
//! Imperium crate; everything else may depend on it.

mod chat;
mod local;
pub mod markdown;
mod pricing;

pub use chat::{ChatMessage, FunctionCall, ToolCall, ToolSpec, Usage};
pub use local::{AcquireFuture, Availability, LocalLease, LocalModels};
pub use pricing::Pricing;
