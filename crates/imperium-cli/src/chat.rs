//! `imperium chat`: an interactive session in the terminal.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Context;
use imperium_cli::client::Client;
use imperium_protocol::methods::{
    ContextFit, SessionAgentResume, SessionAgentResumeParams, SessionAgents, SessionAgentsParams,
    SessionControl, SessionControlParams, SessionCreate, SessionCreateParams, SessionQueue,
    SessionQueueParams, SessionRespond, SessionRespondParams, SessionSend, SessionSendParams,
    SessionSuggestionRespond, SuggestionRespondParams,
};
use imperium_protocol::session::{
    AgentEvent, AgentStatus, AgentSummary, ContextCategory, PermissionDecision, QueuedMessage,
    ROOT_AGENT, SESSION_EVENT, SendMode, SessionEvent, SessionEventParams, StepStatus,
    TokenCountMethod,
};
use tokio::sync::mpsc;

use crate::{block_on, connect};

pub fn run(
    env: Option<String>,
    project: Option<PathBuf>,
    model: Option<String>,
    context_summary: bool,
    worktree: Option<Option<String>>,
) -> anyhow::Result<()> {
    let project = match (&env, project) {
        (Some(_), _) => None,
        (None, Some(project)) => Some(project),
        (None, None) => {
            Some(std::env::current_dir().context("could not read the current directory")?)
        }
    };
    let project = project
        .map(|project| {
            std::path::absolute(&project)
                .with_context(|| format!("project folder {}", project.display()))
        })
        .transpose()?;
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = match &env {
            Some(env) => Some(crate::env::resolve(&mut client, env).await?.id),
            None => None,
        };
        let session = client
            .call::<SessionCreate>(SessionCreateParams {
                environment_id,
                project_dir: project.map(|project| project.to_string_lossy().into_owned()),
                model,
                worktree: worktree.as_ref().map(|_| true),
                worktree_branch: worktree.flatten(),
            })
            .await?;
        let session_id = session.session_id.clone();
        for tree in &session.worktrees {
            println!(
                "Worktree {} on branch {} (for {})",
                tree.path, tree.branch, tree.folder
            );
        }
        println!(
            "Session {} with {} in {}",
            session.session_id,
            session
                .model
                .as_deref()
                .unwrap_or("no model yet (pick one with /model <id>)"),
            session.folders.join(", ")
        );
        println!(
            "Type a message. While the agent works, messages queue and run in order; /steer \
             <text> adds a message to the running turn, /queue lists what waits. /context shows \
             what the model is reading, /model <id> switches models, /compact summarises older \
             turns; /download, /dismiss, or /never answer a model download suggestion; an \
             empty line (when idle) or /exit quits."
        );
        let mut lines = read_lines();
        let mut chat = Chat {
            context_summary,
            ..Chat::default()
        };
        chat.prompt();
        loop {
            tokio::select! {
                line = lines.recv() => {
                    let Some(line) = line else { break };
                    if chat.on_line(&mut client, &session_id, line).await? == Flow::Quit {
                        break;
                    }
                }
                notification = client.next_notification() => {
                    let notification = notification?;
                    if notification.method != SESSION_EVENT {
                        continue;
                    }
                    let params: SessionEventParams = serde_json::from_value(notification.params)
                        .context("could not read a session event")?;
                    if params.session_id == session_id {
                        chat.on_event(params.agent_event);
                    }
                }
            }
        }
        Ok(())
    })
}

/// Reads standard input on its own thread, so the user can type while a turn runs. The
/// channel closes at end of input.
fn read_lines() -> mpsc::UnboundedReceiver<String> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

/// The terminal side of a chat: whether a turn runs, the permission questions waiting for an
/// answer, and the root agent's latest context breakdown.
#[derive(Default)]
struct Chat {
    context_summary: bool,
    busy: bool,
    /// Permission requests in the order asked; the next line answers the first.
    asking: VecDeque<(u64, String)>,
    /// The user just sent a message to an idle agent, so its `turn_started` needs no echo.
    sent_while_idle: bool,
    latest: Option<Breakdown>,
    /// A model download suggestion waiting for /download, /dismiss, or /never.
    suggestion: Option<String>,
}

impl Chat {
    fn prompt(&self) {
        if let Some((_, summary)) = self.asking.front() {
            print!("\nAllow: {summary}? [y]es / [a]lways this session / [n]o: ");
        } else if !self.busy {
            print!("\n> ");
        }
        std::io::stdout().flush().ok();
    }

    async fn on_line(
        &mut self,
        client: &mut Client,
        session_id: &str,
        line: String,
    ) -> anyhow::Result<Flow> {
        if let Some((request_id, _)) = self.asking.pop_front() {
            client
                .call::<SessionRespond>(SessionRespondParams {
                    session_id: session_id.to_string(),
                    request_id,
                    decision: permission_answer(&line),
                })
                .await?;
            self.prompt();
            return Ok(Flow::Continue);
        }
        let text = line.trim();
        if text == "/exit" || (text.is_empty() && !self.busy) {
            return Ok(Flow::Quit);
        }
        if text.is_empty() {
            return Ok(Flow::Continue);
        }
        match command(text) {
            Some(Command::Context) => match &self.latest {
                Some(breakdown) => print!("{}", render_breakdown(breakdown)),
                None => println!("No model call yet; send a message first."),
            },
            Some(Command::Queue) => {
                let queue = client
                    .call::<SessionQueue>(SessionQueueParams {
                        session_id: session_id.to_string(),
                        action: "list".into(),
                        agent_id: None,
                        id: None,
                        text: None,
                    })
                    .await?;
                print!("{}", describe_queue(&queue.messages));
            }
            Some(Command::Control(action)) => {
                let (action, model) = match action {
                    Control::SetModel(model) => ("set_model", Some(model)),
                    Control::Compact => ("compact", None),
                };
                let result = client
                    .call::<SessionControl>(SessionControlParams {
                        session_id: session_id.to_string(),
                        action: action.into(),
                        resume_policy: None,
                        repo: None,
                        pull_request: None,
                        model,
                        pinned: None,
                    })
                    .await;
                match result {
                    Ok(result) => println!(
                        "{}",
                        describe_fit(
                            action == "set_model",
                            result.model.as_deref().unwrap_or_default(),
                            result.context.as_ref()
                        )
                    ),
                    Err(error) => println!("{error}"),
                }
            }
            Some(Command::Agents) => {
                let result = client
                    .call::<SessionAgents>(SessionAgentsParams {
                        session_id: session_id.to_string(),
                    })
                    .await?;
                print!("{}", describe_agents(&result.agents));
            }
            Some(Command::ResumeAgent(agent_id, text)) => {
                let result = client
                    .call::<SessionAgentResume>(SessionAgentResumeParams {
                        session_id: session_id.to_string(),
                        agent_id,
                        text,
                    })
                    .await;
                if let Err(error) = result {
                    println!("{error}");
                }
            }
            Some(Command::Steer(text)) => self.send(client, session_id, text, true).await?,
            Some(Command::Suggestion(action)) => {
                self.answer_suggestion(client, session_id, action).await?
            }
            None => {
                self.send(client, session_id, text.to_string(), false)
                    .await?
            }
        }
        self.prompt();
        Ok(Flow::Continue)
    }

    /// Answers the waiting model download suggestion (`session.suggestion_respond`).
    async fn answer_suggestion(
        &mut self,
        client: &mut Client,
        session_id: &str,
        action: &'static str,
    ) -> anyhow::Result<()> {
        let Some(id) = self.suggestion.take() else {
            println!("No model suggestion is waiting.");
            return Ok(());
        };
        let result = client
            .call::<SessionSuggestionRespond>(SuggestionRespondParams {
                session_id: session_id.to_string(),
                suggestion_id: id.clone(),
                action: action.into(),
            })
            .await;
        match result {
            Ok(result) => match result.job {
                Some(job) => println!(
                    "Downloading {} {} ({}); you will be told when it is ready.",
                    job.repo, job.variant, job.job_id
                ),
                None if action == "never" => {
                    println!("Model download suggestions are off (models.suggest_downloads).")
                }
                None => println!("Not now."),
            },
            Err(error) => {
                println!("{error}");
                self.suggestion = Some(id);
            }
        }
        Ok(())
    }

    /// Sends a message: it starts a turn when the agent is idle and otherwise queues (or,
    /// steering, joins the running turn); the daemon's `message_queued` says which.
    async fn send(
        &mut self,
        client: &mut Client,
        session_id: &str,
        text: String,
        steer: bool,
    ) -> anyhow::Result<()> {
        client
            .call::<SessionSend>(SessionSendParams {
                session_id: session_id.to_string(),
                text,
                mode: steer.then_some(SendMode::Steer),
                agent_id: None,
            })
            .await?;
        if !self.busy {
            self.busy = true;
            self.sent_while_idle = true;
        }
        Ok(())
    }

    /// Prints one event. Subagent lines are prefixed with their task id.
    fn on_event(&mut self, AgentEvent { agent_id, event }: AgentEvent) {
        let root = agent_id == ROOT_AGENT;
        let tag = if root {
            String::new()
        } else {
            format!("[{agent_id}] ")
        };
        match event {
            SessionEvent::TurnStarted { text } if root => {
                self.busy = true;
                if !std::mem::take(&mut self.sent_while_idle) {
                    println!("\n» {}", text.lines().next().unwrap_or_default());
                }
            }
            SessionEvent::TurnStarted { .. } | SessionEvent::ModelCallCompleted { .. } => {}
            SessionEvent::ContextBreakdown {
                model,
                context_window,
                method,
                categories,
                cached_tokens,
                ..
            } if root => {
                self.latest = Some(Breakdown {
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
                self.asking
                    .push_back((request_id, format!("{tag}{summary}")));
                if self.asking.len() == 1 {
                    self.prompt();
                }
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
            SessionEvent::TaskResumed {
                task_id,
                text,
                model,
            } => println!(
                "\n{tag}→ {task_id} resumed on {model}: {}",
                text.lines().next().unwrap_or_default()
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
                if self.context_summary
                    && let Some(breakdown) = &self.latest
                {
                    println!("{}", summary_line(breakdown));
                }
                self.turn_ended();
            }
            SessionEvent::TurnFailed { message } if root => {
                println!("\nturn failed: {message}");
                self.turn_ended();
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
            SessionEvent::MessageQueued { id, text, steer } => {
                let first = text.lines().next().unwrap_or_default();
                if steer {
                    println!("\n{tag}· steering {id} joins the running turn: {first}");
                } else {
                    println!("\n{tag}· queued {id}: {first}");
                }
            }
            SessionEvent::Steered { id, .. } => println!("\n{tag}· steering {id} delivered"),
            SessionEvent::ModelSuggestion {
                id,
                repo,
                quant,
                size_bytes,
                fit,
                reason,
                ..
            } => {
                println!(
                    "\n{tag}· suggestion: download {repo} ({quant}, {:.1} GB, fits: {fit}) because \
                     {reason}.\n  /download to get it, /dismiss for not now, /never to stop \
                     suggesting downloads.",
                    size_bytes as f64 / 1e9
                );
                self.suggestion = Some(id);
            }
            SessionEvent::ModelSuggestionUpdate {
                state,
                model,
                message,
                ..
            } => match state.as_str() {
                "ready" => println!(
                    "\n{tag}· the suggested model is ready: /model {} switches to it",
                    model.unwrap_or_default()
                ),
                "failed" | "cancelled" => println!(
                    "\n{tag}· the suggested download {state}: {}",
                    message.unwrap_or_default()
                ),
                _ => {}
            },
            // This client prints the result of its own /model and /compact, and lists the
            // queue on /queue.
            SessionEvent::ModelChanged { .. }
            | SessionEvent::ContextCompacted { .. }
            | SessionEvent::QueueChanged { .. } => {}
        }
    }

    fn turn_ended(&mut self) {
        self.busy = false;
        self.sent_while_idle = false;
        self.asking.clear();
        self.prompt();
    }
}

/// A chat command that the client handles rather than sending as a message.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    /// `/context`: the latest context breakdown.
    Context,
    /// `/queue`: the messages waiting for the agent.
    Queue,
    /// `/steer <text>`: a message for the running turn.
    Steer(String),
    /// `/agents`: the session's agents and their kept state.
    Agents,
    /// `/resume-agent <id> <text>`: a follow-up for a finished subagent.
    ResumeAgent(String, String),
    /// `/download`, `/dismiss`, or `/never`: the answer to a model download suggestion, as
    /// `session.suggestion_respond` names it.
    Suggestion(&'static str),
    Control(Control),
}

fn command(text: &str) -> Option<Command> {
    match text {
        "/context" => return Some(Command::Context),
        "/queue" => return Some(Command::Queue),
        "/agents" => return Some(Command::Agents),
        "/download" => return Some(Command::Suggestion("download")),
        "/dismiss" => return Some(Command::Suggestion("dismiss")),
        "/never" => return Some(Command::Suggestion("never")),
        _ => {}
    }
    if let Some(steer) = text.strip_prefix("/steer ") {
        let steer = steer.trim();
        return (!steer.is_empty()).then(|| Command::Steer(steer.to_string()));
    }
    if let Some(rest) = text.strip_prefix("/resume-agent ") {
        let (agent_id, follow_up) = rest.trim().split_once(char::is_whitespace)?;
        let follow_up = follow_up.trim();
        return (!follow_up.is_empty())
            .then(|| Command::ResumeAgent(agent_id.to_string(), follow_up.to_string()));
    }
    control_command(text).map(Command::Control)
}

/// The agents as `/agents` prints them.
fn describe_agents(agents: &[AgentSummary]) -> String {
    let mut out = String::new();
    for agent in agents {
        let status = match agent.status {
            AgentStatus::Running => "running",
            AgentStatus::Idle => "idle",
            AgentStatus::Failed => "failed",
            AgentStatus::Interrupted => "interrupted",
        };
        let resumable = if agent.resumable && agent.status != AgentStatus::Running {
            ", resumable"
        } else {
            ""
        };
        let cost = agent
            .cost_usd
            .map_or(String::new(), |cost| format!(", ${cost:.4}"));
        let title = if agent.agent_id == ROOT_AGENT {
            "the session's agent"
        } else {
            agent.title.as_str()
        };
        out.push_str(&format!(
            "  {} ({status}{resumable}) {} · {} turns{cost}: {title}\n",
            agent.agent_id, agent.model, agent.turns
        ));
    }
    if agents.iter().any(|agent| agent.agent_id != ROOT_AGENT) {
        out.push_str("  /resume-agent <id> <text> gives a finished subagent a follow-up.\n");
    } else {
        out.push_str("  No subagents yet.\n");
    }
    out
}

/// The queue as `/queue` prints it.
fn describe_queue(messages: &[QueuedMessage]) -> String {
    if messages.is_empty() {
        return "Nothing is queued.\n".into();
    }
    let mut out = String::new();
    for message in messages {
        let kind = if message.steer { " (steering)" } else { "" };
        out.push_str(&format!(
            "  {}{kind}: {}\n",
            message.id,
            message.text.lines().next().unwrap_or_default()
        ));
    }
    out
}

/// A chat command that controls the session rather than messaging it.
#[derive(Debug, PartialEq, Eq)]
enum Control {
    /// `/model <id>`: switch the session's model.
    SetModel(String),
    /// `/compact`: summarise older turns now.
    Compact,
}

fn control_command(text: &str) -> Option<Control> {
    if text == "/compact" {
        return Some(Control::Compact);
    }
    let model = text.strip_prefix("/model ")?.trim();
    (!model.is_empty()).then(|| Control::SetModel(model.to_string()))
}

/// One line on a model switch or compaction: `Switched to x:y; compacted 30.1k -> 9.2k tokens
/// (12 messages summarised by x:z).`
fn describe_fit(switched: bool, model: &str, fit: Option<&ContextFit>) -> String {
    let head = if switched {
        format!("Switched to {model}")
    } else {
        format!("Context of {model}")
    };
    let Some(fit) = fit else {
        return format!("{head}.");
    };
    let tokens = |tokens: u64| short(i64::try_from(tokens).unwrap_or(i64::MAX));
    match &fit.compaction {
        Some(compaction) => format!(
            "{head}; compacted {} -> {} tokens ({} messages summarised by {}).",
            tokens(compaction.before_tokens),
            tokens(compaction.after_tokens),
            compaction.summarised_messages,
            compaction.summary_model
        ),
        None => match fit.budget {
            Some(budget) => format!(
                "{head}; the conversation ({} tokens) fits its budget of {} tokens{}.",
                tokens(fit.tokens),
                tokens(budget),
                if switched {
                    ""
                } else {
                    "; nothing older to compact"
                }
            ),
            None => format!(
                "{head}; its context window is unknown, so the conversation ({} tokens) was \
                 not checked.",
                tokens(fit.tokens)
            ),
        },
    }
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

/// The decision a typed answer to a permission question gives.
fn permission_answer(answer: &str) -> PermissionDecision {
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => PermissionDecision::Allow,
        "a" | "always" => PermissionDecision::AllowForSession,
        _ => PermissionDecision::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn suggestion_answers_are_commands() {
        assert_eq!(command("/download"), Some(Command::Suggestion("download")));
        assert_eq!(command("/never"), Some(Command::Suggestion("never")));
        assert_ne!(command("/dismiss"), None);
    }

    #[test]
    fn model_and_compact_commands_are_recognised() {
        assert_eq!(
            control_command("/model hosted:big"),
            Some(Control::SetModel("hosted:big".into()))
        );
        assert_eq!(control_command("/compact"), Some(Control::Compact));
        assert_eq!(control_command("/model "), None);
        assert_eq!(control_command("tell me about /model x"), None);
    }

    #[test]
    fn steer_and_queue_commands_are_recognised() {
        assert_eq!(
            command("/steer use tabs"),
            Some(Command::Steer("use tabs".into()))
        );
        assert_eq!(command("/steer   "), None);
        assert_eq!(command("/queue"), Some(Command::Queue));
        assert_eq!(command("/agents"), Some(Command::Agents));
        assert_eq!(
            command("/resume-agent t2  now check the tests"),
            Some(Command::ResumeAgent(
                "t2".into(),
                "now check the tests".into()
            ))
        );
        assert_eq!(command("/resume-agent t2"), None);
        assert_eq!(command("/resume-agent t2   "), None);
        assert_eq!(command("/context"), Some(Command::Context));
        assert_eq!(
            command("/compact"),
            Some(Command::Control(Control::Compact))
        );
        assert_eq!(command("please /steer this"), None);
    }

    #[test]
    fn the_queue_lists_ids_and_marks_steering() {
        assert_eq!(describe_queue(&[]), "Nothing is queued.\n");
        let queue = [
            QueuedMessage {
                id: "q1".into(),
                text: "then add tests\nmore".into(),
                steer: false,
            },
            QueuedMessage {
                id: "q2".into(),
                text: "use tabs".into(),
                steer: true,
            },
        ];
        assert_eq!(
            describe_queue(&queue),
            "  q1: then add tests\n  q2 (steering): use tabs\n"
        );
        assert_eq!(
            permission_answer(" A "),
            PermissionDecision::AllowForSession
        );
        assert_eq!(permission_answer(""), PermissionDecision::Deny);
    }

    #[test]
    fn switches_report_their_compaction() {
        let fit = ContextFit {
            tokens: 9_200,
            context_window: Some(16_384),
            budget: Some(13_926),
            method: TokenCountMethod::Exact,
            compaction: Some(imperium_protocol::methods::Compaction {
                before_tokens: 30_100,
                after_tokens: 9_200,
                summarised_messages: 12,
                summary_model: "hosted:big".into(),
            }),
        };
        assert_eq!(
            describe_fit(true, "local:small", Some(&fit)),
            "Switched to local:small; compacted 30.1k -> 9.2k tokens (12 messages summarised by \
             hosted:big)."
        );
        let unchanged = ContextFit {
            compaction: None,
            ..fit
        };
        assert!(describe_fit(true, "local:small", Some(&unchanged)).contains("fits its budget"));
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
