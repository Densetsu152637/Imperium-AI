//! How a project agent starts work: it hands a brief to the daemon, which runs it as a new
//! session and later sends the agent that session's report as a message.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub type LaunchFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// Starts sessions on an agent's behalf; returns the new session's id.
pub trait SessionLauncher: Send + Sync + fmt::Debug {
    fn launch<'a>(&'a self, brief: &'a str, model: Option<&'a str>) -> LaunchFuture<'a>;
}
