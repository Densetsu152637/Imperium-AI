//! A persistent agent's permission profile: what it may use *beyond* the safety floor and the
//! environment's permission mode, which always apply on top. A profile can only narrow what the
//! session offers (it hides and refuses tools); it never grants a tool, a path or an approval.
//!
//! The profile is a live handle: a session's workspace holds it, every agent of the session
//! (its subagents too) reads it at each model call and each tool call, and the sessions the
//! agent starts get the same handle, so they cannot escape it by delegating, and a change
//! applies from the next call.

use std::sync::{Arc, PoisonError, RwLock};

/// The names of the built-in tools that run commands.
pub const SHELL_TOOLS: [&str; 3] = ["run_command", "command_output", "command_stop"];

/// Which of a kind of outside tool (MCP servers, plugins) an agent may use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Allowlist {
    /// Everything the session offers (no extra restriction).
    #[default]
    All,
    /// Only the named ones, compared ignoring case; an empty list allows none.
    Named(Vec<String>),
}

impl Allowlist {
    /// No server or plugin at all.
    pub fn none() -> Self {
        Allowlist::Named(Vec::new())
    }

    /// Whether `name` is allowed.
    pub fn permits(&self, name: &str) -> bool {
        match self {
            Allowlist::All => true,
            Allowlist::Named(names) => names.iter().any(|known| known.eq_ignore_ascii_case(name)),
        }
    }

    /// Whether nothing at all is allowed.
    pub fn is_none(&self) -> bool {
        matches!(self, Allowlist::Named(names) if names.is_empty())
    }
}

/// What a profile says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileRules {
    /// MCP servers (by the name in the configuration) whose tools the agent may use.
    pub mcp: Allowlist,
    /// Plugins the agent may activate and use the tools of.
    pub plugins: Allowlist,
    /// Local files only: no web, browser, search, media, MCP, plugin or other provider tools, no
    /// commands, and file tools confined to the environment's folders and the session's scratch
    /// folder.
    pub local_files_only: bool,
}

impl ProfileRules {
    /// Whether the rules restrict nothing.
    pub fn is_unrestricted(&self) -> bool {
        *self == ProfileRules::default()
    }
}

/// A profile shared by the sessions of one agent; clones share the rules.
#[derive(Debug, Clone, Default)]
pub struct AgentProfile(Arc<RwLock<ProfileRules>>);

impl PartialEq for AgentProfile {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for AgentProfile {}

impl AgentProfile {
    pub fn new(rules: ProfileRules) -> Self {
        AgentProfile(Arc::new(RwLock::new(rules)))
    }

    /// The rules as they are now.
    pub fn rules(&self) -> ProfileRules {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replaces the rules; every holder of the handle sees them from its next call.
    pub fn set(&self, rules: ProfileRules) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = rules;
    }

    pub fn is_unrestricted(&self) -> bool {
        self.rules().is_unrestricted()
    }

    /// Whether files are all the agent may touch.
    pub fn local_files_only(&self) -> bool {
        self.rules().local_files_only
    }

    /// Whether the agent may run commands.
    pub fn allows_shell(&self) -> bool {
        !self.local_files_only()
    }

    /// Whether the agent may use the tools of the integrations built into Imperium (web, browser,
    /// search, media, preview, and the like).
    pub fn allows_provider_tools(&self) -> bool {
        !self.local_files_only()
    }

    /// Whether the agent may use a tool of the MCP server `server` (`None`: a server whose name
    /// is not known, which only an unrestricted list allows).
    pub fn allows_mcp(&self, server: Option<&str>) -> bool {
        let rules = self.rules();
        if rules.local_files_only {
            return false;
        }
        match (&rules.mcp, server) {
            (Allowlist::All, _) => true,
            (list, Some(server)) => list.permits(server),
            (Allowlist::Named(_), None) => false,
        }
    }

    /// Whether the agent may use plugin `plugin`'s tools.
    pub fn allows_plugin(&self, plugin: &str) -> bool {
        let rules = self.rules();
        !rules.local_files_only && rules.plugins.permits(plugin)
    }

    /// Whether the agent may activate any plugin at all.
    pub fn allows_any_plugin(&self) -> bool {
        let rules = self.rules();
        !rules.local_files_only && !rules.plugins.is_none()
    }

    /// Why a tool is refused, if the profile refuses it: `source` says where the tool comes from.
    pub fn refusal(&self, source: &ToolSource<'_>) -> Option<String> {
        let rules = self.rules();
        let lock = "this agent is limited to local files only";
        let allowed = match source {
            ToolSource::Shell if rules.local_files_only => Err(lock.to_string()),
            ToolSource::Provider if rules.local_files_only => Err(lock.to_string()),
            ToolSource::PluginActivation if rules.local_files_only => Err(lock.to_string()),
            ToolSource::PluginActivation if rules.plugins.is_none() => {
                Err("this agent may not use plugins".to_string())
            }
            ToolSource::Mcp { .. } | ToolSource::Plugin { .. } if rules.local_files_only => {
                Err(lock.to_string())
            }
            ToolSource::Mcp { server } => {
                if self.allows_mcp(*server) {
                    Ok(())
                } else {
                    Err(match server {
                        Some(server) => format!("this agent may not use the MCP server {server}"),
                        None => "this agent may not use MCP servers".to_string(),
                    })
                }
            }
            ToolSource::Plugin { plugin } => {
                if rules.plugins.permits(plugin) {
                    Ok(())
                } else {
                    Err(format!("this agent may not use the plugin {plugin}"))
                }
            }
            _ => Ok(()),
        };
        allowed.err()
    }
}

/// Where a tool comes from, for judging it against a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource<'a> {
    /// `run_command` and the tools that read or stop its processes.
    Shell,
    /// A tool of an integration built into Imperium (web, browser, media, preview, ...).
    Provider,
    /// A tool of an MCP server (`None`: the server is not known).
    Mcp { server: Option<&'a str> },
    /// A tool of an active plugin.
    Plugin { plugin: &'a str },
    /// `activate_plugin`.
    PluginActivation,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(mcp: Allowlist, plugins: Allowlist, local_files_only: bool) -> AgentProfile {
        AgentProfile::new(ProfileRules {
            mcp,
            plugins,
            local_files_only,
        })
    }

    #[test]
    fn the_default_profile_restricts_nothing() {
        let profile = AgentProfile::default();
        assert!(profile.is_unrestricted());
        assert!(profile.allows_shell());
        assert!(profile.allows_provider_tools());
        assert!(profile.allows_mcp(Some("any")));
        assert!(profile.allows_mcp(None));
        assert!(profile.allows_plugin("any"));
        assert!(profile.allows_any_plugin());
        for source in [
            ToolSource::Shell,
            ToolSource::Provider,
            ToolSource::Mcp { server: Some("a") },
            ToolSource::Plugin { plugin: "p" },
            ToolSource::PluginActivation,
        ] {
            assert_eq!(profile.refusal(&source), None);
        }
    }

    #[test]
    fn allowlists_name_servers_and_plugins_ignoring_case() {
        let profile = profile(
            Allowlist::Named(vec!["Github".into()]),
            Allowlist::Named(vec!["docs".into()]),
            false,
        );
        assert!(profile.allows_mcp(Some("github")));
        assert!(!profile.allows_mcp(Some("slack")));
        assert!(!profile.allows_mcp(None));
        assert!(profile.allows_plugin("DOCS"));
        assert!(!profile.allows_plugin("other"));
        assert!(profile.allows_any_plugin());
        assert!(profile.allows_shell());
        assert!(
            profile
                .refusal(&ToolSource::Mcp {
                    server: Some("slack")
                })
                .unwrap()
                .contains("slack")
        );
        assert!(
            profile
                .refusal(&ToolSource::Plugin { plugin: "other" })
                .is_some()
        );
    }

    #[test]
    fn none_allows_no_server_and_no_plugin() {
        let profile = profile(Allowlist::none(), Allowlist::none(), false);
        assert!(!profile.allows_mcp(Some("github")));
        assert!(!profile.allows_plugin("docs"));
        assert!(!profile.allows_any_plugin());
        assert!(profile.refusal(&ToolSource::PluginActivation).is_some());
        // Built-in integrations and commands are not covered by the allowlists.
        assert!(profile.allows_provider_tools());
        assert!(profile.allows_shell());
    }

    #[test]
    fn local_files_only_removes_everything_but_file_tools() {
        let profile = profile(Allowlist::All, Allowlist::All, true);
        assert!(!profile.allows_shell());
        assert!(!profile.allows_provider_tools());
        assert!(!profile.allows_mcp(Some("github")));
        assert!(!profile.allows_plugin("docs"));
        assert!(!profile.allows_any_plugin());
        for source in [
            ToolSource::Shell,
            ToolSource::Provider,
            ToolSource::Mcp { server: Some("a") },
            ToolSource::Plugin { plugin: "p" },
            ToolSource::PluginActivation,
        ] {
            assert!(
                profile
                    .refusal(&source)
                    .unwrap()
                    .contains("local files only"),
                "{source:?}"
            );
        }
    }

    #[test]
    fn a_change_reaches_every_holder_of_the_handle() {
        let profile = AgentProfile::default();
        let session = profile.clone();
        assert!(session.allows_shell());
        profile.set(ProfileRules {
            local_files_only: true,
            ..ProfileRules::default()
        });
        assert!(!session.allows_shell());
        assert_eq!(session, profile);
        assert_ne!(AgentProfile::default(), profile);
    }
}
