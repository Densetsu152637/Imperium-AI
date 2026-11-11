//! The interface between the agent loop and the tools it offers beyond its own, so web,
//! browser, media, and MCP tools (or new ones) are plugged in without the agent loop depending
//! on them. The agent runs hooks, the required-context check, and approvals around every call;
//! a provider only describes its tools, says how a call is checked, and runs it.

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde::de::DeserializeOwned;

use crate::ToolSpec;

/// A provider's call: the text the model sees, or an error (shown as `error: <text>`).
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
/// A provider's start-up: notices for the user about anything that is unavailable.
pub type StartFuture<'a> = Pin<Box<dyn Future<Output = Vec<String>> + Send + 'a>>;

/// Where a provider's tools sit among those offered to the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolGroup {
    /// Integrations shipped with Imperium (media, web, browser): offered after the agent's own
    /// tools and counted as built-in tools in context breakdowns.
    #[default]
    Builtin,
    /// Tools from outside servers (MCP): offered right after the file and command tools,
    /// counted as MCP tools in context breakdowns, and matched before other providers.
    External,
}

/// How the agent checks a call before the provider runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCheck {
    /// What the call does, for the approval request and the `tool_started` event.
    pub summary: String,
    /// Ask the user first, unless they allowed the tool for the session.
    pub ask: bool,
    /// Refuse the call while required workflow context is unloaded (under
    /// `required-before-action`).
    pub needs_context: bool,
}

/// Something a call produced that the session reports to its clients.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolEvent {
    /// A media file was saved in the project (`kind` is `image` or `speech`).
    MediaCreated {
        path: String,
        kind: String,
        prompt: String,
    },
    /// The browser's page after an action, with a base64 PNG screenshot.
    BrowserView {
        url: String,
        title: String,
        screenshot: String,
    },
}

/// What a call may use from the session.
pub struct ToolContext<'a> {
    /// The project folder.
    pub root: &'a Path,
    /// Reports an event to the session's clients, attributed to the calling agent.
    pub emit: &'a (dyn Fn(ToolEvent) + Send + Sync),
}

/// A set of tools offered to every agent in a session.
pub trait ToolProvider: Send + Sync + fmt::Debug {
    /// Where the tools sit among those offered; [`ToolGroup::Builtin`] unless overridden.
    fn group(&self) -> ToolGroup {
        ToolGroup::Builtin
    }

    /// Whether subagents are offered the tools too; false keeps them (and their instructions)
    /// to the root agent, the one that delegates.
    fn for_subagents(&self) -> bool {
        true
    }

    /// Prepares the tools before the session's first turn (such as starting servers).
    fn start(&self) -> StartFuture<'_> {
        Box::pin(async { Vec::new() })
    }

    /// Instructions added to the system prompt while the tools are offered.
    fn instructions(&self) -> Option<&str> {
        None
    }

    /// The tool definitions, in the order the model sees them.
    fn specs(&self) -> Vec<ToolSpec>;

    /// Whether this provider runs the tool called `name`.
    fn handles(&self, name: &str) -> bool;

    /// Reads a call's arguments and says how to check it. An error goes back to the model
    /// before any hook runs.
    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String>;

    /// Runs a call that passed its checks.
    fn call<'a>(
        &'a self,
        context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a>;
}

/// Reads a call's JSON arguments; empty arguments are an empty object.
pub fn parse_arguments<T: DeserializeOwned>(arguments: &str) -> Result<T, String> {
    let arguments = if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    };
    serde_json::from_str(arguments).map_err(|error| format!("invalid arguments: {error}"))
}

/// The first `limit` characters of `text`, with an ellipsis when it was longer.
pub fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arguments_are_an_empty_object() {
        #[derive(Debug, serde::Deserialize)]
        struct Args {
            #[serde(default)]
            n: u32,
        }
        let args: Args = parse_arguments("  ").unwrap();
        assert_eq!(args.n, 0);
        assert!(
            parse_arguments::<Args>("{")
                .unwrap_err()
                .starts_with("invalid arguments")
        );
    }

    #[test]
    fn truncation_counts_characters() {
        assert_eq!(truncate_chars("héllo", 5), "héllo");
        assert_eq!(truncate_chars("héllo", 2), "hé…");
    }
}
