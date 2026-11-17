//! `imperium run`: one task through the real Imperium agent loop, against any OpenAI-compatible
//! endpoint, with the result printed as JSON. It is the harness evaluations of Imperium's own
//! models run through (see "Running evals through the harness" in docs/03-agent-engine.md).
//!
//! The session runs in the shared daemon (`session.eval_create`), so the user's permission rules,
//! path grants, isolation tier and protected folders apply exactly as they do in the app. This
//! module reads the command line, the task, the variant and the recordings, starts the session,
//! follows its events and prints what the daemon recorded.

pub mod replay;
pub mod variant;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::ValueEnum;
use imperium_api::{Effort, PermissionMode};
use imperium_client::Client;
use imperium_config::Paths;
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::methods::{
    EvalModel, EvalOptions, EvalReply, EvalResult, EvalVariant, SessionDelete, SessionDeleteParams,
    SessionEvalCreate, SessionEvalCreateParams, SessionEvalResult, SessionEvalResultParams,
    SessionPromptPreview, SessionSend, SessionSendParams,
};
use imperium_protocol::session::{
    AgentEvent, Plan, ROOT_AGENT, SESSION_EVENT, SessionEvent, SessionEventParams,
};
use serde_json::{Value, json};

/// The version of the JSON this command prints.
pub const SCHEMA_VERSION: u32 = 1;

/// What `{workspace}` stands for in a prompt that is printed without a real folder.
const WORKSPACE_PLACEHOLDER: &str = "{workspace}";

/// The loop flags `--flag key=value` (and a variant file's `[flags]`) understand.
pub const FLAGS: &[(&str, &str)] = &[
    (
        "max_steps",
        "most model calls the root agent makes in the turn (default 50)",
    ),
    (
        "effort",
        "reasoning effort: low, medium, high or max (--effort wins)",
    ),
    (
        "auto_compact",
        "true or false: compact the conversation when it nears the context window (default false)",
    ),
    (
        "auto_compact_at",
        "share of the window (0 to 1) at which a running turn compacts; 0 is off (default off)",
    ),
    (
        "knowledge",
        "true or false: knowledge agents and their tools (default true)",
    ),
    (
        "attended",
        "true or false: act as if a person reads the turns, which turns the harness aids that \
         talk to one on when `harness` allows it (default false)",
    ),
    (
        "harness",
        "on, off or auto: the harness aids for small models while attended (default auto)",
    ),
];

/// An on/off switch on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

/// `imperium run`: run one task through the agent loop and print the result as JSON.
#[derive(Debug, Clone, clap::Args)]
pub struct RunArgs {
    /// Run a task and print its result. Required to run a task (`--print-prompt` needs it not).
    #[arg(long, required_unless_present = "print_prompt")]
    pub headless: bool,
    /// Print the result as JSON. It is the only output format; scripts pass it to say so.
    #[arg(long)]
    pub json: bool,
    /// Print the resolved system prompt and tool schemas as JSON and stop, without calling a
    /// model. The prompt shows `{workspace}` where the working folder goes.
    #[arg(long, conflicts_with_all = ["task", "subagent_replay"])]
    pub print_prompt: bool,
    /// The task: a JSON file with a `prompt` (its other fields are kept in the output), or the
    /// prompt itself.
    #[arg(long, required_unless_present = "print_prompt")]
    pub task: Option<String>,
    /// The folder the agent works in, as a project of the daemon: the user's permission rules,
    /// grants and isolation tier for it apply. Point it at a copy: the tools can change it where
    /// the permission mode lets them.
    #[arg(long, required_unless_present = "print_prompt")]
    pub workspace: Option<PathBuf>,
    /// The root model's endpoint: an OpenAI-compatible base URL such as
    /// `http://127.0.0.1:8080/v1`, optionally followed by `#<model id>`.
    #[arg(long, required_unless_present = "print_prompt")]
    pub root_model: Option<String>,
    /// The root model's id at that endpoint (when `--root-model` has no `#<id>`).
    #[arg(long)]
    pub model_id: Option<String>,
    /// The environment variable that holds the endpoint's API key. The key is never printed.
    #[arg(long, value_name = "VAR")]
    pub api_key_env: Option<String>,
    /// The root model runs on this machine (a local llama-server): the harness aids that are
    /// `auto` count as on, as in the product, where `auto` means on for a local model.
    #[arg(long)]
    pub local: bool,
    /// Look up named variants under this folder's `config` folder (laid out as
    /// `IMPERIUM_HOME`) instead of the user's own.
    #[arg(long, value_name = "DIR")]
    pub config_home: Option<PathBuf>,
    /// Run the named agent file (`agents/<name>.md`) as the root agent instead of the standard
    /// root: its instructions follow the base ones and only the tools it lists are offered; a
    /// file that lists `tools` with nothing in the list has none (a single-turn critic).
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
    /// The root model's context window in tokens.
    #[arg(long, value_name = "N")]
    pub context_tokens: Option<u64>,
    /// How hard a model that can reason thinks: low, medium, high or max.
    #[arg(long)]
    pub effort: Option<Effort>,
    /// Turn thinking on or off by sending `chat_template_kwargs.enable_thinking` (llama.cpp,
    /// vLLM).
    #[arg(long, value_enum)]
    pub thinking: Option<Switch>,
    /// A JSON object sent as `chat_template_kwargs` with every request of the root model.
    #[arg(long, value_name = "JSON")]
    pub chat_template_kwargs: Option<String>,
    /// A named variant (`<config>/harness-variants/<name>.toml`) or the path of a variant file.
    #[arg(long)]
    pub variant: Option<String>,
    /// A text file appended to the root agent's system prompt, after any variant's edits.
    #[arg(long, value_name = "FILE")]
    pub prompt_overlay: Option<PathBuf>,
    /// A loop flag, `key=value`; repeatable. The flags are listed in docs/03-agent-engine.md:
    /// max_steps, effort, auto_compact, auto_compact_at, knowledge, attended, harness.
    #[arg(long = "flag", value_name = "KEY=VALUE")]
    pub flags: Vec<String>,
    /// Answer subagents from this recordings file instead of running them (no model calls for
    /// subagents). A task file with a `subagent_replay` object is used when this is not given.
    #[arg(long, value_name = "FILE")]
    pub subagent_replay: Option<PathBuf>,
    /// Stop before the next model call once this much has been spent, in US dollars. Needs
    /// prices (`--price-in` and `--price-out`).
    #[arg(long, value_name = "USD")]
    pub cost_ceiling: Option<f64>,
    /// Stop before the next model call once this many tokens (prompt plus completion, over all
    /// agents) have been used.
    #[arg(long, value_name = "N")]
    pub token_ceiling: Option<u64>,
    /// Price of a million prompt tokens in US dollars, for the cost ledger.
    #[arg(long, value_name = "USD")]
    pub price_in: Option<f64>,
    /// Price of a million completion tokens in US dollars, for the cost ledger.
    #[arg(long, value_name = "USD")]
    pub price_out: Option<f64>,
    /// How the agent's tool calls are checked, on top of the user's permission rules and the
    /// isolation tier. A call that would ask is always denied: nobody answers. `full` runs
    /// everything the rules and the safety floor do not stop.
    #[arg(long, default_value_t = PermissionMode::Default)]
    pub permission_mode: PermissionMode,
    /// Stop the run after this many seconds.
    #[arg(long, default_value_t = 1800, value_name = "SECONDS")]
    pub timeout_s: u64,
    /// Write the session's events, one JSON object per line, to this file.
    #[arg(long, value_name = "FILE")]
    pub journal: Option<PathBuf>,
}

/// What a run came to: the JSON to print and the process exit code.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub json: Value,
    /// 0 when the root agent finished its turn; 3 when the run stopped early (see the JSON's
    /// `exit_reason`).
    pub code: i32,
}

/// Runs the command in the daemon `client` is connected to: one task, or the prompt printout.
pub async fn run(client: &mut Client, args: RunArgs) -> anyhow::Result<Outcome> {
    let printing = args.print_prompt;
    if args.permission_mode == PermissionMode::Full {
        eprintln!(
            "warning: --permission-mode full runs everything your permission rules and the \
             safety floor do not stop"
        );
    }
    let setup = Setup::new(&args)?;
    let project_dir = match &args.workspace {
        Some(dir) => {
            let dir = std::path::absolute(dir)
                .with_context(|| format!("the workspace {}", dir.display()))?;
            if !dir.is_dir() {
                bail!("the workspace {} is not a folder", dir.display());
            }
            Some(dir.to_string_lossy().into_owned())
        }
        None => None,
    };
    let params = SessionEvalCreateParams {
        environment_id: None,
        project_dir,
        options: setup.options.clone(),
    };
    if printing {
        let preview = client.call::<SessionPromptPreview>(params).await?;
        return Ok(Outcome {
            json: json!({
                "schema_version": SCHEMA_VERSION,
                "mode": "print_prompt",
                "engine": engine_json(),
                "variant": variant_json(&setup.options.variant, &preview.variant_config_hash, &preview.warnings),
                "context_tokens": args.context_tokens,
                "prompt_hash": preview.prompt_hash,
                "system_prompt": preview.system_prompt,
                "workspace_placeholder": WORKSPACE_PLACEHOLDER,
                "tools": preview.tools,
            }),
            code: 0,
        });
    }

    let started = std::time::Instant::now();
    let created = client.call::<SessionEvalCreate>(params).await?;
    let session_id = created.session_id.clone();
    let sent = client
        .call::<SessionSend>(SessionSendParams {
            session_id: session_id.clone(),
            text: setup.task.prompt.clone(),
            mode: None,
            agent_id: None,
            confirm_budget: None,
            attachments: Vec::new(),
        })
        .await;
    if let Err(error) = sent {
        let _ = delete(client, &session_id).await;
        return Err(error.into());
    }
    let mut log = EventLog::default();
    let end = follow(client, &session_id, args.timeout_s, &mut log).await;
    let wall_time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let result = client
        .call::<SessionEvalResult>(SessionEvalResultParams {
            session_id: session_id.clone(),
        })
        .await;
    let _ = delete(client, &session_id).await;
    let result = result?;
    if let Some(path) = &args.journal {
        write_journal(path, &log.events)?;
    }
    for warning in &result.warnings {
        eprintln!("warning: variant {}: {warning}", setup.options.variant.id);
    }
    let exit_reason = exit_reason(&end, &result);
    let priced = setup.options.model.price_in.is_some() || setup.options.model.price_out.is_some();
    let json = json!({
        "schema_version": SCHEMA_VERSION,
        "mode": "run",
        "engine": engine_json(),
        "task": setup.task.json(),
        "agent": args.agent,
        "model": setup.model_json(),
        "variant": variant_json(&setup.options.variant, &result.variant_config_hash, &result.warnings),
        "prompt_hash": result.prompt_hash,
        "system_prompt": result.last_request.as_ref().map(|request| request.system_prompt.clone()),
        "tools": result.first_request.as_ref().map_or_else(Vec::new, |request| request.tools.clone()),
        "transcript": transcript(&result),
        "final_reply": final_reply(&result),
        "plan": result
            .plan
            .clone()
            .or_else(|| log.plan.as_ref().and_then(|plan| serde_json::to_value(plan).ok())),
        "subagents": subagents_json(&log, &result),
        "replay": result.replay,
        "ledger": {
            "total": total_ledger(&result, priced),
            "root": ledger_json(&result.root, priced),
            "subagents": ledger_json(&result.subagents, priced),
            "calls_without_reported_usage": result.estimated_calls,
            "priced": priced,
            "cost_ceiling_usd": args.cost_ceiling,
            "token_ceiling": args.token_ceiling,
        },
        "wall_time_ms": wall_time_ms,
        "exit_reason": exit_reason,
        "error": end.error(),
        "root_tool_calls": log.tool_calls,
        "failed_tool_calls": log.failed_tool_calls,
        "permission_requests": result.permissions,
        "questions_asked": result.questions,
        "compactions": log.compactions,
        "notices": log.notices,
        "endpoint_errors": result.errors,
    });
    let code = if exit_reason == "completed" { 0 } else { 3 };
    Ok(Outcome { json, code })
}

/// Deletes the evaluation session: it is not kept.
async fn delete(client: &mut Client, session_id: &str) -> Result<(), imperium_client::ClientError> {
    client
        .call::<SessionDelete>(SessionDeleteParams {
            session_id: session_id.to_string(),
            remove_worktrees: false,
        })
        .await
        .map(|_| ())
}

/// How often the daemon is asked how the run stands when no event has come: the daemon is the
/// authority on how the turn ended, and a notification can be missed.
const POLL: Duration = Duration::from_secs(2);

/// Reads the session's events until the root agent's turn ends, the session pauses for a usage
/// limit, the connection fails, or `timeout_s` passes. When the events are quiet it asks the
/// daemon for the run's end.
async fn follow(client: &mut Client, session_id: &str, timeout_s: u64, log: &mut EventLog) -> End {
    let deadline = tokio::time::sleep(Duration::from_secs(timeout_s));
    tokio::pin!(deadline);
    loop {
        let received = tokio::select! {
            () = &mut deadline => return End::Timeout,
            () = tokio::time::sleep(POLL) => None,
            notification = client.next_notification() => Some(notification),
        };
        let Some(received) = received else {
            let asked = client
                .call::<SessionEvalResult>(SessionEvalResultParams {
                    session_id: session_id.to_string(),
                })
                .await;
            match asked.map(|result| result.ended) {
                Ok(Some(ended)) => return end_of(&ended),
                Ok(None) => continue,
                Err(error) => return End::Failed(error.to_string()),
            }
        };
        let notification = match received {
            Ok(notification) => notification,
            Err(error) => return End::Failed(error.to_string()),
        };
        if notification.method != SESSION_EVENT {
            continue;
        }
        let Ok(params) = serde_json::from_value::<SessionEventParams>(notification.params) else {
            continue;
        };
        if params.session_id != session_id {
            continue;
        }
        if let Some(end) = log.observe(params.agent_event) {
            return end;
        }
    }
}

/// The run's end as the daemon recorded it.
fn end_of(ended: &imperium_protocol::methods::EvalEnd) -> End {
    let message = ended.message.clone().unwrap_or_default();
    match ended.reason.as_str() {
        "completed" => End::Completed,
        "timeout" => End::Timeout,
        "paused" => End::Paused(message),
        _ => End::Failed(message),
    }
}

fn write_journal(path: &Path, events: &[AgentEvent]) -> anyhow::Result<()> {
    let mut text = String::new();
    for event in events {
        text.push_str(&serde_json::to_string(event)?);
        text.push('\n');
    }
    std::fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}

/// How the run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum End {
    Completed,
    Failed(String),
    Timeout,
    Paused(String),
}

impl End {
    fn error(&self) -> Value {
        match self {
            End::Completed | End::Timeout => Value::Null,
            End::Failed(message) | End::Paused(message) => json!(message),
        }
    }
}

fn exit_reason(end: &End, result: &EvalResult) -> &'static str {
    match end {
        End::Completed => "completed",
        End::Timeout => "timeout",
        End::Paused(_) => "rate_limited",
        End::Failed(message) => match result.ceiling.as_deref() {
            Some("cost_ceiling") => "cost_ceiling",
            Some("token_ceiling") => "token_ceiling",
            _ if message.starts_with("stopped after") && message.contains("model calls") => {
                "step_limit"
            }
            _ if result.errors.is_empty() => "error",
            _ => "model_error",
        },
    }
}

/// What the run saw happen, from the session's events.
#[derive(Debug, Default)]
struct EventLog {
    events: Vec<AgentEvent>,
    plan: Option<Plan>,
    subagents: Vec<SubagentRow>,
    notices: Vec<String>,
    compactions: u64,
    tool_calls: u64,
    failed_tool_calls: u64,
}

#[derive(Debug)]
struct SubagentRow {
    task_id: String,
    brief: String,
    model: String,
    finished: Option<(bool, String)>,
}

impl EventLog {
    /// Takes in one event and says when the root agent's turn ended.
    fn observe(&mut self, event: AgentEvent) -> Option<End> {
        let root = event.agent_id == ROOT_AGENT;
        let end = match &event.event {
            SessionEvent::PlanUpdated { plan } if root => {
                self.plan = Some(plan.clone());
                None
            }
            SessionEvent::TaskSpawned {
                task_id,
                brief,
                model,
                ..
            } => {
                self.subagents.push(SubagentRow {
                    task_id: task_id.clone(),
                    brief: brief.clone(),
                    model: model.clone(),
                    finished: None,
                });
                None
            }
            SessionEvent::TaskFinished {
                task_id,
                ok,
                report,
                ..
            } => {
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|row| row.task_id == *task_id)
                {
                    row.finished = Some((*ok, report.clone()));
                }
                None
            }
            SessionEvent::ToolFinished { ok, .. } if root => {
                self.tool_calls += 1;
                if !ok {
                    self.failed_tool_calls += 1;
                }
                None
            }
            SessionEvent::Notice { message } => {
                self.notices.push(message.clone());
                None
            }
            SessionEvent::ContextCompacted { .. } if root => {
                self.compactions += 1;
                None
            }
            SessionEvent::Paused { reason, .. } => Some(End::Paused(reason.clone())),
            SessionEvent::TurnCompleted { .. } if root => Some(End::Completed),
            SessionEvent::TurnFailed { message } if root => Some(End::Failed(message.clone())),
            _ => None,
        };
        self.events.push(event);
        end
    }
}

/// The task as given: its prompt, and the file it came from.
#[derive(Debug, Clone)]
struct Task {
    id: Option<String>,
    prompt: String,
    source: String,
    raw: Option<Value>,
}

impl Task {
    /// `spec` is a JSON file when it names one that exists, otherwise the prompt itself.
    fn load(spec: &str) -> anyhow::Result<Self> {
        let path = Path::new(spec);
        if path.extension().is_some_and(|ext| ext == "json") && path.is_file() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("could not read the task {}", path.display()))?;
            let raw: Value = serde_json::from_str(&text)
                .with_context(|| format!("{} is not valid JSON", path.display()))?;
            let string = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_string);
            let prompt = ["prompt", "user_prompt", "input", "message"]
                .iter()
                .find_map(|key| string(key))
                .with_context(|| {
                    format!("{} has no `prompt` (a string) to send", path.display())
                })?;
            let id = ["id", "task_id", "name"].iter().find_map(|key| string(key));
            return Ok(Task {
                id,
                prompt,
                source: path.display().to_string(),
                raw: Some(raw),
            });
        }
        if spec.trim().is_empty() {
            bail!("--task is empty");
        }
        Ok(Task {
            id: None,
            prompt: spec.to_string(),
            source: "inline".into(),
            raw: None,
        })
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "source": self.source,
            "prompt": self.prompt,
            "raw": self.raw,
        })
    }
}

/// Everything resolved from the command line before the session starts.
struct Setup {
    task: Task,
    options: EvalOptions,
}

impl Setup {
    fn new(args: &RunArgs) -> anyhow::Result<Self> {
        let printing = args.print_prompt;
        let task = if printing {
            Task {
                id: None,
                prompt: String::new(),
                source: "print-prompt".into(),
                raw: None,
            }
        } else {
            Task::load(args.task.as_deref().context("--task is required")?)?
        };

        let mut eval_variant = match &args.variant {
            Some(spec) => {
                let config_dir = match &args.config_home {
                    Some(home) => Some(Paths::under(home).config_dir().to_path_buf()),
                    None => Paths::from_env()
                        .ok()
                        .map(|paths| paths.config_dir().to_path_buf()),
                };
                variant::load(spec, config_dir.as_deref())?
            }
            None => variant::baseline(),
        };
        eval_variant = variant::with_flags(eval_variant, &args.flags)?;
        if let Some(path) = &args.prompt_overlay {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("could not read the overlay {}", path.display()))?;
            eval_variant = variant::with_overlay(eval_variant, text.trim_end().to_string());
        }

        let replay: Option<Vec<EvalReply>> = if printing {
            None
        } else {
            let from_task = task
                .raw
                .as_ref()
                .filter(|raw| raw.get("subagent_replay").is_some())
                .cloned();
            match (&args.subagent_replay, from_task) {
                (Some(path), _) => Some(replay::load(path)?),
                (None, Some(raw)) => Some(replay::from_value(raw)?),
                (None, None) => None,
            }
        };

        let (base_url, model_id) = if printing {
            (
                args.root_model
                    .clone()
                    .unwrap_or_else(|| "http://print-prompt.invalid/v1".into()),
                args.model_id
                    .clone()
                    .unwrap_or_else(|| "print-prompt".into()),
            )
        } else {
            split_model(
                args.root_model
                    .as_deref()
                    .context("--root-model is required")?,
                args.model_id.as_deref(),
            )?
        };
        let api_key = match &args.api_key_env {
            Some(name) => Some(
                std::env::var(name)
                    .with_context(|| format!("the environment variable {name} is not set"))?,
            ),
            None => None,
        };

        let mut kwargs = serde_json::Map::new();
        if let Some(switch) = args.thinking {
            kwargs.insert("enable_thinking".into(), json!(switch == Switch::On));
        }
        if let Some(text) = &args.chat_template_kwargs {
            let value: Value =
                serde_json::from_str(text).context("--chat-template-kwargs is not valid JSON")?;
            let Value::Object(map) = value else {
                bail!("--chat-template-kwargs must be a JSON object");
            };
            kwargs.extend(map);
        }

        Ok(Setup {
            task,
            options: EvalOptions {
                model: EvalModel {
                    base_url,
                    model_id,
                    api_key,
                    chat_template_kwargs: (!kwargs.is_empty()).then_some(Value::Object(kwargs)),
                    local: args.local,
                    effort: args.effort.map(|effort| effort.as_str().to_string()),
                    context_tokens: args.context_tokens,
                    price_in: args.price_in,
                    price_out: args.price_out,
                },
                variant: eval_variant,
                replay,
                agent: args.agent.clone(),
                permission_mode: Some(args.permission_mode.as_str().to_string()),
                cost_ceiling_usd: args.cost_ceiling,
                token_ceiling: args.token_ceiling,
                timeout_s: Some(args.timeout_s),
            },
        })
    }

    fn model_json(&self) -> Value {
        let model = &self.options.model;
        json!({
            "base_url": model.base_url,
            "model_id": model.model_id,
            "context_tokens": model.context_tokens,
            "effort": model.effort,
            "chat_template_kwargs": model.chat_template_kwargs,
            "price_in_usd_per_mtok": model.price_in,
            "price_out_usd_per_mtok": model.price_out,
        })
    }
}

fn variant_json(variant: &EvalVariant, config_hash: &str, warnings: &[String]) -> Value {
    json!({
        "id": variant.id,
        "description": variant.description,
        "source": variant.source,
        "config_hash": config_hash,
        "flags": variant.flags,
        "overlay": variant.overlay.is_some(),
        "warnings": warnings,
    })
}

/// The base URL and model id from `--root-model` (`<url>` or `<url>#<id>`) and `--model-id`.
fn split_model(root_model: &str, model_id: Option<&str>) -> anyhow::Result<(String, String)> {
    let (url, inline) = match root_model.split_once('#') {
        Some((url, id)) => (url, Some(id)),
        None => (root_model, None),
    };
    let id = model_id
        .or(inline)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .context("give the model id: --model-id <id>, or --root-model <base-url>#<id>")?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        bail!("--root-model must be an http or https base URL such as http://127.0.0.1:8080/v1");
    }
    Ok((url.to_string(), id.to_string()))
}

/// The root agent's conversation as the model last read it, plus the reply it got.
fn transcript(result: &EvalResult) -> Value {
    let Some(last) = &result.last_request else {
        return json!([]);
    };
    let mut messages = last.messages.clone();
    if let Some(reply) = &result.last_reply {
        messages.push(reply.clone());
    }
    Value::Array(messages)
}

fn final_reply(result: &EvalResult) -> Value {
    result
        .last_reply
        .as_ref()
        .and_then(|reply| reply.get("content"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn ledger_json(ledger: &imperium_protocol::methods::EvalLedger, priced: bool) -> Value {
    json!({
        "model_calls": ledger.calls,
        "prompt_tokens": ledger.prompt_tokens,
        "completion_tokens": ledger.completion_tokens,
        "cached_tokens": ledger.cached_tokens,
        "cost_usd": if priced { json!(ledger.cost_usd) } else { Value::Null },
    })
}

fn total_ledger(result: &EvalResult, priced: bool) -> Value {
    let (root, subagents) = (&result.root, &result.subagents);
    ledger_json(
        &imperium_protocol::methods::EvalLedger {
            calls: root.calls + subagents.calls,
            prompt_tokens: root.prompt_tokens + subagents.prompt_tokens,
            completion_tokens: root.completion_tokens + subagents.completion_tokens,
            cached_tokens: root.cached_tokens + subagents.cached_tokens,
            cost_usd: root.cost_usd + subagents.cost_usd,
        },
        priced,
    )
}

fn subagents_json(log: &EventLog, result: &EvalResult) -> Value {
    Value::Array(
        log.subagents
            .iter()
            .map(|row| {
                let recorded = result
                    .answered
                    .iter()
                    .find(|answer| answer.task_id == row.task_id);
                let (ok, report) = match (recorded, &row.finished) {
                    (Some(answer), _) => (Some(answer.ok), Some(answer.report.clone())),
                    (None, Some((ok, report))) => (Some(*ok), Some(report.clone())),
                    (None, None) => (None, None),
                };
                json!({
                    "task_id": row.task_id,
                    "brief": row.brief,
                    "model": row.model,
                    "source": if recorded.is_some() { "replay" } else { "model" },
                    "role": recorded.map(|answer| answer.role),
                    "recording": recorded.and_then(|answer| answer.reply),
                    "ok": ok,
                    "report": report,
                })
            })
            .collect(),
    )
}

fn engine_json() -> Value {
    json!({
        "imperium_version": env!("CARGO_PKG_VERSION"),
        "protocol_version": PROTOCOL_VERSION.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_comes_from_the_url_fragment_or_the_flag() {
        assert_eq!(
            split_model("http://h/v1#m", None).unwrap(),
            ("http://h/v1".to_string(), "m".to_string())
        );
        assert_eq!(
            split_model("http://h/v1#a", Some("b")).unwrap().1,
            "b".to_string()
        );
        assert!(split_model("http://h/v1", None).is_err());
        assert!(split_model("ftp://h#m", None).is_err());
    }
}
