//! `imperium chat`: an interactive session in the terminal.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Context;
use imperium_cli::client::Client;
use imperium_protocol::methods::{
    SessionCreate, SessionCreateParams, SessionRespond, SessionRespondParams, SessionSend,
    SessionSendParams,
};
use imperium_protocol::session::{
    AgentEvent, PermissionDecision, ROOT_AGENT, SESSION_EVENT, SessionEvent, SessionEventParams,
    StepStatus,
};

use crate::{block_on, connect};

pub fn run(project: Option<PathBuf>, model: Option<String>) -> anyhow::Result<()> {
    let project = match project {
        Some(project) => project,
        None => std::env::current_dir().context("could not read the current directory")?,
    };
    let project = std::path::absolute(&project)
        .with_context(|| format!("project folder {}", project.display()))?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let session = client
            .call::<SessionCreate>(SessionCreateParams {
                project_dir: project.to_string_lossy().into_owned(),
                model,
            })
            .await?;
        println!(
            "Session {} with {} in {}",
            session.session_id,
            session.model,
            project.display()
        );
        println!("Type a message. An empty line or /exit quits.");
        loop {
            let Some(text) = prompt("\n> ")? else { break };
            let text = text.trim();
            if text.is_empty() || text == "/exit" {
                break;
            }
            client
                .call::<SessionSend>(SessionSendParams {
                    session_id: session.session_id.clone(),
                    text: text.to_string(),
                })
                .await?;
            follow_turn(&mut client, &session.session_id).await?;
        }
        Ok(())
    })
}

/// Prints a turn's events until the root agent completes or fails, asking the user about
/// permissions. Subagent lines are prefixed with their task id.
async fn follow_turn(client: &mut Client, session_id: &str) -> anyhow::Result<()> {
    loop {
        let notification = client.next_notification().await?;
        if notification.method != SESSION_EVENT {
            continue;
        }
        let params: SessionEventParams = serde_json::from_value(notification.params)
            .context("could not read a session event")?;
        if params.session_id != session_id {
            continue;
        }
        let AgentEvent { agent_id, event } = params.agent_event;
        let root = agent_id == ROOT_AGENT;
        let tag = if root {
            String::new()
        } else {
            format!("[{agent_id}] ")
        };
        match event {
            SessionEvent::TurnStarted { .. } | SessionEvent::ModelCallCompleted { .. } => {}
            // Subagent replies arrive as their task report; only the root streams to the terminal.
            SessionEvent::TextDelta { text } if root => {
                print!("{text}");
                std::io::stdout().flush().ok();
            }
            SessionEvent::TextDelta { .. } => {}
            SessionEvent::ToolStarted { summary, .. } => println!("\n{tag}· {summary}"),
            SessionEvent::PermissionRequested {
                request_id,
                summary,
                ..
            } => {
                let decision = ask_permission(&format!("{tag}{summary}"))?;
                client
                    .call::<SessionRespond>(SessionRespondParams {
                        session_id: session_id.to_string(),
                        request_id,
                        decision,
                    })
                    .await?;
            }
            SessionEvent::ToolFinished { ok, output, .. } => {
                if !ok {
                    println!("  {tag}✗ {}", output.lines().next().unwrap_or_default());
                }
            }
            SessionEvent::PlanUpdated { plan } => {
                println!("\n{tag}Plan: {}", plan.goal);
                for step in plan.steps {
                    let mark = match step.status {
                        StepStatus::Done => "✓",
                        StepStatus::InProgress => "▸",
                        StepStatus::Blocked => "!",
                        StepStatus::Skipped => "–",
                        StepStatus::Pending => " ",
                    };
                    println!("  [{mark}] {}", step.title);
                }
            }
            SessionEvent::TaskSpawned {
                task_id,
                brief,
                model,
            } => println!(
                "\n{tag}→ {task_id} on {model}: {}",
                brief.lines().next().unwrap_or_default()
            ),
            SessionEvent::TaskFinished {
                task_id,
                ok,
                cost_usd,
                ..
            } => {
                let cost = cost_usd.map_or(String::new(), |cost| format!(" (${cost:.4})"));
                let state = if ok { "finished" } else { "failed" };
                println!("\n← {task_id} {state}{cost}");
            }
            SessionEvent::TurnCompleted {
                usage,
                cost_usd,
                steps,
            } if root => {
                let cost = cost_usd.map_or(String::new(), |cost| format!(", ${cost:.4}"));
                println!(
                    "\n[{} in ({} cached) / {} out tokens{cost}, {steps} model calls]",
                    usage.prompt_tokens, usage.cached_tokens, usage.completion_tokens
                );
                return Ok(());
            }
            SessionEvent::TurnFailed { message } if root => {
                println!("\nturn failed: {message}");
                return Ok(());
            }
            SessionEvent::TurnCompleted { .. } => {}
            SessionEvent::TurnFailed { message } => println!("\n{tag}failed: {message}"),
            SessionEvent::Notice { message } => println!("\n{tag}note: {message}"),
            SessionEvent::MemoryUsed { names } => {
                println!("\n{tag}· recalled {}", names.join(", "));
            }
            SessionEvent::MemorySaved { name, scope } => {
                println!("\n{tag}· saved memory {name} ({scope})");
            }
            SessionEvent::ContextManifest {
                role,
                items,
                documentation,
            } => {
                let required = items.iter().filter(|item| item.required).count();
                println!(
                    "\n{tag}· context for {role}: {required} required, {} documentation pointers",
                    documentation.len()
                );
            }
            SessionEvent::Paused {
                until_unix_ms,
                reason,
            } => {
                let until = chrono::DateTime::from_timestamp_millis(
                    i64::try_from(until_unix_ms).unwrap_or(0),
                )
                .map(|time| {
                    time.with_timezone(&chrono::Local)
                        .format("%H:%M")
                        .to_string()
                })
                .unwrap_or_default();
                println!("\n{tag}· usage limit; waiting until {until} ({reason})");
            }
            SessionEvent::Resumed {} => println!("\n{tag}· resuming"),
            SessionEvent::StatusChanged { .. } => {}
            SessionEvent::MediaCreated { path, kind, .. } => {
                println!("\n{tag}· created {kind}: {path}");
            }
            SessionEvent::BrowserView { url, title, .. } => {
                println!("\n{tag}· browser: {title} ({url})");
            }
            SessionEvent::ContextLoaded { references } => {
                println!("\n{tag}· loaded {}", references.join(", "));
            }
        }
    }
}

fn ask_permission(summary: &str) -> anyhow::Result<PermissionDecision> {
    let question = format!("\nAllow: {summary}? [y]es / [a]lways this session / [n]o: ");
    let answer = prompt(&question)?.unwrap_or_default();
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => PermissionDecision::Allow,
        "a" | "always" => PermissionDecision::AllowForSession,
        _ => PermissionDecision::Deny,
    })
}

/// Prints a prompt and reads one line; `None` at end of input.
fn prompt(text: &str) -> anyhow::Result<Option<String>> {
    print!("{text}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let read = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("could not read from standard input")?;
    Ok((read > 0).then_some(line))
}
