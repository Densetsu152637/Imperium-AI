//! Evaluation sessions: one task through the real agent loop with a harness variant, recorded
//! subagents, a root model of its own and unattended permission handling, made by
//! `session.eval_create` for `imperium run` (docs/03-agent-engine.md#running-evals-through-the-harness-implemented).
//!
//! The session is an ordinary one in everything the user's configuration decides: its working
//! folders, the permission rules and path grants of its environment, the isolation tier, and the
//! folders the safety floor guards. What it leaves out is what makes a run differ between
//! machines: the user's models, memory, skills, hooks, MCP servers, personality and project
//! instructions.

mod record;
mod replay;
mod variant;

use std::sync::{Arc, Mutex, PoisonError};

use imperium_api::{Effort, PermissionMode, Pricing, ToolProvider};
use imperium_config::HarnessMode;
use imperium_engine::{ChatClient, ChatClients, Endpoint, ResolvedModel, Session, SubagentReplay};
use imperium_models::OpenAiCompatible;
use imperium_protocol::methods::{
    EvalEnd, EvalOptions, EvalPermission, EvalQuestion, EvalResult, EvalVariant, PromptPreview,
};
use imperium_protocol::session::{AgentEvent, PermissionDecision, ROOT_AGENT, SessionEvent};
use imperium_web::{SearchFuture, SearchProvider, Web, WebError, WebTools};
use tokio::sync::broadcast::error::RecvError;

use record::{Ceilings, Recorder, Shared};
use replay::Replay;
use variant::{LoopFlags, Placeholders};

/// What `{workspace}` stands for in a prompt that is hashed or printed without a real folder.
pub(crate) const WORKSPACE_PLACEHOLDER: &str = "{workspace}";

/// What the stand-in model of a preview is sent.
pub(crate) const PREVIEW_MESSAGE: &str = "print-prompt";

/// Everything an evaluation session keeps beside the engine's own state.
#[derive(Debug)]
pub(crate) struct EvalRun {
    record: Shared,
    replay: Option<Arc<Replay>>,
    variant: Arc<EvalVariant>,
    asks: Mutex<Asks>,
    /// How the root's turn ended and its last plan, from the session's events: the daemon is
    /// the authority on it, whatever a client saw of the notifications.
    progress: Mutex<Progress>,
    /// The environment this run made for its folder, deleted with the session.
    ephemeral_env: Mutex<Option<String>>,
}

#[derive(Debug, Default)]
struct Progress {
    ended: Option<EvalEnd>,
    plan: Option<serde_json::Value>,
}

#[derive(Debug, Default)]
struct Asks {
    permissions: Vec<EvalPermission>,
    questions: Vec<EvalQuestion>,
}

/// An evaluation request checked and turned into what the session is built from.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub run: Arc<EvalRun>,
    pub model: ResolvedModel,
    extra_body: Option<serde_json::Value>,
    placeholders: Placeholders,
    pricing: Option<Pricing>,
    pub gate: Arc<Ceilings>,
    pub tools: Vec<Arc<dyn ToolProvider>>,
    pub flags: LoopFlags,
    pub permission_mode: PermissionMode,
    pub effort: Option<Effort>,
    pub agent: Option<String>,
    pub timeout_s: Option<u64>,
    pub subagent_replay: Option<Arc<dyn SubagentReplay>>,
    /// Answer every model call with a fixed reply (a prompt preview).
    pub preview: bool,
}

impl Prepared {
    /// The model clients for the session: the endpoint's own, wrapped to record and to apply
    /// the variant.
    pub(crate) fn clients(&self) -> ChatClients {
        let mut clients = ChatClients::default();
        let record = Arc::clone(&self.run.record);
        let variant = Arc::clone(&self.run.variant);
        let extra_body = self.extra_body.clone();
        let (placeholders, pricing, preview) = (self.placeholders, self.pricing, self.preview);
        clients.register("openai", move |http, endpoint| {
            let mut real = OpenAiCompatible::new(http.clone(), endpoint);
            if let Some(extra) = &extra_body {
                real = real.with_extra_body(extra.clone());
            }
            Arc::new(Recorder {
                inner: (!preview).then(|| -> Arc<dyn ChatClient> { Arc::new(real) }),
                record: Arc::clone(&record),
                variant: Arc::clone(&variant),
                placeholders,
                pricing,
            })
        });
        clients
    }

    /// Checks `options` and builds what a session needs. Every error is the client's to fix.
    pub(crate) fn new(options: &EvalOptions, preview: bool) -> Result<Prepared, String> {
        let flags = variant::validate(&options.variant)?;
        let permission_mode = match options.permission_mode.as_deref() {
            None => PermissionMode::Default,
            Some(text) => text
                .parse::<PermissionMode>()
                .map_err(|error| error.to_string())?,
        };
        if permission_mode == PermissionMode::Auto {
            return Err(
                "permission mode auto asks a classifier model about each call, which a \
                 deterministic eval does not do: use default, plan, ask, accept_edits or full"
                    .into(),
            );
        }
        let pricing = match (options.model.price_in, options.model.price_out) {
            (None, None) => None,
            (input, output) => Some(Pricing {
                input_usd_per_mtok: input.unwrap_or(0.0),
                output_usd_per_mtok: output.unwrap_or(0.0),
                cached_input_usd_per_mtok: None,
            }),
        };
        if options.cost_ceiling_usd.is_some() && pricing.is_none() {
            return Err("a cost ceiling needs prices (price_in and price_out)".into());
        }
        let effort = match options.model.effort.as_deref() {
            Some(text) => Some(text.parse::<Effort>().map_err(|error| error.to_string())?),
            None => flags.effort,
        };
        let replay = match &options.replay {
            Some(replies) => {
                replay::validate(replies)?;
                Some(Arc::new(Replay::new(replies.clone())))
            }
            None => None,
        };
        let extra_body = match &options.model.chat_template_kwargs {
            Some(serde_json::Value::Object(map)) if !map.is_empty() => Some(
                serde_json::json!({ "chat_template_kwargs": serde_json::Value::Object(map.clone()) }),
            ),
            Some(serde_json::Value::Object(_)) | None => None,
            Some(_) => return Err("chat_template_kwargs must be a JSON object".into()),
        };
        let url = options.model.base_url.as_str();
        if !preview && !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("the model's base_url must be an http or https URL".into());
        }
        let record: Shared = Shared::default();
        let variant = Arc::new(options.variant.clone());
        let placeholders = Placeholders {
            context_tokens: options.model.context_tokens,
        };
        let model = ResolvedModel {
            spec: format!("eval:{}", options.model.model_id),
            endpoint: Endpoint {
                base_url: options.model.base_url.clone(),
                api_key: options.model.api_key.clone(),
            },
            model: options.model.model_id.clone(),
            pricing,
            local: false,
            context_window: options.model.context_tokens,
            kind: "openai".into(),
            reasoning: effort.map(|_| true),
        };
        let web: Arc<dyn ToolProvider> = Arc::new(WebTools {
            web: Web::new(Some(Arc::new(OfflineSearch))),
            allow_domains: Vec::new(),
            browser: None,
            downloads: None,
        });
        let gate = Arc::new(Ceilings {
            record: Arc::clone(&record),
            cost_usd: options.cost_ceiling_usd,
            tokens: options.token_ceiling,
        });
        let subagent_replay = replay
            .clone()
            .map(|replay| -> Arc<dyn SubagentReplay> { replay });
        let flags = LoopFlags {
            // `auto` means on for a model that runs on this machine, which the engine cannot
            // tell here (it has no local runtime), so the request says it.
            harness: match flags.harness {
                HarnessMode::Auto if options.model.local => HarnessMode::On,
                other => other,
            },
            ..flags
        };
        Ok(Prepared {
            run: Arc::new(EvalRun {
                record,
                replay,
                variant,
                asks: Mutex::new(Asks::default()),
                progress: Mutex::new(Progress::default()),
                ephemeral_env: Mutex::new(None),
            }),
            model,
            extra_body,
            placeholders,
            pricing,
            gate,
            tools: vec![web],
            flags,
            permission_mode,
            effort,
            agent: options.agent.clone(),
            timeout_s: options.timeout_s,
            subagent_replay,
            preview,
        })
    }
}

/// Web search with no results: the tool is offered (its schema is part of the prompt under test)
/// but nothing leaves the machine.
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

impl EvalRun {
    /// What the session recorded, with the prompt's identity from its first request.
    pub(crate) fn result(&self, workspace_root: &str) -> EvalResult {
        let record = record::lock(&self.record);
        let (prompt_hash, template) = match &record.first_root {
            Some(first) => {
                let template = first
                    .system_prompt
                    .replace(workspace_root, WORKSPACE_PLACEHOLDER);
                (
                    Some(variant::prompt_hash(&template, &first.tools)),
                    Some(template),
                )
            }
            None => (None, None),
        };
        let asks = self.asks.lock().unwrap_or_else(PoisonError::into_inner);
        let progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        EvalResult {
            variant_config_hash: variant::config_hash(&self.variant),
            prompt_hash,
            system_prompt_template: template,
            first_request: record.first_root.clone(),
            last_request: record.last_root.clone(),
            last_reply: record.last_reply.clone(),
            root: record.root,
            subagents: record.subagents,
            estimated_calls: record.estimated_calls,
            warnings: record.warnings.clone(),
            errors: record.errors.clone(),
            ceiling: record.ceiling.map(str::to_string),
            answered: self
                .replay
                .as_ref()
                .map(|replay| replay.answered())
                .unwrap_or_default(),
            replay: self.replay.as_ref().map(|replay| replay.summary()),
            permissions: asks.permissions.clone(),
            questions: asks.questions.clone(),
            workspace_root: workspace_root.to_string(),
            ended: progress.ended.clone(),
            plan: progress.plan.clone(),
        }
    }

    /// Takes in one of the session's events: how the root's turn ended, and its plan.
    pub(crate) fn observe(&self, event: &AgentEvent) {
        if event.agent_id != ROOT_AGENT && !matches!(event.event, SessionEvent::Paused { .. }) {
            return;
        }
        let mut progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        let end = |reason: &str, message: Option<String>| EvalEnd {
            reason: reason.to_string(),
            message,
        };
        match &event.event {
            SessionEvent::PlanUpdated { plan } => {
                progress.plan = serde_json::to_value(plan).ok();
            }
            SessionEvent::TurnCompleted { .. } => {
                progress.ended.get_or_insert_with(|| end("completed", None));
            }
            SessionEvent::TurnFailed { message } => {
                progress
                    .ended
                    .get_or_insert_with(|| end("failed", Some(message.clone())));
            }
            SessionEvent::Paused { reason, .. } => {
                progress
                    .ended
                    .get_or_insert_with(|| end("paused", Some(reason.clone())));
            }
            _ => {}
        }
    }

    /// The daemon stopped the session at its deadline.
    pub(crate) fn mark_timeout(&self) {
        let mut progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        progress.ended.get_or_insert(EvalEnd {
            reason: "timeout".into(),
            message: None,
        });
    }

    /// Remembers the environment this run made for its folder.
    pub(crate) fn set_ephemeral_env(&self, id: Option<String>) {
        *self
            .ephemeral_env
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = id;
    }

    /// The environment this run made, once.
    pub(crate) fn take_ephemeral_env(&self) -> Option<String> {
        self.ephemeral_env
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The first request's prompt and tools, for a preview.
    pub(crate) fn preview(&self, workspace_root: &str) -> Option<PromptPreview> {
        let result = self.result(workspace_root);
        let first = result.first_request?;
        let template = result.system_prompt_template?;
        Some(PromptPreview {
            variant_config_hash: result.variant_config_hash,
            prompt_hash: result.prompt_hash?,
            system_prompt: template,
            tools: first.tools,
            warnings: result.warnings,
        })
    }
}

/// Answers what an evaluation session asks, because nobody is there: a permission request is
/// denied and a batch of questions gets its recommended answers. It looks at the session's
/// history again after a lag, so a request whose event was dropped is still answered. It holds
/// the session only while it answers, so deleting the session ends it.
pub(crate) fn watch(session: &Arc<Session>, run: Arc<EvalRun>) {
    let (history, mut events) = session.subscribe_with_history();
    let mut seen = history.len();
    let weak = Arc::downgrade(session);
    tokio::spawn(async move {
        let mut batch = history;
        loop {
            let Some(session) = weak.upgrade() else {
                break;
            };
            for event in batch.drain(..) {
                answer(&session, &run, event);
            }
            drop(session);
            match events.recv().await {
                Ok(event) => {
                    seen += 1;
                    batch.push(event);
                }
                Err(RecvError::Lagged(_)) => {
                    let Some(session) = weak.upgrade() else {
                        break;
                    };
                    let (history, fresh) = session.subscribe_with_history();
                    events = fresh;
                    let missed = history.len().saturating_sub(seen);
                    let from = history.len() - missed;
                    seen = history.len();
                    batch = history.into_iter().skip(from).collect();
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

fn answer(session: &Session, run: &EvalRun, event: AgentEvent) {
    run.observe(&event);
    let mut asks = run.asks.lock().unwrap_or_else(PoisonError::into_inner);
    match event.event {
        SessionEvent::PermissionRequested {
            request_id,
            tool,
            summary,
            ..
        } => {
            let _ = session.respond(request_id, PermissionDecision::Deny);
            asks.permissions.push(EvalPermission {
                agent: event.agent_id,
                tool,
                summary,
                decision: "denied".into(),
            });
        }
        SessionEvent::QuestionsAsked {
            request_id,
            questions,
        } => {
            let _ = session.answer(request_id, vec![String::new(); questions.len()]);
            asks.questions.push(EvalQuestion {
                agent: event.agent_id,
                questions: questions.iter().map(|q| q.question.clone()).collect(),
                answered_with: "the recommended answers".into(),
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use imperium_protocol::session::TokenUsage;

    use super::*;

    fn run() -> EvalRun {
        EvalRun {
            record: Shared::default(),
            replay: None,
            variant: Arc::new(EvalVariant::default()),
            asks: Mutex::default(),
            progress: Mutex::default(),
            ephemeral_env: Mutex::default(),
        }
    }

    fn event(agent: &str, event: SessionEvent) -> AgentEvent {
        AgentEvent {
            agent_id: agent.into(),
            event,
        }
    }

    /// The daemon keeps how the root's turn ended itself, so a client that missed the
    /// notification can still ask.
    #[test]
    fn the_end_of_the_root_turn_is_recorded_from_the_sessions_events() {
        let run = run();
        run.observe(&event(
            "t1",
            SessionEvent::TurnFailed {
                message: "a subagent".into(),
            },
        ));
        assert!(run.result("/w").ended.is_none());
        run.observe(&event(
            "root",
            SessionEvent::TurnCompleted {
                usage: TokenUsage::default(),
                cost_usd: None,
                steps: 2,
            },
        ));
        run.observe(&event(
            "root",
            SessionEvent::TurnFailed {
                message: "later".into(),
            },
        ));
        let ended = run.result("/w").ended.unwrap();
        assert_eq!((ended.reason.as_str(), ended.message), ("completed", None));
    }

    #[test]
    fn a_deadline_ends_the_run_once() {
        let run = run();
        run.mark_timeout();
        run.observe(&event(
            "root",
            SessionEvent::TurnFailed {
                message: "stopped".into(),
            },
        ));
        assert_eq!(run.result("/w").ended.unwrap().reason, "timeout");
    }
}
