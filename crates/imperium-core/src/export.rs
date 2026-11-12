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
                node.usage = Some(*usage);
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
/// `text` on one line, for a heading.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` as a block quote: every line keeps its `>`, so a multi-line message cannot leak out.
fn quote(label: &str, text: &str) -> String {
    let mut out = String::new();
    for (index, line) in text.trim_end().lines().enumerate() {
        let lead = if index == 0 {
            format!("{label}: ")
        } else {
            String::new()
        };
        let _ = writeln!(out, "> {lead}{line}");
    }
    if out.is_empty() {
        let _ = writeln!(out, "> {label}");
    }
    out.push('\n');
    out
}

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
                    "> Context compacted for {}: {summarised_messages} messages summarised, \
                     {before_tokens} to {after_tokens} tokens.\n",
                    one_line(model)
                );
            }
            Entry::ModelChanged { from, to } => {
                let _ = writeln!(
                    out,
                    "> Model changed from {} to {}.\n",
                    one_line(from),
                    one_line(to)
                );
            }
            Entry::Notice { message } => {
                out.push_str(&quote("Notice", message));
            }
            Entry::Failure { message } => {
                out.push_str(&quote("Turn failed", message));
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
        one_line(&agent.agent_id),
        one_line(&agent.title)
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
    let title = one_line(&transcript.title);
    let title = if title.is_empty() {
        transcript.session_id.as_str()
    } else {
        title.as_str()
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

/// Every string in an agent's log and below, newline-separated.
fn collect_text(agent: &Agent, out: &mut String) {
    let mut add = |text: &str| {
        out.push_str(text);
        out.push('\n');
    };
    add(&agent.task);
    if let Some(report) = &agent.report {
        add(report);
    }
    for entry in &agent.entries {
        match entry {
            Entry::User { text, .. } | Entry::Assistant { text } => add(text),
            Entry::Tool {
                summary,
                arguments,
                output,
                ..
            } => {
                add(summary);
                arguments.iter().chain(output).for_each(|text| add(text));
            }
            Entry::Followup { text, .. } => add(text),
            Entry::Media { prompt, .. } => add(prompt),
            Entry::Notice { message } | Entry::Failure { message } => add(message),
            Entry::Subagent { .. } | Entry::Compaction { .. } | Entry::ModelChanged { .. } => {}
        }
    }
    for child in &agent.subagents {
        collect_text(child, out);
    }
}

/// What credential `text` seems to hold, judged by the shapes real ones have (a known prefix
/// and a long enough body, a private key header, a JWT). Anything else, a git commit hash
/// included, does not count; a clean result is not a promise that nothing secret is there.
fn credential_in(text: &str) -> Option<&'static str> {
    /// Which bytes the body after the prefix may hold.
    #[derive(Clone, Copy)]
    enum Body {
        /// Letters, digits, `_` and `-`.
        Any,
        /// Letters and digits only, so identifiers with underscores do not match.
        Alnum,
        /// Capital letters and digits only, as AWS key ids are.
        Upper,
    }
    // (prefix, reason, shortest body, body shape, exact body length when the format has one)
    const PREFIXES: [(&str, &str, usize, Body, Option<usize>); 16] = [
        ("sk-ant-", "an Anthropic API key", 16, Body::Any, None),
        ("sk-", "an API key", 16, Body::Any, None),
        ("sk_live_", "a Stripe key", 16, Body::Alnum, None),
        ("sk_test_", "a Stripe key", 16, Body::Alnum, None),
        ("ghp_", "a GitHub token", 16, Body::Any, None),
        ("gho_", "a GitHub token", 16, Body::Any, None),
        ("ghs_", "a GitHub token", 16, Body::Any, None),
        ("github_pat_", "a GitHub token", 16, Body::Any, None),
        ("glpat-", "a GitLab token", 16, Body::Any, None),
        ("xoxb-", "a Slack token", 16, Body::Any, None),
        ("xoxp-", "a Slack token", 16, Body::Any, None),
        ("AKIA", "an AWS key", 16, Body::Upper, Some(16)),
        ("ASIA", "an AWS key", 16, Body::Upper, Some(16)),
        ("AIza", "a Google API key", 16, Body::Any, None),
        ("hf_", "a Hugging Face token", 30, Body::Alnum, None),
        ("npm_", "an npm token", 36, Body::Alnum, Some(36)),
    ];
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        return Some("a private key");
    }
    for word in text.split(|c: char| c.is_whitespace() || "\"'`=:,;()[]{}<>".contains(c)) {
        for (prefix, reason, shortest, shape, exact) in PREFIXES {
            if let Some(body) = word.strip_prefix(prefix)
                && body.len() >= shortest
                && exact.is_none_or(|len| body.len() == len)
                && body.bytes().all(|b| match shape {
                    Body::Any => b.is_ascii_alphanumeric() || b == b'_' || b == b'-',
                    Body::Alnum => b.is_ascii_alphanumeric(),
                    Body::Upper => b.is_ascii_uppercase() || b.is_ascii_digit(),
                })
            {
                return Some(reason);
            }
        }
        // A JWT: three base64url parts, the first a JSON header ("eyJ").
        if word.starts_with("eyJ") {
            let parts: Vec<&str> = word.split('.').collect();
            if parts.len() == 3 && parts.iter().all(|part| part.len() >= 8) {
                return Some("a JSON web token");
            }
        }
    }
    None
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
    // Scanned as the strings the transcript holds, before they are serialised (JSON escaping
    // would break up a multi-line key).
    let mut text = String::new();
    collect_text(&transcript.root, &mut text);
    let warning = credential_in(&text).map(|reason| {
        format!(
            "This transcript looks like it contains {reason}. Exports are verbatim, including \
             everything the agents read and ran, so check it before sharing. A transcript \
             without this warning can still hold secrets."
        )
    });
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
        warning,
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
    fn multi_line_titles_and_notices_cannot_break_the_markdown() {
        let mut list = events();
        list.push(at(
            "root",
            SessionEvent::Notice {
                message: "first\n# Injected heading\nthird".into(),
            },
        ));
        let result = render("s1", "Two\n# lines", &list, ExportFormat::Markdown, true);
        assert!(result.content.starts_with("# Two # lines\n"));
        assert!(
            result
                .content
                .contains("> Notice: first\n> # Injected heading\n> third\n")
        );
        assert!(!result.content.contains("\n# Injected heading"));
    }

    #[test]
    fn a_transcript_with_a_credential_carries_a_warning() {
        let mut list = events();
        list.push(at(
            "root",
            SessionEvent::TextDelta {
                text: "the key is ghp_abcdefghijklmnopqrstuvwxyz0123".into(),
            },
        ));
        let result = render("s1", "t", &list, ExportFormat::Json, true);
        assert!(result.warning.unwrap().contains("GitHub token"));
        let clean = render("s1", "t", &events(), ExportFormat::Json, true);
        assert!(clean.warning.is_none());
    }

    #[test]
    fn git_hashes_do_not_warn_but_known_credential_shapes_do() {
        let hash = "9fceb02d0ae598e95dc970b74767f19372d61af8";
        assert_eq!(
            credential_in(&format!("commit {hash} and {hash}12345678")),
            None
        );
        for (text, reason) in [
            ("key sk-ant-abcdefghijklmnop1234", "Anthropic"),
            ("AKIAABCDEFGHIJKLMNOP", "AWS"),
            ("token=glpat-abcdefghij0123456789", "GitLab"),
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIE", "private key"),
            (
                "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
                "web token",
            ),
        ] {
            assert!(credential_in(text).unwrap().contains(reason), "{text}");
        }
        assert_eq!(credential_in("sk-short"), None);
        // Ordinary identifiers that share a prefix are not credentials.
        for text in [
            "npm_config_registry_url_value_here",
            "hf_hub_download_from_the_cache_dir",
            "ASIA_PACIFIC_REGION_SETTINGS",
            "AKIA_is_a_prefix_only",
            "ASIAPACIFICREGION",
        ] {
            assert_eq!(credential_in(text), None, "{text}");
        }
        for (text, reason) in [
            // Built from parts so secret scanners do not flag the test file.
            (["sk_live_", "abcdefghijklmnop1234"].concat(), "Stripe"),
            (["ASIA", "ABCDEFGHIJKLMNOP"].concat(), "AWS"),
            (
                ["npm_", "abcdefghijklmnopqrstuvwxyz0123456789"].concat(),
                "npm",
            ),
            (
                ["hf_", "abcdefghijklmnopqrstuvwxyzABCDEFGH"].concat(),
                "Hugging Face",
            ),
        ] {
            assert!(credential_in(&text).unwrap().contains(reason), "{text}");
        }
    }

    #[test]
    fn a_key_split_over_json_escapes_is_still_found_in_the_strings() {
        let mut list = events();
        list.push(at(
            "t1",
            SessionEvent::ToolFinished {
                call_id: "c9".into(),
                ok: true,
                output: "cat .env\nTOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123\n".into(),
                tool: Some("run_command".into()),
                duration_ms: None,
            },
        ));
        let result = render("s1", "t", &list, ExportFormat::Json, true);
        assert!(result.warning.is_some());
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
