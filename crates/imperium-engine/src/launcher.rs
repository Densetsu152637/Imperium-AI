//! How a project agent starts work: it hands a brief to the daemon, which runs it as a new
//! session and later sends the agent that session's report as a message.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub type LaunchFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// A request to start a session: the brief, and optionally why and from what.
#[derive(Debug, Clone, Copy)]
pub struct LaunchRequest<'a> {
    pub brief: &'a str,
    pub model: Option<&'a str>,
    /// Why the agent starts it, in a sentence.
    pub reason: Option<&'a str>,
    /// What the agent read to decide it (a file and section, an issue, a session).
    pub source: Option<&'a str>,
}

/// Starts sessions on an agent's behalf; returns the new session's id.
pub trait SessionLauncher: Send + Sync + fmt::Debug {
    fn launch<'a>(&'a self, brief: &'a str, model: Option<&'a str>) -> LaunchFuture<'a>;

    /// Like [`SessionLauncher::launch`], with the reason and source the agent gave. Launchers
    /// that do not record them ignore them.
    fn launch_request<'a>(&'a self, request: LaunchRequest<'a>) -> LaunchFuture<'a> {
        self.launch(request.brief, request.model)
    }
}
