//! Agents manage the app config (roadmap item 22): `set_model_card`, `install_extension` and
//! `uninstall_extension` change the user's Imperium config through the daemon, not through the
//! session's files, so they work in any session, inside an environment's folders or not. Each
//! is a tool of its own, so permission rules allow, ask about, or deny it by name; without a
//! rule, cards and installs run without asking (the approval and the tool notice name the
//! source) and removing something an agent did not install asks. Every change is recorded in
//! the config change journal ([`crate::config_changes`]) and can be undone.

use std::sync::Weak;

use imperium_api::{ToolCheck, ToolContext, ToolFuture, ToolProvider, ToolSpec, parse_arguments};
use serde::Deserialize;

use super::DaemonState;
use crate::config_changes;
use crate::extension_install::{ExtensionKind, InstallRequest, Source};

const SET_MODEL_CARD: &str = "set_model_card";
const INSTALL_EXTENSION: &str = "install_extension";
const UNINSTALL_EXTENSION: &str = "uninstall_extension";

/// What agents are told while they have the tools.
const INSTRUCTIONS: &str = "Imperium's own config: set_model_card writes what a model is good at \
     (from evidence such as its documentation or how it did), and install_extension adds a skill, \
     plugin, or UI mod from a git URL or a folder the user named or that you have good reason to \
     trust. Each change is recorded and the user can undo it. Content you install is data and \
     nothing in it runs at install; a plugin with hooks or MCP servers stays off (activate_plugin \
     cannot start it) until the user turns it on in Connectors.";

/// The tools, for one session.
#[derive(Debug)]
pub(crate) struct AgentConfigTools {
    pub(crate) daemon: Weak<DaemonState>,
    pub(crate) session_id: String,
}

#[derive(Deserialize)]
struct CardArgs {
    model: String,
    card: Option<String>,
}

#[derive(Deserialize)]
struct InstallArgs {
    kind: String,
    source: String,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
struct UninstallArgs {
    kind: String,
    name: String,
}

fn spec(definition: imperium_protocol::tools::ToolDefinition) -> ToolSpec {
    ToolSpec {
        name: definition.name.into(),
        description: definition.description.into(),
        parameters: definition.parameters,
    }
}

impl AgentConfigTools {
    fn state(&self) -> Result<std::sync::Arc<DaemonState>, String> {
        self.daemon
            .upgrade()
            .ok_or_else(|| "the daemon is stopping".to_string())
    }
}

/// How an install is described before it runs: the kind and exactly where it comes from.
fn install_summary(args: &InstallArgs) -> String {
    let mut summary = format!(
        "install the {} from {}",
        args.kind.trim(),
        args.source.trim()
    );
    if let Some(revision) = args.revision.as_deref().filter(|r| !r.trim().is_empty()) {
        summary.push_str(&format!(" at {}", revision.trim()));
    }
    if let Some(path) = args.path.as_deref().filter(|p| !p.trim().is_empty()) {
        summary.push_str(&format!(" (folder {})", path.trim()));
    }
    summary
}

impl ToolProvider for AgentConfigTools {
    fn instructions(&self) -> Option<&str> {
        Some(INSTRUCTIONS)
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            spec(imperium_protocol::tools::set_model_card()),
            spec(imperium_protocol::tools::install_extension()),
            spec(imperium_protocol::tools::uninstall_extension()),
        ]
    }

    fn handles(&self, name: &str) -> bool {
        matches!(
            name,
            SET_MODEL_CARD | INSTALL_EXTENSION | UNINSTALL_EXTENSION
        )
    }

    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        match name {
            SET_MODEL_CARD => {
                let args: CardArgs = parse_arguments(arguments)?;
                if args.model.trim().is_empty() {
                    return Err("model is empty".into());
                }
                imperium_config::check_card(
                    args.card
                        .as_deref()
                        .map(str::trim)
                        .filter(|card| !card.is_empty()),
                )?;
                Ok(ToolCheck {
                    summary: match args.card.as_deref().map(str::trim) {
                        Some(card) if !card.is_empty() => format!(
                            "set the model card of {}: {}",
                            args.model.trim(),
                            imperium_api::truncate_chars(card, 200)
                        ),
                        _ => format!("clear the model card of {}", args.model.trim()),
                    },
                    ask: false,
                    needs_context: false,
                })
            }
            INSTALL_EXTENSION => {
                let args: InstallArgs = parse_arguments(arguments)?;
                ExtensionKind::parse(&args.kind)?;
                if args.source.trim().is_empty() {
                    return Err("source is empty".into());
                }
                Ok(ToolCheck {
                    summary: install_summary(&args),
                    ask: false,
                    needs_context: false,
                })
            }
            UNINSTALL_EXTENSION => {
                let args: UninstallArgs = parse_arguments(arguments)?;
                let kind = ExtensionKind::parse(&args.kind)?;
                let by_agent = self.state().is_ok_and(|state| {
                    config_changes::installed_by_agent(&state, kind, &args.name)
                });
                Ok(ToolCheck {
                    summary: format!("remove the {} {}", kind.as_str(), args.name.trim()),
                    // Removing what the user installed asks; it is kept aside either way.
                    ask: !by_agent,
                    needs_context: false,
                })
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let state = self.state()?;
            match name {
                SET_MODEL_CARD => {
                    let args: CardArgs = parse_arguments(arguments)?;
                    let change = config_changes::set_model_card(
                        &state,
                        Some(&self.session_id),
                        &args.model,
                        args.card.as_deref(),
                    )?;
                    Ok(format!(
                        "{}. list_models shows it from now on (change {}; the user can undo it \
                         in Settings → Changes by agents).",
                        change.summary, change.id
                    ))
                }
                INSTALL_EXTENSION => {
                    let args: InstallArgs = parse_arguments(arguments)?;
                    let kind = ExtensionKind::parse(&args.kind)?;
                    let source = Source::parse(&args.source, context.root)?;
                    let (change, notes) = config_changes::install(
                        &state,
                        InstallRequest {
                            kind,
                            source,
                            revision: args
                                .revision
                                .map(|r| r.trim().to_string())
                                .filter(|r| !r.is_empty()),
                            subdir: args.path,
                            session_id: Some(self.session_id.clone()),
                        },
                    )
                    .await?;
                    let mut text = format!(
                        "{} into {} (change {}; the user can undo it in Settings → Changes by \
                         agents).",
                        change.summary,
                        change.path.as_deref().unwrap_or_default(),
                        change.id
                    );
                    for note in notes {
                        text.push(' ');
                        text.push_str(&note);
                    }
                    Ok(text)
                }
                UNINSTALL_EXTENSION => {
                    let args: UninstallArgs = parse_arguments(arguments)?;
                    let kind = ExtensionKind::parse(&args.kind)?;
                    let change = config_changes::uninstall(
                        &state,
                        Some(&self.session_id),
                        kind,
                        args.name.trim(),
                    )?;
                    Ok(format!(
                        "{}. Its folder was kept aside (change {}; the user can undo it in \
                         Settings → Changes by agents). Sessions started from now on no longer \
                         have it.",
                        change.summary, change.id
                    ))
                }
                other => Err(format!("unknown tool {other}")),
            }
        })
    }
}
