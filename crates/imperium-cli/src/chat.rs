//! `imperium chat`: an interactive session in the terminal.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Context;
use imperium_cli::client::Client;
use imperium_protocol::methods::{
    SessionAnswer, SessionAnswerParams, SessionCreate, SessionCreateParams, SessionRespond,
    SessionRespondParams, SessionSend, SessionSendParams,
};
use imperium_protocol::session::{
    AgentEvent, ContextCategory, PermissionDecision, Question, ROOT_AGENT, SESSION_EVENT,
    SessionEvent, SessionEventParams, StepStatus, TokenCountMethod,
};

use crate::{block_on, connect};

pub fn run(
    project: Option<PathBuf>,
    model: Option<String>,
    context_summary: bool,
) -> anyhow::Result<()> {
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
        println!(
            "Type a message. /context shows what the model is reading; an empty line or /exit quits."
        );
        let mut latest = None;
        loop {
            let Some(text) = prompt("\n> ")? else { break };
            let text = text.trim();
            if text.is_empty() || text == "/exit" {
                break;
            }
            if text == "/context" {
                match &latest {
                    Some(breakdown) => print!("{}", render_breakdown(breakdown)),
                    None => println!("No model call yet; send a message first."),
                }
                continue;
            }
            client
                .call::<SessionSend>(SessionSendParams {
                    session_id: session.session_id.clone(),
                    text: text.to_string(),
                })
                .await?;
            follow_turn(&mut client, &session.session_id, &mut latest).await?;
            if context_summary && let Some(breakdown) = &latest {
                println!("{}", summary_line(breakdown));
            }
        }
        Ok(())
    })
}

/// The root agent's latest context breakdown: what its last model call read.
struct Breakdown {
    model: String,
    context_window: Option<u64>,
    method: TokenCountMethod,
    categories: Vec<ContextCategory>,
    cached_tokens: u64,
}

impl Breakdown {
    /// Tokens the request held: the categories add up to the provider's count when it gave one.
    fn used(&self) -> i64 {
        self.categories.iter().map(|category| category.tokens).sum()
    }
}

/// A token count as `12.3k`, `1.2M`, or as is below a thousand.
fn short(tokens: i64) -> String {
    let magnitude = tokens.unsigned_abs() as f64;
    if magnitude >= 1e6 {
        format!("{:.1}M", tokens as f64 / 1e6)
    } else if magnitude >= 1e3 {
        format!("{:.1}k", tokens as f64 / 1e3)
    } else {
        tokens.to_string()
    }
}

fn method_words(method: TokenCountMethod) -> &'static str {
    match method {
        TokenCountMethod::Exact => "exact, counted with the model's tokenizer",
        TokenCountMethod::Calibrated => "calibrated to the provider's count",
        TokenCountMethod::Estimated => "estimated; the provider reported no usage",
    }
}

/// One line for after a turn: `[context: 12.3k / 32.8k tokens (38%), exact]`.
fn summary_line(breakdown: &Breakdown) -> String {
    let used = breakdown.used();
    let method = match breakdown.method {
        TokenCountMethod::Exact => "exact",
        TokenCountMethod::Calibrated => "calibrated",
        TokenCountMethod::Estimated => "estimated",
    };
    match breakdown.context_window {
        Some(window) if window > 0 => format!(
            "[context: {} / {} tokens ({:.0}%), {method}]",
            short(used),
            short(i64::try_from(window).unwrap_or(i64::MAX)),
            used as f64 * 100.0 / window as f64
        ),
        _ => format!(
            "[context: {} tokens, window unknown, {method}]",
            short(used)
        ),
    }
}

/// The breakdown as a bar (used against free) and a table of categories with their share of the
/// window.
fn render_breakdown(breakdown: &Breakdown) -> String {
    const WIDTH: usize = 40;
    let used = breakdown.used();
    let window = breakdown
        .context_window
        .filter(|window| *window > 0)
        .map(|window| i64::try_from(window).unwrap_or(i64::MAX));
    let mut out = format!(
        "\nContext of {}: {}",
        breakdown.model,
        summary_line(breakdown)
    );
    out.push('\n');
    if let Some(window) = window {
        let filled = usize::try_from((used.clamp(0, window) * WIDTH as i64) / window).unwrap_or(0);
        out.push_str(&format!(
            "[{}{}]\n",
            "█".repeat(filled),
            "·".repeat(WIDTH - filled)
        ));
    }
    let share = |tokens: i64| {
        window.map_or(String::new(), |window| {
            format!("{:>6.1}%", tokens as f64 * 100.0 / window as f64)
        })
    };
    for category in &breakdown.categories {
        out.push_str(&format!(
            "  {:<30}{:>10}{}\n",
            category.label,
            category.tokens,
            share(category.tokens)
        ));
    }
    if let Some(window) = window {
        let free = (window - used).max(0);
        out.push_str(&format!(
            "  {:<30}{:>10}{}\n",
            "Free space",
            free,
            share(free)
        ));
    }
    out.push_str(&format!(
        "  {} cached of the prompt; {}.\n",
        breakdown.cached_tokens,
        method_words(breakdown.method)
    ));
    out
}

/// Prints a turn's events until the root agent completes or fails, asking the user about
/// permissions. Subagent lines are prefixed with their task id. Keeps the root agent's latest
/// context breakdown in `latest`.
async fn follow_turn(
    client: &mut Client,
    session_id: &str,
    latest: &mut Option<Breakdown>,
) -> anyhow::Result<()> {
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
            SessionEvent::ContextBreakdown {
                model,
                context_window,
                method,
                categories,
                cached_tokens,
                ..
            } if root => {
                *latest = Some(Breakdown {
                    model,
                    context_window,
                    method,
                    categories,
                    cached_tokens,
                });
            }
            SessionEvent::ContextBreakdown { .. } => {}
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
            SessionEvent::QuestionsAsked {
                request_id,
                questions,
            } => {
                let answers = ask_questions(&tag, &questions)?;
                client
                    .call::<SessionAnswer>(SessionAnswerParams {
                        session_id: session_id.to_string(),
                        request_id,
                        answers,
                    })
                    .await?;
            }
            SessionEvent::QuestionsAnswered { .. } => {}
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

/// Asks a batch of questions. Enter takes the recommended answer, a number picks an option,
/// and anything else is the answer itself.
fn ask_questions(tag: &str, questions: &[Question]) -> anyhow::Result<Vec<String>> {
    println!(
        "
{tag}The agent has {} question(s):",
        questions.len()
    );
    let mut answers = Vec::new();
    for (index, question) in questions.iter().enumerate() {
        println!(
            "
{}. {}",
            index + 1,
            question.question
        );
        for (number, option) in question.options.iter().enumerate() {
            println!("   {}) {option}", number + 1);
        }
        let line = prompt(&format!("   [Enter = {}]: ", question.recommended))?;
        answers.push(choose_answer(question, line.as_deref().unwrap_or_default()));
    }
    Ok(answers)
}

fn choose_answer(question: &Question, input: &str) -> String {
    let input = input.trim();
    if input.is_empty() {
        return question.recommended.clone();
    }
    input
        .parse::<usize>()
        .ok()
        .and_then(|number| question.options.get(number.checked_sub(1)?))
        .cloned()
        .unwrap_or_else(|| input.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_default_to_the_recommendation_and_numbers_pick_options() {
        let question = Question {
            question: "Which database?".into(),
            options: vec!["sqlite".into(), "postgres".into()],
            recommended: "sqlite".into(),
        };
        assert_eq!(
            choose_answer(
                &question, "
"
            ),
            "sqlite"
        );
        assert_eq!(
            choose_answer(
                &question, "2
"
            ),
            "postgres"
        );
        assert_eq!(choose_answer(&question, "0"), "0");
        assert_eq!(choose_answer(&question, "9"), "9");
        assert_eq!(
            choose_answer(
                &question, "duckdb
"
            ),
            "duckdb"
        );
    }

    fn breakdown(window: Option<u64>) -> Breakdown {
        let category = |key: &str, label: &str, tokens| ContextCategory {
            key: key.into(),
            label: label.into(),
            tokens,
        };
        Breakdown {
            model: "llamacpp:q".into(),
            context_window: window,
            method: TokenCountMethod::Exact,
            categories: vec![
                category("system_base", "System prompt", 9_000),
                category("user", "User messages", 3_000),
                category("overhead", "Chat template and formatting", 288),
            ],
            cached_tokens: 0,
        }
    }

    #[test]
    fn the_summary_line_compares_use_with_the_window() {
        assert_eq!(
            summary_line(&breakdown(Some(32_768))),
            "[context: 12.3k / 32.8k tokens (38%), exact]"
        );
        assert_eq!(
            summary_line(&breakdown(None)),
            "[context: 12.3k tokens, window unknown, exact]"
        );
    }

    #[test]
    fn the_table_lists_every_category_and_the_free_space() {
        let text = render_breakdown(&breakdown(Some(32_768)));
        assert!(text.contains("System prompt"));
        assert!(text.contains("Chat template and formatting"));
        assert!(text.contains("Free space"));
        assert!(text.contains("20480"));
        assert!(text.contains("exact, counted with the model's tokenizer"));
        assert!(text.contains(&format!("[{}", "█".repeat(15))));
    }
}
