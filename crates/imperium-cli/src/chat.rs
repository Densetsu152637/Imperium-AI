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
    PermissionDecision, SESSION_EVENT, SessionEvent, SessionEventParams,
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

/// Prints a turn's events until it completes or fails, asking the user about permissions.
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
        match params.event {
            SessionEvent::TurnStarted { .. } | SessionEvent::ModelCallCompleted { .. } => {}
            SessionEvent::TextDelta { text } => {
                print!("{text}");
                std::io::stdout().flush().ok();
            }
            SessionEvent::ToolStarted { summary, .. } => println!("\n· {summary}"),
            SessionEvent::PermissionRequested {
                request_id,
                summary,
                ..
            } => {
                let decision = ask_permission(&summary)?;
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
                    println!("  ✗ {}", output.lines().next().unwrap_or_default());
                }
            }
            SessionEvent::TurnCompleted {
                usage,
                cost_usd,
                steps,
            } => {
                let cost = cost_usd.map_or(String::new(), |cost| format!(", ${cost:.4}"));
                println!(
                    "\n[{} in ({} cached) / {} out tokens{cost}, {steps} model calls]",
                    usage.prompt_tokens, usage.cached_tokens, usage.completion_tokens
                );
                return Ok(());
            }
            SessionEvent::TurnFailed { message } => {
                println!("\nturn failed: {message}");
                return Ok(());
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
