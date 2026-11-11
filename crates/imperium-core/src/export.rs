//! `session.export`: a session's transcript as Markdown or JSON, built from its journal events.
//!
//! The journal holds every agent's events (a subagent's carry its task id), so the same code
//! serves running sessions (their in-memory history) and archived ones (read from disk). The
//! transcript has the root agent's messages, tool calls with arguments and results, and the
//! subagent task tree: each subagent's log sits under the task that started it, at the point in
//! its parent's log where it was spawned. Files agents were given or made are listed apart.

use std::collections::HashMap;
use std::fmt::Write as _;

use imperium_protocol::methods::{ExportFormat, SessionExportParams, SessionExportResult};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent, TokenUsage};
use serde::Serialize;

use crate::state::DaemonState;

/// Bumped when the JSON document changes shape.
const DOCUMENT_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
struct Transcript {
    version: u32,
    session_id: String,
    title: String,
    /// The root agent; subagents are under the agent that started them.
    root: Agent,
    /// Files attached to messages and created by agents (images, speech), in order.
    artefacts: Vec<Artefact>,
}

#[derive(Debug, Default, Serialize)]
struct Agent {
    agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    title: String,
    /// The brief it was given (empty for the root).
    task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// How its latest task ended; absent for the root and for one still working.
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
    /// Its latest report.
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<TokenUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    entries: Vec<Entry>,
    /// Subagents it started, with their own logs (left out with `include_subagents: false`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subagents: Vec<Agent>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Entry {
    User {
        text: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    Assistant {
        text: String,
    },
    Tool {
        call_id: String,
        tool: String,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        arguments: Option<String>,
        /// Absent when the call never finished.
        #[serde(skip_serializing_if = "Option::is_none")]
        ok: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    /// A subagent was started here; its log is under `subagents`.
    Subagent {
        task_id: String,
    },
    /// A follow-up the user or parent gave a finished subagent.
    Followup {
        task_id: String,
        text: String,
    },
    Media {
        path: String,
        kind: String,
        prompt: String,
    },
    Compaction {
        before_tokens: u64,
        after_tokens: u64,
        summarised_messages: u32,
        model: String,
    },
    ModelChanged {
        from: String,
        to: String,
    },
    Notice {
        message: String,
    },
    Failure {
        message: String,
    },
}

#[derive(Debug, Serialize)]
struct Artefact {
    path: String,
    /// `attachment`, `image`, or `speech`.
    kind: String,
    /// The agent that was given or made it.
    agent_id: String,
    /// What made it, for a created file.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
}

/// Builds the transcript from a session's events.
fn build(session_id: &str, title: &str, events: &[AgentEvent], subagents: bool) -> Transcript {
    let mut agents: HashMap<String, Agent> = HashMap::new();
    // Agent ids in the order they were first seen, so subagents nest in start order.
    let mut order: Vec<String> = Vec::new();
    let mut pending: HashMap<String, String> = HashMap::new();
    let mut artefacts = Vec::new();
    let agent = |id: &str, agents: &mut HashMap<String, Agent>, order: &mut Vec<String>| {
        if !agents.contains_key(id) {
            agents.insert(
                id.to_string(),
                Agent {
                    agent_id: id.to_string(),
                    title: if id == ROOT_AGENT { "Root agent" } else { id }.to_string(),
                    ..Agent::default()
                },
            );
            order.push(id.to_string());
        }
    };
    agent(ROOT_AGENT, &mut agents, &mut order);

    for AgentEvent { agent_id, event } in events {
        agent(agent_id, &mut agents, &mut order);
        // The reply streams as deltas; any other event ends the fragment being gathered.
        if !matches!(event, SessionEvent::TextDelta { .. })
            && let Some(text) = pending.remove(agent_id)
            && let Some(node) = agents.get_mut(agent_id)
        {
            push_assistant(node, text);
        }
        match event {
            SessionEvent::TextDelta { text } => {
                pending.entry(agent_id.clone()).or_default().push_str(text);
            }
            SessionEvent::TurnStarted { text, attachments } => {
                for attachment in attachments {
                    artefacts.push(Artefact {
                        path: attachment.path.clone(),
                        kind: "attachment".into(),
                        agent_id: agent_id.clone(),
                        prompt: None,
                    });
                }
                agents.get_mut(agent_id).unwrap().entries.push(Entry::User {
                    text: text.clone(),
                    attachments: attachments.iter().map(|a| a.path.clone()).collect(),
                });
            }
            SessionEvent::ToolStarted {
                call_id,
                tool,
                summary,
                arguments,
            } => agents.get_mut(agent_id).unwrap().entries.push(Entry::Tool {
                call_id: call_id.clone(),
                tool: tool.clone(),
                summary: summary.clone(),
                arguments: arguments.clone(),
                ok: None,
                output: None,
                duration_ms: None,
            }),
            SessionEvent::ToolFinished {
                call_id,
                ok,
                output,
                tool,
                duration_ms,
            } => {
                let node = agents.get_mut(agent_id).unwrap();
                let open = node.entries.iter_mut().rev().find_map(|entry| match entry {
                    Entry::Tool {
                        call_id: id,
                        ok: done,
                        ..
                    } if id == call_id && done.is_none() => Some(entry),
                    _ => None,
                });
                match open {
                    Some(Entry::Tool {
                        ok: done,
                        output: out,
                        duration_ms: took,
                        ..
                    }) => {
                        *done = Some(*ok);
                        *out = Some(output.clone());
                        *took = *duration_ms;
                    }
                    // Denied or blocked before it started.
                    _ => node.entries.push(Entry::Tool {
                        call_id: call_id.clone(),
                        tool: tool.clone().unwrap_or_default(),
                        summary: String::new(),
                        arguments: None,
                        ok: Some(*ok),
                        output: Some(output.clone()),
                        duration_ms: *duration_ms,
                    }),
                }
            }
            SessionEvent::TaskSpawned {
                task_id,
                brief,
                model,
                title,
                ..
            } => {
                agents
                    .get_mut(agent_id)
                    .unwrap()
                    .entries
                    .push(Entry::Subagent {
                        task_id: task_id.clone(),
                    });
                agent(task_id, &mut agents, &mut order);
                let node = agents.get_mut(task_id).unwrap();
                node.parent = Some(agent_id.clone());
                node.task = brief.clone();
                node.model = Some(model.clone());
                if !title.is_empty() {
                    node.title = title.clone();
                }
            }
            SessionEvent::TaskFinished {
                task_id,
                ok,
                report,
                usage,
                cost_usd,
                ..
            } => {
                agent(task_id, &mut agents, &mut order);
                let node = agents.get_mut(task_id).unwrap();
                node.ok = Some(*ok);
                node.report = Some(report.clone());
                node.usage = Some(usage.clone());
                node.cost_usd = *cost_usd;
            }
            SessionEvent::TaskResumed { task_id, text, .. } => {
                agents
                    .get_mut(agent_id)
                    .unwrap()
                    .entries
                    .push(Entry::Followup {
                        task_id: task_id.clone(),
                        text: text.clone(),
                    });
            }
            SessionEvent::MediaCreated { path, kind, prompt } => {
                artefacts.push(Artefact {
                    path: path.clone(),
                    kind: kind.clone(),
                    agent_id: agent_id.clone(),
                    prompt: Some(prompt.clone()),
                });
                agents
                    .get_mut(agent_id)
                    .unwrap()
                    .entries
                    .push(Entry::Media {
                        path: path.clone(),
                        kind: kind.clone(),
                        prompt: prompt.clone(),
                    });
            }
            SessionEvent::ContextCompacted {
                model,
                before_tokens,
                after_tokens,
                summarised_messages,
                ..
            } => agents
                .get_mut(agent_id)
                .unwrap()
                .entries
                .push(Entry::Compaction {
                    before_tokens: *before_tokens,
                    after_tokens: *after_tokens,
                    summarised_messages: *summarised_messages,
                    model: model.clone(),
                }),
            SessionEvent::ModelChanged { from, to, .. } => {
                if agent_id == ROOT_AGENT {
                    agents
                        .get_mut(agent_id)
                        .unwrap()
                        .entries
                        .push(Entry::ModelChanged {
                            from: from.clone(),
                            to: to.clone(),
                        });
                }
            }
            SessionEvent::Notice { message } => {
                agents
                    .get_mut(agent_id)
                    .unwrap()
                    .entries
                    .push(Entry::Notice {
                        message: message.clone(),
                    })
            }
            SessionEvent::TurnFailed { message } => {
                agents
                    .get_mut(agent_id)
                    .unwrap()
                    .entries
                    .push(Entry::Failure {
                        message: message.clone(),
                    })
            }
            _ => {}
        }
    }
    // Replies still streaming when the events end.
    for (id, text) in pending {
        if let Some(node) = agents.get_mut(&id) {
            push_assistant(node, text);
        }
    }

    // Nest each subagent under its parent, last started first so children are complete before
    // they move.
    for id in order.iter().rev() {
        if id == ROOT_AGENT {
            continue;
        }
        let Some(node) = agents.remove(id) else {
            continue;
        };
        let parent = node.parent.clone().unwrap_or_else(|| ROOT_AGENT.into());
        match agents.get_mut(&parent) {
            Some(parent) => parent.subagents.insert(0, node),
            None => agents
                .get_mut(ROOT_AGENT)
                .expect("the root agent is always present")
                .subagents
                .insert(0, node),
        }
    }
    let mut root = agents.remove(ROOT_AGENT).unwrap_or_default();
    if !subagents {
        strip(&mut root);
    }
    Transcript {
        version: DOCUMENT_VERSION,
        session_id: session_id.into(),
        title: title.into(),
        root,
        artefacts,
    }
}

/// Keeps each subagent's task, report, and usage but drops its log, and their descendants.
fn strip(agent: &mut Agent) {
    for child in &mut agent.subagents {
        child.entries.clear();
        strip(child);
    }
}

fn push_assistant(node: &mut Agent, text: String) {
    if !text.trim().is_empty() {
        node.entries.push(Entry::Assistant { text });
    }
}

/// Markdown for one agent's log and, where each was started, its subagents'.
fn markdown(out: &mut String, agent: &Agent, depth: usize) {
    let hashes = "#".repeat((depth + 2).min(6));
    for entry in &agent.entries {
        match entry {
            Entry::User { text, attachments } => {
                let _ = writeln!(out, "{hashes} User\n\n{}\n", text.trim_end());
                for path in attachments {
                    let _ = writeln!(out, "- Attached: `{path}`");
                }
                if !attachments.is_empty() {
                    out.push('\n');
                }
            }
            Entry::Assistant { text } => {
                let _ = writeln!(out, "{hashes} Assistant\n\n{}\n", text.trim_end());
            }
            Entry::Tool {
                tool,
                summary,
                arguments,
                ok,
                output,
                duration_ms,
                ..
            } => {
                let status = match ok {
                    Some(true) => "ok",
                    Some(false) => "failed",
                    None => "no result",
                };
                let took = duration_ms.map_or(String::new(), |ms| format!(", {ms} ms"));
                let _ = writeln!(out, "{hashes} Tool: {tool} ({status}{took})\n");
                if !summary.is_empty() {
                    let _ = writeln!(out, "{summary}\n");
                }
                if let Some(arguments) = arguments {
                    let _ = writeln!(out, "Arguments:\n\n{}\n", fenced(arguments, "json"));
                }
                if let Some(output) = output {
                    let _ = writeln!(out, "Result:\n\n{}\n", fenced(output, ""));
                }
            }
            Entry::Subagent { task_id } => {
                if let Some(child) = agent.subagents.iter().find(|c| &c.agent_id == task_id) {
                    subagent(out, child, depth + 1);
                }
            }
            Entry::Followup { task_id, text } => {
                let _ = writeln!(
                    out,
                    "{hashes} Follow-up for {task_id}\n\n{}\n",
                    text.trim_end()
                );
            }
            Entry::Media { path, kind, prompt } => {
                let _ = writeln!(out, "- Created {kind}: `{path}` (from: {prompt})\n");
            }
            Entry::Compaction {
                before_tokens,
                after_tokens,
                summarised_messages,
                model,
            } => {
                let _ = writeln!(
                    out,
                    "> Context compacted for {model}: {summarised_messages} messages summarised, \
                     {before_tokens} to {after_tokens} tokens.\n"
                );
            }
            Entry::ModelChanged { from, to } => {
                let _ = writeln!(out, "> Model changed from {from} to {to}.\n");
            }
            Entry::Notice { message } => {
                let _ = writeln!(out, "> Notice: {message}\n");
            }
            Entry::Failure { message } => {
                let _ = writeln!(out, "> Turn failed: {message}\n");
            }
        }
    }
}

fn subagent(out: &mut String, agent: &Agent, depth: usize) {
    let hashes = "#".repeat((depth + 1).min(6));
    let state = match agent.ok {
        Some(true) => "finished",
        Some(false) => "failed",
        None => "unfinished",
    };
    let _ = writeln!(
        out,
        "{hashes} Subagent {}: {} ({state})\n",
        agent.agent_id, agent.title
    );
    if let Some(model) = &agent.model {
        let _ = writeln!(out, "Model: `{model}`\n");
    }
    let _ = writeln!(out, "Task:\n\n{}\n", fenced(&agent.task, ""));
    markdown(out, agent, depth + 1);
    if let Some(report) = &agent.report {
        let _ = writeln!(out, "Report:\n\n{}\n", fenced(report, ""));
    }
}

/// `text` in a code fence long enough not to be closed by anything inside it.
fn fenced(text: &str, language: &str) -> String {
    let mut ticks = 3;
    while text.contains(&"`".repeat(ticks)) {
        ticks += 1;
    }
    let fence = "`".repeat(ticks);
    format!("{fence}{language}\n{}\n{fence}", text.trim_end())
}

fn render_markdown(transcript: &Transcript) -> String {
    let title = if transcript.title.is_empty() {
        transcript.session_id.as_str()
    } else {
        transcript.title.as_str()
    };
    let mut out = format!("# {title}\n\nSession `{}`\n\n", transcript.session_id);
    markdown(&mut out, &transcript.root, 0);
    if !transcript.artefacts.is_empty() {
        out.push_str("## Files\n\n");
        for artefact in &transcript.artefacts {
            let _ = writeln!(
                out,
                "- `{}` ({}, {})",
                artefact.path, artefact.kind, artefact.agent_id
            );
        }
        out.push('\n');
    }
    out
}

/// A file name for the export: the title (or id) as a short slug.
fn filename(title: &str, session_id: &str, format: ExportFormat) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let stem: String = if slug.is_empty() {
        session_id.to_string()
    } else {
        slug.chars()
            .take(48)
            .collect::<String>()
            .trim_end_matches('-')
            .to_string()
    };
    let extension = match format {
        ExportFormat::Markdown => "md",
        ExportFormat::Json => "json",
    };
    format!("{stem}.{extension}")
}

/// Renders events as `format`. Public to the crate for tests; the daemon calls [`export`].
pub(crate) fn render(
    session_id: &str,
    title: &str,
    events: &[AgentEvent],
    format: ExportFormat,
    subagents: bool,
) -> SessionExportResult {
    let transcript = build(session_id, title, events, subagents);
    let content = match format {
        ExportFormat::Markdown => render_markdown(&transcript),
        ExportFormat::Json => {
            serde_json::to_string_pretty(&transcript).unwrap_or_else(|_| "{}".into())
        }
    };
    SessionExportResult {
        format,
        filename: filename(title, session_id, format),
        content,
    }
}

/// `session.export`: the session's transcript, or an error message when there is no such
/// session. Works for running and archived sessions.
pub fn export(
    state: &DaemonState,
    params: &SessionExportParams,
) -> Result<SessionExportResult, String> {
    let (title, events) = state
        .transcript_source(&params.session_id)
        .ok_or_else(|| format!("no session {:?}", params.session_id))?;
    Ok(render(
        &params.session_id,
        &title,
        &events,
        params.format,
        params.include_subagents.unwrap_or(true),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_protocol::session::TokenUsage;

    fn at(agent: &str, event: SessionEvent) -> AgentEvent {
        AgentEvent {
            agent_id: agent.into(),
            event,
        }
    }

    fn events() -> Vec<AgentEvent> {
        vec![
            at(
                "root",
                SessionEvent::TurnStarted {
                    text: "fix it".into(),
                    attachments: Vec::new(),
                },
            ),
            at(
                "root",
                SessionEvent::TextDelta {
                    text: "Looking".into(),
                },
            ),
            at(
                "root",
                SessionEvent::TextDelta {
                    text: " now.".into(),
                },
            ),
            at(
                "root",
                SessionEvent::ToolStarted {
                    call_id: "c1".into(),
                    tool: "spawn_agent".into(),
                    summary: "spawn".into(),
                    arguments: Some("{\"brief\":\"look\"}".into()),
                },
            ),
            at(
                "root",
                SessionEvent::TaskSpawned {
                    task_id: "t1".into(),
                    brief: "look around".into(),
                    model: "m".into(),
                    title: "Look around".into(),
                    at_ms: None,
                },
            ),
            at(
                "t1",
                SessionEvent::TurnStarted {
                    text: "look around".into(),
                    attachments: Vec::new(),
                },
            ),
            at(
                "t1",
                SessionEvent::TextDelta {
                    text: "found it".into(),
                },
            ),
            at(
                "t1",
                SessionEvent::TaskFinished {
                    task_id: "t1".into(),
                    ok: true,
                    report: "found it".into(),
                    usage: TokenUsage::default(),
                    cost_usd: None,
                    at_ms: None,
                },
            ),
            at(
                "root",
                SessionEvent::ToolFinished {
                    call_id: "c1".into(),
                    ok: true,
                    output: "started t1".into(),
                    tool: Some("spawn_agent".into()),
                    duration_ms: Some(3),
                },
            ),
            at(
                "root",
                SessionEvent::TextDelta {
                    text: "Done.".into(),
                },
            ),
        ]
    }

    #[test]
    fn markdown_nests_the_subagent_where_it_was_started() {
        let result = render(
            "s1",
            "Fix the parser",
            &events(),
            ExportFormat::Markdown,
            true,
        );
        assert_eq!(result.filename, "fix-the-parser.md");
        let text = result.content;
        assert!(text.starts_with("# Fix the parser"));
        assert!(text.contains("Looking now."));
        let tool = text.find("Tool: spawn_agent (ok, 3 ms)").unwrap();
        let sub = text.find("Subagent t1: Look around (finished)").unwrap();
        let done = text.find("Done.").unwrap();
        assert!(tool < sub && sub < done);
        assert!(text.contains("\"brief\":\"look\""));
        assert!(text.contains("found it"));
    }

    #[test]
    fn json_nests_subagents_and_can_leave_their_logs_out() {
        let full = render("s1", "t", &events(), ExportFormat::Json, true);
        let doc: serde_json::Value = serde_json::from_str(&full.content).unwrap();
        let sub = &doc["root"]["subagents"][0];
        assert_eq!(sub["agent_id"], "t1");
        assert_eq!(sub["report"], "found it");
        assert!(sub["entries"].as_array().unwrap().len() >= 2);
        assert_eq!(full.filename, "t.json");

        let short = render("s1", "t", &events(), ExportFormat::Json, false);
        let doc: serde_json::Value = serde_json::from_str(&short.content).unwrap();
        let sub = &doc["root"]["subagents"][0];
        assert_eq!(sub["task"], "look around");
        assert_eq!(sub["report"], "found it");
        assert!(sub["entries"].as_array().unwrap().is_empty());
    }
}
