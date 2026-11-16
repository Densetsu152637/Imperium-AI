//! `imperium run`: one task through the real Imperium agent loop with no daemon and no desktop,
//! against any OpenAI-compatible endpoint, with the result printed as JSON. It is the harness
//! evaluations of Imperium's own models run through (see "Running evals through the harness" in
//! docs/03-agent-engine.md).
//!
//! The session is the engine's own (`imperium_engine::Session`): the same root prompt, tool
//! schemas and loop as the daemon's. A [`record::Recorder`] wraps the model client, so the
//! output holds exactly what was sent and what came back; a [`variant::Variant`] edits what the
//! root agent is given; a [`replay::Replay`] answers subagents from recordings.

pub mod record;
pub mod replay;
pub mod variant;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use clap::ValueEnum;
use imperium_api::encoding::hex;
use imperium_api::{Effort, PermissionMode, Pricing, ToolProvider};
use imperium_config::{
    Enforce, HarnessMode, HarnessSettings, KnowledgeSettings, Paths, RoutingSettings,
};
use imperium_engine::{
    ChatClient, ChatClients, Endpoint, KnowledgeConfig, ResolvedModel, Session, SessionConfig,
    SessionOptions, SubagentReplay,
};
use imperium_models::OpenAiCompatible;
use imperium_policy::permissions::Permissions;
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::session::{AgentEvent, PermissionDecision, Plan, ROOT_AGENT, SessionEvent};
use imperium_tools::Workspace;
use imperium_web::{SearchFuture, SearchProvider, Web, WebError, WebTools};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast::error::RecvError;

use record::{Ceilings, Record, Recorder, Shared};
use replay::{Recordings, Replay};
use variant::{Placeholders, Variant};

/// The version of the JSON this command prints.
pub const SCHEMA_VERSION: u32 = 1;

/// What `{workspace}` stands for in a prompt that is hashed or printed without a real folder.
const WORKSPACE_PLACEHOLDER: &str = "{workspace}";

/// The text the stand-in model is sent in `--print-prompt` mode.
const PRINT_PROMPT_MESSAGE: &str = "print-prompt";

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

/// `imperium run`: run one task headlessly and print the result as JSON.
#[derive(Debug, Clone, clap::Args)]
pub struct RunArgs {
    /// Run without the daemon or the desktop. Required to run a task (`--print-prompt` needs
    /// neither).
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
    /// The folder the agent works in. Its tools can read and, where the permission mode lets
    /// them, change it: point it at a copy.
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
    /// Read the configuration from this folder, laid out as `IMPERIUM_HOME`, instead of the
    /// user's own. A headless run uses it for variants and for the isolation check of
    /// `--permission-mode full`; it applies none of the other settings.
    #[arg(long, value_name = "DIR")]
    pub config_home: Option<PathBuf>,
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
    /// How the agent's tool calls are checked. A call that would ask is always denied: nobody
    /// answers. `full` runs everything the rules and the safety floor do not stop.
    #[arg(long, default_value_t = PermissionMode::Default)]
    pub permission_mode: PermissionMode,
    /// Stop the run after this many seconds.
    #[arg(long, default_value_t = 1800, value_name = "SECONDS")]
    pub timeout_s: u64,
    /// Keep the session's journal at this path (events, one JSON object per line).
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

/// Runs the command: one task, or the prompt printout.
pub async fn run(args: RunArgs) -> anyhow::Result<Outcome> {
    let started = Instant::now();
    let printing = args.print_prompt;
    if args.permission_mode == PermissionMode::Auto {
        bail!(
            "--permission-mode auto asks a classifier model about each call, which a \
             deterministic eval does not do: use default, plan, ask, accept_edits or full"
        );
    }
    if args.permission_mode == PermissionMode::Full {
        check_full_allowed(args.config_home.as_deref())?;
        eprintln!(
            "warning: --permission-mode full runs commands unconfined, in your own shell and \
             account, under only the safety floor's built-in rules"
        );
    }
    let setup = Setup::new(&args)?;
    let scratch = Scratch::new(args.journal.as_deref())?;
    let workspace_dir = match (&args.workspace, printing) {
        (Some(dir), _) => dir.clone(),
        (None, true) => scratch.dir().join("workspace"),
        (None, false) => bail!("--workspace is required"),
    };
    if printing {
        std::fs::create_dir_all(&workspace_dir).context("could not make a folder to print for")?;
    }
    let workspace = Workspace::new(&workspace_dir)
        .with_context(|| format!("the workspace {} is not a folder", workspace_dir.display()))?
        .with_permissions(Permissions::default().with_mode(args.permission_mode));

    let record: Shared = Shared::default();
    let model = setup.model(&record, printing);
    let replay = setup.replay.clone();
    let config = setup.session_config(&workspace, model, &record, &scratch);
    let session = Session::spawn_with(
        config,
        SessionOptions {
            replay: replay
                .clone()
                .map(|replay| -> Arc<dyn SubagentReplay> { replay }),
        },
    )
    .context("could not start the session")?;
    let (earlier, mut events) = session.subscribe_with_history();
    let prompt = if printing {
        PRINT_PROMPT_MESSAGE.to_string()
    } else {
        setup.task.prompt.clone()
    };
    session.send(prompt).map_err(|error| anyhow!("{error}"))?;

    let mut log = EventLog::default();
    // Events taken in so far: after a lag the history says what was missed, so an ask whose
    // event was dropped is still answered.
    let mut seen = earlier.len();
    let deadline = tokio::time::sleep(Duration::from_secs(args.timeout_s));
    tokio::pin!(deadline);
    let end = loop {
        let received = tokio::select! {
            () = &mut deadline => None,
            event = events.recv() => Some(event),
        };
        let Some(received) = received else {
            session.stop();
            break End::Timeout;
        };
        let batch = match received {
            Ok(event) => {
                seen += 1;
                vec![event]
            }
            Err(RecvError::Lagged(_)) => {
                let (history, fresh) = session.subscribe_with_history();
                events = fresh;
                let missed = history.len().saturating_sub(seen);
                seen = history.len();
                history.into_iter().skip(seen - missed).collect()
            }
            Err(RecvError::Closed) => break End::Failed("the session closed".into()),
        };
        let ended = batch
            .into_iter()
            .find_map(|event| log.observe(&session, event));
        if let Some(end) = ended {
            break end;
        }
    };
    let wall_time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let workspace_root = session.project_dir().to_string();
    session.stop();
    drop(session);

    let record = record::lock(&record);
    let exit_reason = exit_reason(&end, &record);
    let variant = setup.variant_json(&record);
    for warning in &record.warnings {
        eprintln!("warning: variant {}: {warning}", setup.variant.id);
    }
    let (prompt_hash, system_prompt, tools) = prompt_parts(&record, &workspace_root);
    let json = if printing {
        json!({
            "schema_version": SCHEMA_VERSION,
            "mode": "print_prompt",
            "engine": engine_json(),
            "variant": variant,
            "context_tokens": args.context_tokens,
            "prompt_hash": prompt_hash,
            "system_prompt": system_prompt,
            "workspace_placeholder": WORKSPACE_PLACEHOLDER,
            "tools": tools,
        })
    } else {
        let priced = setup.pricing.is_some();
        json!({
            "schema_version": SCHEMA_VERSION,
            "mode": "run",
            "engine": engine_json(),
            "task": setup.task.json(),
            "model": setup.model_json(),
            "variant": variant,
            "prompt_hash": prompt_hash,
            "system_prompt": record.last_root.as_ref().map(|request| request.system_prompt.clone()),
            "tools": tools,
            "transcript": transcript(&record),
            "final_reply": final_reply(&record),
            "plan": log.plan.as_ref().and_then(|plan| serde_json::to_value(plan).ok()),
            "subagents": subagents_json(&log, replay.as_deref()),
            "replay": replay.as_ref().map(|replay| replay.summary()),
            "ledger": {
                "total": total_ledger(&record, priced),
                "root": record.root.to_json(priced),
                "subagents": record.subagents.to_json(priced),
                "calls_without_reported_usage": record.estimated_calls,
                "priced": priced,
                "cost_ceiling_usd": args.cost_ceiling,
                "token_ceiling": args.token_ceiling,
            },
            "wall_time_ms": wall_time_ms,
            "exit_reason": exit_reason,
            "error": end.error(),
            "root_tool_calls": log.tool_calls,
            "failed_tool_calls": log.failed_tool_calls,
            "permission_requests": log.permissions,
            "questions_asked": log.questions,
            "compactions": log.compactions,
            "notices": log.notices,
            "endpoint_errors": record.errors,
        })
    };
    let code = if printing || exit_reason == "completed" {
        0
    } else {
        3
    };
    Ok(Outcome { json, code })
}

/// Refuses `full` mode when the user has configured an isolation tier (`os` or `container`):
/// this run builds none of it, so `full` would run commands with less protection than the user
/// asked for. Until runs go through the daemon, that is an error rather than a surprise.
fn check_full_allowed(config_home: Option<&Path>) -> anyhow::Result<()> {
    let paths = match config_home {
        Some(home) => Paths::under(home),
        None => Paths::from_env().context("could not find the configuration folder")?,
    };
    let settings = imperium_config::load(&paths, None, std::env::vars())
        .and_then(|config| config.settings())
        .context("could not read the configuration to check the isolation tier")?;
    let tier = &settings.isolation.tier;
    if tier != "policy" {
        bail!(
            "--permission-mode full is refused: your configuration sets isolation.tier = {tier:?}, \
             and a headless run does not build that isolation, so commands would run unconfined. \
             Use another permission mode, or set isolation.tier = \"policy\" to accept that"
        );
    }
    Ok(())
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

fn exit_reason(end: &End, record: &Record) -> &'static str {
    match end {
        End::Completed => "completed",
        End::Timeout => "timeout",
        End::Paused(_) => "rate_limited",
        End::Failed(message) => {
            if let Some(ceiling) = record.ceiling {
                ceiling
            } else if message.starts_with("stopped after") && message.contains("model calls") {
                "step_limit"
            } else if record.errors.is_empty() {
                "error"
            } else {
                "model_error"
            }
        }
    }
}

/// What the run saw happen, from the session's events.
#[derive(Debug, Default)]
struct EventLog {
    plan: Option<Plan>,
    subagents: Vec<SubagentRow>,
    notices: Vec<String>,
    permissions: Vec<Value>,
    questions: Vec<Value>,
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
    /// Takes in one event; answers what needs an answer (nobody is there to), and says when the
    /// root agent's turn ended.
    fn observe(&mut self, session: &Session, event: AgentEvent) -> Option<End> {
        let root = event.agent_id == ROOT_AGENT;
        match event.event {
            SessionEvent::PermissionRequested {
                request_id,
                tool,
                summary,
                ..
            } => {
                let _ = session.respond(request_id, PermissionDecision::Deny);
                self.permissions.push(json!({
                    "agent": event.agent_id,
                    "tool": tool,
                    "summary": summary,
                    "decision": "denied",
                }));
            }
            SessionEvent::QuestionsAsked {
                request_id,
                questions,
            } => {
                let _ = session.answer(request_id, vec![String::new(); questions.len()]);
                self.questions.push(json!({
                    "agent": event.agent_id,
                    "questions": questions.iter().map(|question| question.question.clone()).collect::<Vec<_>>(),
                    "answered_with": "the recommended answers",
                }));
            }
            SessionEvent::PlanUpdated { plan } if root => self.plan = Some(plan),
            SessionEvent::TaskSpawned {
                task_id,
                brief,
                model,
                ..
            } => self.subagents.push(SubagentRow {
                task_id,
                brief,
                model,
                finished: None,
            }),
            SessionEvent::TaskFinished {
                task_id,
                ok,
                report,
                ..
            } => {
                if let Some(row) = self.subagents.iter_mut().find(|row| row.task_id == task_id) {
                    row.finished = Some((ok, report));
                }
            }
            SessionEvent::ToolFinished { ok, .. } if root => {
                self.tool_calls += 1;
                if !ok {
                    self.failed_tool_calls += 1;
                }
            }
            SessionEvent::Notice { message } => self.notices.push(message),
            SessionEvent::ContextCompacted { .. } if root => self.compactions += 1,
            SessionEvent::Paused { reason, .. } => {
                session.stop();
                return Some(End::Paused(reason));
            }
            SessionEvent::TurnCompleted { .. } if root => return Some(End::Completed),
            SessionEvent::TurnFailed { message } if root => return Some(End::Failed(message)),
            _ => {}
        }
        None
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

/// The loop flags, typed.
#[derive(Debug, Clone, PartialEq)]
struct LoopFlags {
    max_steps: u32,
    effort: Option<Effort>,
    auto_compact: bool,
    auto_compact_at: Option<f64>,
    knowledge: bool,
    attended: bool,
    harness: HarnessMode,
}

impl LoopFlags {
    fn parse(flags: &BTreeMap<String, String>) -> anyhow::Result<Self> {
        let mut parsed = LoopFlags {
            max_steps: 50,
            effort: None,
            auto_compact: false,
            auto_compact_at: None,
            knowledge: true,
            attended: false,
            harness: HarnessMode::Auto,
        };
        for (key, value) in flags {
            let bad = |what: &str| anyhow!("flag {key}={value}: expected {what}");
            match key.as_str() {
                "max_steps" => {
                    parsed.max_steps = value
                        .parse::<u32>()
                        .ok()
                        .filter(|steps| *steps > 0)
                        .ok_or_else(|| bad("a whole number above 0"))?;
                }
                "effort" => {
                    parsed.effort = Some(
                        value
                            .parse::<Effort>()
                            .map_err(|_| bad("low, medium, high or max"))?,
                    );
                }
                "auto_compact" => {
                    parsed.auto_compact = value.parse().map_err(|_| bad("true or false"))?;
                }
                "auto_compact_at" => {
                    let share: f64 = value.parse().map_err(|_| bad("a number from 0 to 1"))?;
                    if !(0.0..=1.0).contains(&share) {
                        return Err(bad("a number from 0 to 1"));
                    }
                    parsed.auto_compact_at = Some(share);
                }
                "knowledge" => {
                    parsed.knowledge = value.parse().map_err(|_| bad("true or false"))?;
                }
                "attended" => {
                    parsed.attended = value.parse().map_err(|_| bad("true or false"))?;
                }
                "harness" => {
                    parsed.harness = match value.as_str() {
                        "on" => HarnessMode::On,
                        "off" => HarnessMode::Off,
                        "auto" => HarnessMode::Auto,
                        _ => return Err(bad("on, off or auto")),
                    };
                }
                other => {
                    let known: Vec<&str> = FLAGS.iter().map(|(name, _)| *name).collect();
                    bail!("unknown flag {other:?}; the flags are {}", known.join(", "));
                }
            }
        }
        Ok(parsed)
    }
}

/// Everything resolved from the command line before the session starts.
struct Setup {
    local: bool,
    task: Task,
    variant: Arc<Variant>,
    flags: LoopFlags,
    placeholders: Placeholders,
    replay: Option<Arc<Replay>>,
    pricing: Option<Pricing>,
    base_url: String,
    model_id: String,
    api_key: Option<String>,
    effort: Option<Effort>,
    extra_body: Option<Value>,
    context_tokens: Option<u64>,
    cost_ceiling: Option<f64>,
    token_ceiling: Option<u64>,
}

impl Setup {
    fn new(args: &RunArgs) -> anyhow::Result<Self> {
        let printing = args.print_prompt;
        let task = if printing {
            Task {
                id: None,
                prompt: PRINT_PROMPT_MESSAGE.into(),
                source: "print-prompt".into(),
                raw: None,
            }
        } else {
            Task::load(args.task.as_deref().context("--task is required")?)?
        };

        let mut variant = match &args.variant {
            Some(spec) => {
                let config_dir = match &args.config_home {
                    Some(home) => Some(Paths::under(home).config_dir().to_path_buf()),
                    None => Paths::from_env()
                        .ok()
                        .map(|paths| paths.config_dir().to_path_buf()),
                };
                Variant::load(spec, config_dir.as_deref())?
            }
            None => Variant::baseline(),
        };
        variant = variant.with_flags(&args.flags)?;
        if let Some(path) = &args.prompt_overlay {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("could not read the overlay {}", path.display()))?;
            variant = variant.with_overlay(text.trim_end().to_string());
        }
        let flags = LoopFlags::parse(&variant.flags)?;

        let replay = if printing {
            None
        } else {
            let from_task = task
                .raw
                .as_ref()
                .filter(|raw| raw.get("subagent_replay").is_some())
                .cloned();
            match (&args.subagent_replay, from_task) {
                (Some(path), _) => Some(Recordings::load(path)?),
                (None, Some(raw)) => Some(Recordings::from_value(raw)?),
                (None, None) => None,
            }
            .map(|recordings| Arc::new(Replay::new(recordings)))
        };

        let pricing = match (args.price_in, args.price_out) {
            (None, None) => None,
            (input, output) => Some(Pricing {
                input_usd_per_mtok: input.unwrap_or(0.0),
                output_usd_per_mtok: output.unwrap_or(0.0),
                cached_input_usd_per_mtok: None,
            }),
        };
        if args.cost_ceiling.is_some() && pricing.is_none() {
            bail!(
                "--cost-ceiling needs prices: give --price-in and --price-out (per million tokens)"
            );
        }

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
        let extra_body =
            (!kwargs.is_empty()).then(|| json!({ "chat_template_kwargs": Value::Object(kwargs) }));

        Ok(Setup {
            local: args.local,
            task,
            variant: Arc::new(variant),
            effort: args.effort.or(flags.effort),
            flags,
            placeholders: Placeholders {
                context_tokens: args.context_tokens,
            },
            replay,
            pricing,
            base_url,
            model_id,
            api_key,
            extra_body,
            context_tokens: args.context_tokens,
            cost_ceiling: args.cost_ceiling,
            token_ceiling: args.token_ceiling,
        })
    }

    /// The root model, whose calls go through the recorder.
    fn model(&self, record: &Shared, printing: bool) -> (ResolvedModel, ChatClients) {
        let mut clients = ChatClients::default();
        let record = Arc::clone(record);
        let variant = Arc::clone(&self.variant);
        let placeholders = self.placeholders;
        let pricing = self.pricing;
        let extra = self.extra_body.clone();
        clients.register("openai", move |http, endpoint| {
            let mut real = OpenAiCompatible::new(http.clone(), endpoint);
            if let Some(extra) = &extra {
                real = real.with_extra_body(extra.clone());
            }
            Arc::new(Recorder {
                inner: (!printing).then(|| -> Arc<dyn ChatClient> { Arc::new(real) }),
                record: Arc::clone(&record),
                variant: Arc::clone(&variant),
                placeholders,
                pricing,
            })
        });
        let model = ResolvedModel {
            spec: format!("eval:{}", self.model_id),
            endpoint: Endpoint {
                base_url: self.base_url.clone(),
                api_key: self.api_key.clone(),
            },
            model: self.model_id.clone(),
            pricing: self.pricing,
            local: false,
            context_window: self.context_tokens,
            kind: "openai".into(),
            reasoning: self.effort.map(|_| true),
        };
        (model, clients)
    }

    fn session_config(
        &self,
        workspace: &Workspace,
        model: (ResolvedModel, ChatClients),
        record: &Shared,
        scratch: &Scratch,
    ) -> SessionConfig {
        let (model, clients) = model;
        let web: Arc<dyn ToolProvider> = Arc::new(WebTools {
            web: Web::new(Some(Arc::new(OfflineSearch))),
            allow_domains: Vec::new(),
            browser: None,
            downloads: None,
        });
        // `auto` means on for a model that runs on this machine, which the engine cannot tell
        // here (it has no local runtime), so `--local` says it.
        let mode = match self.flags.harness {
            HarnessMode::Auto if self.local => HarnessMode::On,
            other => other,
        };
        SessionConfig {
            id: "eval".into(),
            workspace: workspace.clone(),
            effort: self.effort.unwrap_or_default(),
            model: Some(model),
            catalog: Vec::new(),
            routing: RoutingSettings::default(),
            clients,
            local: None,
            tools: vec![web],
            notices: Vec::new(),
            extensions: Default::default(),
            memory: None,
            context: None,
            enforce: Enforce::Warn,
            pre_approved: Vec::new(),
            launcher: None,
            resume: false,
            resume_policy: imperium_engine::default_resume_policy(),
            instructions: None,
            personality: None,
            max_steps: self.flags.max_steps,
            background: None,
            checkpoints: None,
            budget_gate: Some(Arc::new(Ceilings {
                record: Arc::clone(record),
                cost_usd: self.cost_ceiling,
                tokens: self.token_ceiling,
            })),
            auto_compact: self.flags.auto_compact,
            auto_compact_at: self.flags.auto_compact_at,
            attended: self.flags.attended,
            attachments: Default::default(),
            auto_permissions: Default::default(),
            harness: HarnessSettings {
                dereference: mode,
                state: mode,
                hints: mode,
                clarify: mode,
                ..HarnessSettings::default()
            },
            keep_agent_state: false,
            journal_path: scratch.journal(),
            knowledge: KnowledgeConfig {
                settings: KnowledgeSettings {
                    enabled: self.flags.knowledge,
                    ..KnowledgeSettings::default()
                },
                ..KnowledgeConfig::default()
            },
        }
    }

    fn model_json(&self) -> Value {
        json!({
            "base_url": self.base_url,
            "model_id": self.model_id,
            "context_tokens": self.context_tokens,
            "effort": self.effort.map(|effort| effort.as_str()),
            "chat_template_kwargs": self
                .extra_body
                .as_ref()
                .and_then(|body| body.get("chat_template_kwargs")),
            "price_in_usd_per_mtok": self.pricing.map(|pricing| pricing.input_usd_per_mtok),
            "price_out_usd_per_mtok": self.pricing.map(|pricing| pricing.output_usd_per_mtok),
        })
    }

    fn variant_json(&self, record: &Record) -> Value {
        json!({
            "id": self.variant.id,
            "description": self.variant.description,
            "source": self.variant.source,
            "config_hash": self.variant.config_hash(),
            "flags": self.variant.flags,
            "overlay": self.variant.overlay.is_some(),
            "warnings": record.warnings,
        })
    }
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

/// The hash of the prompt and tools the root agent was first given, the prompt (with the working
/// folder as `{workspace}`) and the tools.
fn prompt_parts(record: &Record, workspace_root: &str) -> (Value, Value, Value) {
    let Some(first) = &record.first_root else {
        return (Value::Null, Value::Null, json!([]));
    };
    let prompt = first
        .system_prompt
        .replace(workspace_root, WORKSPACE_PLACEHOLDER);
    let digest = Sha256::digest(
        serde_json::to_vec(&json!({"system_prompt": prompt, "tools": first.tools}))
            .unwrap_or_default(),
    );
    (
        json!(format!("sha256:{}", hex(&digest))),
        json!(prompt),
        json!(first.tools),
    )
}

/// The root agent's conversation as the model last read it, plus the reply it got.
fn transcript(record: &Record) -> Value {
    let Some(last) = &record.last_root else {
        return json!([]);
    };
    let mut messages = last.messages.clone();
    if let Some(reply) = &record.last_reply {
        messages.push(reply.clone());
    }
    Value::Array(messages)
}

fn final_reply(record: &Record) -> Value {
    record
        .last_reply
        .as_ref()
        .and_then(|reply| reply.get("content"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn total_ledger(record: &Record, priced: bool) -> Value {
    let mut total = record.root;
    total.calls += record.subagents.calls;
    total.prompt_tokens += record.subagents.prompt_tokens;
    total.completion_tokens += record.subagents.completion_tokens;
    total.cached_tokens += record.subagents.cached_tokens;
    total.cost_usd += record.subagents.cost_usd;
    total.to_json(priced)
}

fn subagents_json(log: &EventLog, replay: Option<&Replay>) -> Value {
    let answered = replay.map(Replay::answered).unwrap_or_default();
    Value::Array(
        log.subagents
            .iter()
            .map(|row| {
                let recorded = answered.iter().find(|answer| answer.task_id == row.task_id);
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

/// Web search with no results: the tool is offered (its schema is part of the prompt under
/// test) but nothing leaves the machine.
#[derive(Debug)]
struct OfflineSearch;

impl SearchProvider for OfflineSearch {
    fn search<'a>(
        &'a self,
        _http: &'a reqwest::Client,
        _query: &'a str,
        _count: usize,
    ) -> SearchFuture<'a> {
        Box::pin(async {
            Err(WebError::Http(
                "web search is turned off in headless evals".into(),
            ))
        })
    }
}

/// A temporary folder for the run's journal (and the working folder `--print-prompt` shows),
/// removed when the run ends.
struct Scratch {
    dir: PathBuf,
    journal: PathBuf,
}

impl Scratch {
    fn new(journal: Option<&Path>) -> anyhow::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("imperium-eval-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).context("could not make a temporary folder")?;
        Ok(Scratch {
            journal: journal.map_or_else(|| dir.join("session.jsonl"), Path::to_path_buf),
            dir,
        })
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn journal(&self) -> PathBuf {
        self.journal.clone()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
