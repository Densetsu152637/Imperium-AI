//! `imperium chat`: an interactive session in the terminal.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Context;
use imperium_cli::client::Client;
use imperium_protocol::methods::{
    ContextFit, MODEL_RESIDENCY, ModelResidency, RewindMode, SessionAgentResume,
    SessionAgentResumeParams, SessionAgents, SessionAgentsParams, SessionAnswer,
    SessionAnswerParams, SessionBranch, SessionBranchParams, SessionCheckpoints,
    SessionCheckpointsParams, SessionCheckpointsResult, SessionCompare, SessionCompareParams,
    SessionComparePick, SessionComparePickParams, SessionControl, SessionControlParams,
    SessionCreate, SessionCreateParams, SessionQueue, SessionQueueParams, SessionRespond,
    SessionRespondParams, SessionReview, SessionReviewParams, SessionReviewResult, SessionRewind,
    SessionRewindParams, SessionRewindResult, SessionSend, SessionSendParams, SessionSubscribe,
    SessionSubscribeParams, SessionSuggestionRespond, SuggestionRespondParams,
};
use imperium_protocol::session::{
    AgentEvent, AgentStatus, AgentSummary, ContextCategory, ExtensionKind, LocalWaitReason,
    PauseCause, PermissionDecision, Question, QueuedMessage, ROOT_AGENT, SESSION_EVENT, SendMode,
    SessionEvent, SessionEventParams, StepStatus, TokenCountMethod,
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
    // With neither --env nor --project the session starts in the built-in Home environment,
    // which has no project folders to put on a worktree.
    if worktree.is_some() && env.is_none() && project.is_none() {
        anyhow::bail!(
            "--worktree needs a project: give --project <dir> (or --env <name>); without one \
             the session starts in Home, which has no project folders"
        );
    }
    let project = match (&env, project) {
        (Some(_), _) => None,
        (None, project) => project,
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
        let mut session_id = session.session_id.clone();
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
        if session.environment_id.as_deref() == Some("home") {
            println!(
                "This is the Home environment: no project folders, only the scratch folder above. \
                 To work on a project, start with --project <dir> (or --project . for the current \
                 directory) or --env <name>."
            );
        }
        println!(
            "Type a message. While the agent works, messages queue and run in order; /steer \
             <text> adds a message to the running turn, /queue lists what waits. /context shows \
             what the model is reading, /model <id> switches models, /effort <low|medium|high|max> \
             sets how hard a reasoning model thinks, /mode <ask|default|accept_edits|plan|full> \
             sets how actions are approved, /compact summarises older turns, /rewind lists \
             checkpoints and puts files and the conversation back to one, /diff [n] shows what \
             the session changed (since checkpoint n); /branch continues in a copy of this \
             conversation, /compare <model> <model> -- <message> runs a message on several \
             models and /pick <n> keeps one; /download, /dismiss, or /never answer a \
             model download suggestion; an \
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
                    match chat.on_line(&mut client, &session_id, line).await? {
                        Flow::Quit => break,
                        Flow::Switch(next) => session_id = next,
                        Flow::Continue => {}
                    }
                }
                notification = client.next_notification() => {
                    let notification = notification?;
                    if notification.method == MODEL_RESIDENCY {
                        if let Ok(moved) = serde_json::from_value::<ModelResidency>(notification.params) {
                            println!("\n{}", dim(&residency_line(&moved)));
                        }
                        continue;
                    }
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

/// Something the agent waits on the user for; typed lines answer it.
enum Asking {
    Permission {
        request_id: u64,
        summary: String,
    },
    /// An `ask_user` batch, answered one question per line.
    Questions {
        request_id: u64,
        tag: String,
        questions: Vec<Question>,
        answers: Vec<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
    /// Go on in another session (a conversation rewound into a fork).
    Switch(String),
}

/// The terminal side of a chat: whether a turn runs, the permission questions waiting for an
/// answer, and the root agent's latest context breakdown.
#[derive(Default)]
struct Chat {
    context_summary: bool,
    busy: bool,
    /// Permission requests and question batches in the order asked; the next line answers the
    /// first.
    asking: VecDeque<Asking>,
    /// The user just sent a message to an idle agent, so its `turn_started` needs no echo.
    sent_while_idle: bool,
    latest: Option<Breakdown>,
    /// A model download suggestion waiting for /download, /dismiss, or /never.
    suggestion: Option<String>,
    /// The latest `/compare`: its id and the sessions it started, for `/pick`.
    comparison: Option<(String, Vec<imperium_protocol::methods::CompareBranch>)>,
}

impl Chat {
    fn prompt(&self) {
        match self.asking.front() {
            Some(Asking::Permission { summary, .. }) => {
                print!("\nAllow: {summary}? [y]es / [a]lways this session / [n]o: ");
            }
            Some(Asking::Questions {
                tag,
                questions,
                answers,
                ..
            }) => {
                if answers.is_empty() {
                    println!("\n{tag}The agent has {} question(s):", questions.len());
                }
                if let Some(question) = questions.get(answers.len()) {
                    println!("\n{}. {}", answers.len() + 1, question.question);
                    for (number, option) in question.options.iter().enumerate() {
                        println!("   {}) {option}", number + 1);
                    }
                    print!("   [Enter = {}]: ", question.recommended);
                }
            }
            None if !self.busy => print!("\n> "),
            None => {}
        }
        std::io::stdout().flush().ok();
    }

    async fn on_line(
        &mut self,
        client: &mut Client,
        session_id: &str,
        line: String,
    ) -> anyhow::Result<Flow> {
        match self.asking.front_mut() {
            Some(Asking::Permission { request_id, .. }) => {
                let request_id = *request_id;
                self.asking.pop_front();
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
            Some(Asking::Questions {
                request_id,
                questions,
                answers,
                ..
            }) => {
                if let Some(question) = questions.get(answers.len()) {
                    answers.push(choose_answer(question, &line));
                }
                if answers.len() >= questions.len() {
                    let (request_id, answers) = (*request_id, std::mem::take(answers));
                    self.asking.pop_front();
                    client
                        .call::<SessionAnswer>(SessionAnswerParams {
                            session_id: session_id.to_string(),
                            request_id,
                            answers,
                        })
                        .await?;
                }
                self.prompt();
                return Ok(Flow::Continue);
            }
            None => {}
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
                let (action, model, effort, permission_mode) = match action {
                    Control::SetModel(model) => ("set_model", Some(model), None, None),
                    Control::SetEffort(effort) => ("set_effort", None, Some(effort), None),
                    Control::SetMode(mode) => ("set_permission_mode", None, None, Some(mode)),
                    Control::Compact => ("compact", None, None, None),
                };
                let result = client
                    .call::<SessionControl>(SessionControlParams {
                        session_id: session_id.to_string(),
                        action: action.into(),
                        resume_policy: None,
                        repo: None,
                        pull_request: None,
                        model,
                        effort,
                        permission_mode,
                        pinned: None,
                        title: None,
                    })
                    .await;
                match result {
                    Ok(result) if action == "set_permission_mode" => println!(
                        "{}",
                        describe_mode(result.permission_mode.as_deref().unwrap_or_default())
                    ),
                    Ok(result) if action == "set_effort" => println!(
                        "{}",
                        describe_effort(
                            result.effort.as_deref().unwrap_or_default(),
                            result.reasoning.unwrap_or(false)
                        )
                    ),
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
            Some(Command::Diff(since)) => {
                let result = client
                    .call::<SessionReview>(SessionReviewParams {
                        session_id: session_id.to_string(),
                        since,
                    })
                    .await;
                match result {
                    Ok(review) => print!("{}", describe_review(&review)),
                    Err(error) => println!("{error}"),
                }
            }
            Some(Command::Rewind(None)) => {
                let result = client
                    .call::<SessionCheckpoints>(SessionCheckpointsParams {
                        session_id: session_id.to_string(),
                    })
                    .await;
                match result {
                    Ok(result) => print!("{}", describe_checkpoints(&result)),
                    Err(error) => println!("{error}"),
                }
            }
            Some(Command::Rewind(Some((checkpoint, mode)))) => {
                let result = client
                    .call::<SessionRewind>(SessionRewindParams {
                        session_id: session_id.to_string(),
                        checkpoint,
                        mode,
                    })
                    .await;
                match result {
                    Ok(result) => {
                        print!("{}", describe_rewind(&result));
                        if let Some(forked) = result.session_id {
                            client
                                .call::<SessionSubscribe>(SessionSubscribeParams {
                                    session_id: forked.clone(),
                                })
                                .await?;
                            self.prompt();
                            return Ok(Flow::Switch(forked));
                        }
                    }
                    Err(error) => println!("{error}"),
                }
            }
            Some(Command::Branch(title)) => {
                let result = client
                    .call::<SessionBranch>(SessionBranchParams {
                        session_id: session_id.to_string(),
                        at: None,
                        title,
                        worktree: false,
                    })
                    .await;
                match result {
                    Ok(result) => {
                        println!(
                            "Branched into session {}; continuing there. The original is kept.",
                            result.session_id
                        );
                        client
                            .call::<SessionSubscribe>(SessionSubscribeParams {
                                session_id: result.session_id.clone(),
                            })
                            .await?;
                        self.prompt();
                        return Ok(Flow::Switch(result.session_id));
                    }
                    Err(error) => println!("{error}"),
                }
            }
            Some(Command::Compare(models, message)) => {
                let result = client
                    .call::<SessionCompare>(SessionCompareParams {
                        session_id: session_id.to_string(),
                        message,
                        models,
                        worktree: None,
                    })
                    .await;
                match result {
                    Ok(result) => {
                        println!("Comparing on {} branches:", result.branches.len());
                        for (number, branch) in result.branches.iter().enumerate() {
                            println!(
                                "  {} {} (session {})",
                                number + 1,
                                branch.model,
                                branch.session_id
                            );
                        }
                        println!("/pick <n> keeps one and archives the rest.");
                        self.comparison = Some((result.comparison_id, result.branches));
                    }
                    Err(error) => println!("{error}"),
                }
            }
            Some(Command::Usage(usage)) => println!("{usage}"),
            Some(Command::Pick(number)) => {
                let Some((comparison_id, branches)) = self.comparison.clone() else {
                    println!("No comparison to pick from: start one with /compare.");
                    self.prompt();
                    return Ok(Flow::Continue);
                };
                let Some(branch) = number.checked_sub(1).and_then(|at| branches.get(at)) else {
                    println!("Pick a number from 1 to {}.", branches.len());
                    self.prompt();
                    return Ok(Flow::Continue);
                };
                let result = client
                    .call::<SessionComparePick>(SessionComparePickParams {
                        comparison_id,
                        session_id: branch.session_id.clone(),
                    })
                    .await;
                match result {
                    Ok(result) => {
                        println!(
                            "Kept session {}; archived {}.",
                            result.kept,
                            result.archived.len()
                        );
                        self.comparison = None;
                        client
                            .call::<SessionSubscribe>(SessionSubscribeParams {
                                session_id: result.kept.clone(),
                            })
                            .await?;
                        self.prompt();
                        return Ok(Flow::Switch(result.kept));
                    }
                    Err(error) => println!("{error}"),
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
                confirm_budget: None,
                attachments: Vec::new(),
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
            SessionEvent::TurnStarted { text, .. } if root => {
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
                self.asking.push_back(Asking::Permission {
                    request_id,
                    summary: format!("{tag}{summary}"),
                });
                if self.asking.len() == 1 {
                    self.prompt();
                }
            }
            SessionEvent::QuestionsAsked {
                request_id,
                questions,
            } => {
                self.asking.push_back(Asking::Questions {
                    request_id,
                    tag,
                    questions,
                    answers: Vec::new(),
                });
                if self.asking.len() == 1 {
                    self.prompt();
                }
            }
            // Answered from another client: stop asking.
            SessionEvent::QuestionsAnswered { request_id } => {
                let was_first = matches!(
                    self.asking.front(),
                    Some(Asking::Questions { request_id: id, .. }) if *id == request_id
                );
                self.asking.retain(|asking| {
                    !matches!(asking, Asking::Questions { request_id: id, .. } if *id == request_id)
                });
                if was_first {
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
                title,
                ..
            } => println!(
                "\n{tag}→ {task_id} on {model}: {}",
                if title.is_empty() {
                    brief.lines().next().unwrap_or_default()
                } else {
                    title.as_str()
                }
            ),
            SessionEvent::TaskResumed {
                task_id,
                text,
                model,
                ..
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
            SessionEvent::TaskPaused { task_id, .. } => {
                println!("\n‖ {task_id} paused; /resume-agent continues it");
            }
            SessionEvent::WorkStopped {} => {
                println!("\n‖ session stopped; every agent halts at its next safe point");
            }
            SessionEvent::WorkResumed {} => println!("\n▶ session resumed"),
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
                reason,
                cause: PauseCause::Load | PauseCause::Deferred,
                ..
            } => println!(
                "
{tag}· background work waiting: {reason}"
            ),
            SessionEvent::Paused {
                until_unix_ms,
                reason,
                ..
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
            SessionEvent::LocalModelWaiting { model, reason, .. } => {
                let why = match reason {
                    LocalWaitReason::Loading => "loading it",
                    LocalWaitReason::GrowingPool => "growing its KV cache",
                    LocalWaitReason::KvRoom => "waiting for room in its KV cache",
                    LocalWaitReason::Slot => "waiting for a free slot",
                    LocalWaitReason::OtherModel => "waiting for another model",
                    LocalWaitReason::MakingRoom => "making room on the GPU",
                    LocalWaitReason::SavingContext => "saving the context of a model swapped out",
                    LocalWaitReason::RestoringContext => "restoring its context",
                };
                println!("\n{tag}· {model}: {why}");
            }
            SessionEvent::StatusChanged { .. } => {}
            SessionEvent::MediaCreated { path, kind, .. } => {
                println!("\n{tag}· created {kind}: {path}");
            }
            SessionEvent::BrowserView { url, title, .. } => {
                println!("\n{tag}· browser: {title} ({url})");
            }
            SessionEvent::BrowserControl { user } => {
                let who = if user {
                    "you drive"
                } else {
                    "the agent drives"
                };
                println!("\n{tag}· browser: {who}");
            }
            SessionEvent::PreviewChanged { url, .. } => match url {
                Some(url) => println!("\n{tag}· preview: {url}"),
                None => println!("\n{tag}· preview closed"),
            },
            SessionEvent::ContextLoaded { references } => {
                println!("\n{tag}· loaded {}", references.join(", "));
            }
            SessionEvent::ExtensionLoaded { kind, name, tools } => match kind {
                ExtensionKind::Skill => println!("\n{tag}· loaded skill {name}"),
                ExtensionKind::Plugin => {
                    println!("\n{tag}· activated plugin {name} ({} tools)", tools.len());
                }
            },
            SessionEvent::MessageQueued { id, text, steer } => {
                let first = text.lines().next().unwrap_or_default();
                if steer {
                    println!("\n{tag}· steering {id} joins the running turn: {first}");
                } else {
                    println!("\n{tag}· queued {id}: {first}");
                }
            }
            SessionEvent::Steered { id, paused, .. } => {
                if paused.is_empty() {
                    println!("\n{tag}· steering {id} delivered");
                } else {
                    println!(
                        "\n{tag}· steering {id} delivered; paused {}",
                        paused.join(", ")
                    );
                }
            }
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
            SessionEvent::CompactionStarted { model } => {
                println!("\n{tag}· compacting the conversation for {model}");
            }
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
    /// `/rewind`: the session's checkpoints. `/rewind <n> [files|conversation|both]`: rewinds
    /// to checkpoint `n` (both by default).
    Rewind(Option<(u32, RewindMode)>),
    /// `/branch [title]`: continue in a copy of the conversation as it is now.
    Branch(Option<String>),
    /// `/compare <model> <model> [...] -- <message>`: the message on each model.
    Compare(Vec<String>, String),
    /// `/pick <n>`: keep branch `n` of the latest comparison.
    Pick(usize),
    /// A command typed in the wrong shape: its usage, printed instead of sending the line.
    Usage(&'static str),
    /// `/diff [n]`: what the session changed since its first checkpoint (or checkpoint `n`).
    Diff(Option<u32>),
}

const COMPARE_USAGE: &str =
    "Usage: /compare <model> <model> [<model> <model>] -- <message> (two to four models)";
const PICK_USAGE: &str = "Usage: /pick <n> (the number of a branch from the latest /compare)";

/// `/compare <model> <model> [...] -- <message>`: two to four models, then the message.
fn compare_command(rest: &str) -> Option<Command> {
    let (models, message) = rest.split_once("--")?;
    let models: Vec<String> = models.split_whitespace().map(str::to_string).collect();
    let message = message.trim();
    ((2..=4).contains(&models.len()) && !message.is_empty())
        .then(|| Command::Compare(models, message.to_string()))
}

/// `/rewind`, `/rewind <n>`, or `/rewind <n> <mode>`; `None` when malformed.
fn rewind_command(rest: &str) -> Option<Option<(u32, RewindMode)>> {
    let mut words = rest.split_whitespace();
    let Some(number) = words.next() else {
        return Some(None);
    };
    let number = number.trim_start_matches('#').parse().ok()?;
    let mode = match words.next() {
        None | Some("both") => RewindMode::Both,
        Some("files") => RewindMode::Files,
        Some("conversation") => RewindMode::Conversation,
        Some(_) => return None,
    };
    words.next().is_none().then_some(Some((number, mode)))
}

/// The checkpoints as `/rewind` lists them.
fn describe_checkpoints(result: &SessionCheckpointsResult) -> String {
    if !result.available {
        return format!(
            "No checkpoints: {}.\n",
            result.reason.as_deref().unwrap_or("not available")
        );
    }
    if result.checkpoints.is_empty() {
        return "No checkpoints yet: one is recorded before each turn that changes files.\n".into();
    }
    let mut out = String::new();
    for checkpoint in &result.checkpoints {
        let what = if checkpoint.kind == "rewind" {
            "(the files before a rewind)".to_string()
        } else {
            let line = checkpoint.prompt.lines().next().unwrap_or_default();
            let mut shown: String = line.chars().take(70).collect();
            if shown.len() < line.len() {
                shown.push('…');
            }
            format!("before \"{shown}\"")
        };
        out.push_str(&format!("  #{} {what}\n", checkpoint.number));
    }
    out.push_str(
        "/rewind <n> puts the files and the conversation back (the conversation in a new \
         session); add files or conversation for one of them.\n",
    );
    out
}

/// What a rewind did, as `/rewind <n>` reports it.
fn describe_rewind(result: &SessionRewindResult) -> String {
    let mut out = String::new();
    if let Some(undo) = result.undo_checkpoint {
        out.push_str(&format!(
            "Files rewound: {} restored, {} removed. /rewind {undo} files undoes it.\n",
            result.restored.len(),
            result.removed.len()
        ));
    }
    if let Some(session) = &result.session_id {
        out.push_str(&format!(
            "Conversation rewound into session {session}; continuing there.\n"
        ));
        if let Some(prompt) = result.prompt.as_deref().filter(|p| !p.is_empty()) {
            out.push_str(&format!("The rewound message was: {prompt}\n"));
        }
    }
    out
}

/// The session's changes as `/diff` prints them: a unified diff per file.
fn describe_review(review: &SessionReviewResult) -> String {
    if !review.available {
        return format!(
            "No review: {}.\n",
            review.reason.as_deref().unwrap_or("not available")
        );
    }
    if review.files.is_empty() {
        return "The session has changed no files since its first checkpoint.\n".into();
    }
    let several = review.repos.len() > 1;
    let mut out = String::new();
    for file in &review.files {
        let shown = if several {
            format!("{}/{}", file.repo, file.path)
        } else {
            file.path.clone()
        };
        let (old, new) = match file.status.as_str() {
            "added" => ("/dev/null".to_string(), format!("b/{shown}")),
            "deleted" => (format!("a/{shown}"), "/dev/null".to_string()),
            _ => (format!("a/{shown}"), format!("b/{shown}")),
        };
        out.push_str(&format!("--- {old}\n+++ {new}\n"));
        if file.binary {
            out.push_str("Binary or large file changed\n");
        }
        for hunk in &file.hunks {
            out.push_str(&hunk.header);
            out.push('\n');
            for line in &hunk.lines {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

fn command(text: &str) -> Option<Command> {
    if text == "/branch" || text.starts_with("/branch ") {
        let title = text["/branch".len()..].trim();
        return Some(Command::Branch(
            (!title.is_empty()).then(|| title.to_string()),
        ));
    }
    if text == "/compare" || text.starts_with("/compare ") {
        return Some(
            compare_command(&text["/compare".len()..]).unwrap_or(Command::Usage(COMPARE_USAGE)),
        );
    }
    if text == "/pick" || text.starts_with("/pick ") {
        return Some(
            text["/pick".len()..]
                .trim()
                .trim_start_matches('#')
                .parse()
                .ok()
                .map_or(Command::Usage(PICK_USAGE), Command::Pick),
        );
    }
    if text == "/diff" {
        return Some(Command::Diff(None));
    }
    if let Some(since) = text.strip_prefix("/diff ") {
        return since
            .trim()
            .trim_start_matches('#')
            .parse()
            .ok()
            .map(|since| Command::Diff(Some(since)));
    }
    if text == "/rewind" || text.starts_with("/rewind ") {
        return rewind_command(&text["/rewind".len()..]).map(Command::Rewind);
    }
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
            AgentStatus::Paused => "paused",
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
    /// `/effort <level>`: how hard a model that can reason thinks.
    SetEffort(String),
    /// `/mode <mode>`: how the session's actions are approved.
    SetMode(String),
    /// `/compact`: summarise older turns now.
    Compact,
}

fn control_command(text: &str) -> Option<Control> {
    if text == "/compact" {
        return Some(Control::Compact);
    }
    if let Some(effort) = text.strip_prefix("/effort ") {
        let effort = effort.trim();
        return (!effort.is_empty()).then(|| Control::SetEffort(effort.to_ascii_lowercase()));
    }
    if let Some(mode) = text.strip_prefix("/mode ") {
        let mode = mode.trim();
        return (!mode.is_empty()).then(|| Control::SetMode(mode.to_ascii_lowercase()));
    }
    let model = text.strip_prefix("/model ")?.trim();
    (!model.is_empty()).then(|| Control::SetModel(model.to_string()))
}

/// One line on an effort change, saying when the model cannot use it.
fn describe_effort(effort: &str, reasoning: bool) -> String {
    if reasoning {
        format!("Reasoning effort set to {effort}.")
    } else {
        format!(
            "Reasoning effort set to {effort}; the session's model cannot reason, so it applies \
             once the session is on one that can."
        )
    }
}

/// One line on a permission-mode change, saying what the mode does.
fn describe_mode(mode: &str) -> String {
    let what = match mode {
        "ask" => "every write, edit, and command asks, whatever the allow rules say",
        "accept_edits" => "file edits run without asking; commands follow the rules",
        "plan" => "agents only read and plan; writes and commands are refused",
        "full" => "everything runs without asking except what deny rules or the safety floor stop",
        _ => "writes, edits, and commands ask unless a rule allows them",
    };
    format!("Permission mode set to {mode}: {what}.")
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
            // A switch is estimated: the model is loaded and the conversation fitted to it when
            // the next message is sent.
            Some(budget) if switched && fit.tokens > budget => format!(
                "{head}; the conversation (about {} tokens) is over its budget of {} tokens, so \
                 older turns are compacted when you send the next message.",
                tokens(fit.tokens),
                tokens(budget)
            ),
            Some(budget) if switched => format!(
                "{head}; the conversation (about {} tokens) fits its budget of {} tokens.",
                tokens(fit.tokens),
                tokens(budget)
            ),
            Some(budget) => format!(
                "{head}; the conversation ({} tokens) fits its budget of {} tokens; nothing \
                 older to compact.",
                tokens(fit.tokens),
                tokens(budget)
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

/// One `model.residency` move on one line: what moved, and why.
fn residency_line(moved: &ModelResidency) -> String {
    let model = moved.model_id.as_deref().unwrap_or("?");
    let holder = moved.holder.as_deref().unwrap_or("an agent");
    // What was done, what would have been, and to what.
    let (done, verb, what) = match moved.action.as_str() {
        "load" => ("loaded", "load", model.to_string()),
        "unload" => ("unloaded", "unload", model.to_string()),
        "save_slot" => ("saved", "save", format!("{holder}'s context on {model}")),
        "restore_slot" => (
            "restored",
            "restore",
            format!("{holder}'s context on {model}"),
        ),
        "resize_pool" => (
            "restarted",
            "restart",
            format!("{model} with a larger KV pool"),
        ),
        "set_budget" => (
            "set",
            "set",
            format!(
                "the memory budget to {} MiB of VRAM",
                moved.bytes / (1 << 20)
            ),
        ),
        other => (other, other, model.to_string()),
    };
    let reason = &moved.reason;
    let detail = moved
        .detail
        .as_deref()
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default();
    if moved.ok {
        format!("· {done} {what} ({reason}){detail}")
    } else {
        format!("· could not {verb} {what} ({reason}){detail}")
    }
}

/// `text` dimmed when standard output is a terminal.
fn dim(text: &str) -> String {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        format!("\x1b[2m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// The decision a typed answer to a permission question gives.
fn permission_answer(answer: &str) -> PermissionDecision {
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => PermissionDecision::Allow,
        "a" | "always" => PermissionDecision::AllowForSession,
        _ => PermissionDecision::Deny,
    }
}

/// The answer a typed line gives a question: Enter takes the recommended answer, a number
/// picks an option, and anything else is the answer itself.
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

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_protocol::methods::{ReviewFile, ReviewHunk};

    #[test]
    fn residency_moves_print_what_moved_and_why() {
        let moved = ModelResidency {
            action: "unload".into(),
            model_id: Some("qwen".into()),
            holder: None,
            reason: "crashed".into(),
            ok: true,
            detail: Some("the server exited (exit code: 1)".into()),
            seconds: 0.0,
            bytes: 0,
        };
        assert_eq!(
            residency_line(&moved),
            "· unloaded qwen (crashed): the server exited (exit code: 1)"
        );
        let restore = ModelResidency {
            action: "restore_slot".into(),
            holder: Some("s/root".into()),
            reason: "resume".into(),
            ok: false,
            detail: Some("no file".into()),
            ..moved
        };
        assert_eq!(
            residency_line(&restore),
            "· could not restore s/root's context on qwen (resume): no file"
        );
    }

    #[test]
    fn answers_default_to_the_recommendation_and_numbers_pick_options() {
        let question = Question {
            question: "Which database?".into(),
            options: vec!["sqlite".into(), "postgres".into()],
            recommended: "sqlite".into(),
        };
        assert_eq!(choose_answer(&question, "\n"), "sqlite");
        assert_eq!(choose_answer(&question, "2\n"), "postgres");
        assert_eq!(choose_answer(&question, "0"), "0");
        assert_eq!(choose_answer(&question, "9"), "9");
        assert_eq!(choose_answer(&question, "duckdb\n"), "duckdb");
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
        assert_eq!(
            control_command("/effort High"),
            Some(Control::SetEffort("high".into()))
        );
        assert_eq!(control_command("/effort "), None);
        assert_eq!(
            control_command("/mode Plan"),
            Some(Control::SetMode("plan".into()))
        );
        assert_eq!(control_command("/mode "), None);
        assert!(describe_mode("plan").contains("refused"));
        assert!(describe_effort("max", true).ends_with("max."));
        assert!(describe_effort("low", false).contains("cannot reason"));
        assert_eq!(control_command("tell me about /model x"), None);
    }

    #[test]
    fn branch_and_compare_commands() {
        assert_eq!(command("/branch"), Some(Command::Branch(None)));
        assert_eq!(
            command("/branch  try again "),
            Some(Command::Branch(Some("try again".into())))
        );
        assert_eq!(
            command("/compare fake:a fake:b -- fix the bug"),
            Some(Command::Compare(
                vec!["fake:a".into(), "fake:b".into()],
                "fix the bug".into()
            ))
        );
        // Two to four models and a message are needed.
        for bad in [
            "/compare",
            "/compare fake:a -- hi",
            "/compare fake:a fake:b",
        ] {
            assert_eq!(command(bad), Some(Command::Usage(COMPARE_USAGE)), "{bad}");
        }
        assert_eq!(command("/pick #2"), Some(Command::Pick(2)));
        for bad in ["/pick", "/pick x"] {
            assert_eq!(command(bad), Some(Command::Usage(PICK_USAGE)), "{bad}");
        }
    }

    #[test]
    fn diff_prints_unified_diffs() {
        assert_eq!(command("/diff"), Some(Command::Diff(None)));
        assert_eq!(command("/diff 2"), Some(Command::Diff(Some(2))));
        assert_eq!(command("/diff x"), None);
        let review = SessionReviewResult {
            available: true,
            repos: vec!["/r".into()],
            files: vec![ReviewFile {
                repo: "/r".into(),
                id: "f".into(),
                path: "a.txt".into(),
                status: "added".into(),
                binary: false,
                hunks: vec![ReviewHunk {
                    id: "x".into(),
                    header: "@@ -0,0 +1,1 @@".into(),
                    old_start: 0,
                    old_lines: 0,
                    new_start: 1,
                    new_lines: 1,
                    lines: vec!["+hi".into()],
                }],
            }],
            ..SessionReviewResult::default()
        };
        assert_eq!(
            describe_review(&review),
            "--- /dev/null\n+++ b/a.txt\n@@ -0,0 +1,1 @@\n+hi\n"
        );
    }

    #[test]
    fn rewind_commands_take_a_checkpoint_and_a_mode() {
        assert_eq!(command("/rewind"), Some(Command::Rewind(None)));
        assert_eq!(
            command("/rewind 3"),
            Some(Command::Rewind(Some((3, RewindMode::Both))))
        );
        assert_eq!(
            command("/rewind #2 files"),
            Some(Command::Rewind(Some((2, RewindMode::Files))))
        );
        assert_eq!(
            command("/rewind 2 conversation"),
            Some(Command::Rewind(Some((2, RewindMode::Conversation))))
        );
        assert_eq!(command("/rewind x"), None);
        assert_eq!(command("/rewind 2 all"), None);
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
                attachments: Vec::new(),
            },
            QueuedMessage {
                id: "q2".into(),
                text: "use tabs".into(),
                steer: true,
                attachments: Vec::new(),
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
