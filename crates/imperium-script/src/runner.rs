//! The interface the engine drives programs through (plan C3): [`ScriptRunner`] starts a program,
//! [`ScriptProgram`] yields its events and takes the host's answers. The engine holds an
//! `Arc<dyn ScriptRunner>`, so the traits are object-safe and return boxed futures.

use std::{future::Future, pin::Pin, time::Instant};

use monty_types::MontyObject;

use crate::driver::{Limits, ScriptError, ScriptEvent};

/// A boxed, sendable future: what the traits return so they stay object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Starts programs. [`crate::Launcher`] starts confined workers; [`crate::FakeRunner`] replays
/// scripted events.
pub trait ScriptRunner: Send + Sync {
    /// Starts `code` under `limits`. A program that names `eval` or `exec` is refused with
    /// [`ScriptError::Refused`] and one that cannot get a confined worker with
    /// [`ScriptError::Unavailable`]; neither runs.
    fn start<'a>(
        &'a self,
        code: &'a str,
        limits: &'a Limits,
    ) -> BoxFuture<'a, Result<Box<dyn ScriptProgram>, ScriptError>>;
}

/// One running program. After [`ScriptEvent::NameLookup`] or [`ScriptEvent::Call`] the host
/// answers exactly once before it asks for the next event. Dropping it ends the program.
pub trait ScriptProgram: Send {
    /// The next event, ending the program with [`ScriptError::WallBudget`] when `deadline`
    /// passes first.
    fn next_until(&mut self, deadline: Option<Instant>) -> BoxFuture<'_, ScriptEvent>;

    /// Answers a call with a value.
    fn resume_value(&mut self, value: MontyObject) -> BoxFuture<'_, Result<(), ScriptError>>;

    /// Answers a call by raising `RuntimeError(message)` in the program.
    fn resume_error(&mut self, message: String) -> BoxFuture<'_, Result<(), ScriptError>>;

    /// Answers a call to a name that is no function the program may call: `NameError`.
    fn resume_not_found(&mut self) -> BoxFuture<'_, Result<(), ScriptError>>;

    /// Answers a name lookup: a host function of that name, or `None` for `NameError`.
    fn resume_name(&mut self, function: Option<String>) -> BoxFuture<'_, Result<(), ScriptError>>;

    /// Ends the program now and waits until its worker is gone.
    fn finish(self: Box<Self>) -> BoxFuture<'static, ()>;
}
