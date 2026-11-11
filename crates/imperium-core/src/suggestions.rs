//! Model download suggestions (docs/04-models-and-routing.md#model-download-suggestions): when
//! a session's best available model is poor for what it needs, the daemon offers a model from
//! `[[models.suggestions]]` sized to this machine as a `model_suggestion` event. Nothing is
//! downloaded until the user answers with `session.suggestion_respond`.
//!
//! Each session is watched for the moments it needs a model (a turn starting, a model call's
//! prompt size), and agents can ask with the `suggest_local_model` tool. At most one suggestion
//! is made per need per session; `models.suggest_downloads` turns them all off, and "don't ask
//! again" turns that setting off in the user config file.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use imperium_api::{ToolCheck, ToolContext, ToolFuture, ToolProvider, ToolSpec, parse_arguments};
use imperium_local::suggest::{Detected, Need, Signals, candidates_for, detect, pick};
use imperium_protocol::jsonrpc::{ErrorObject, Notification, codes};
use imperium_protocol::methods::{
    DownloadJob, MODEL_DOWNLOAD_PROGRESS, ModelDownloadParams, SuggestionRespondParams,
    SuggestionRespondResult,
};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use tokio::sync::broadcast;

use crate::DaemonState;

/// The setting suggestions follow, and that "don't ask again" turns off.
pub const SETTING: &str = "models.suggest_downloads";
const REVISION: &str = "main";

/// Suggestion state for every session: what was asked, and offers waiting for an answer.
#[derive(Debug)]
pub struct Suggestions {
    /// `models.suggest_downloads`, kept live: `config.set` and "don't ask again" change it.
    enabled: AtomicBool,
    next: AtomicU64,
    sessions: Mutex<HashMap<String, PerSession>>,
}

#[derive(Debug, Default)]
struct PerSession {
    asked: HashSet<Need>,
    pending: HashMap<String, Offer>,
}

/// What a waiting suggestion would download.
#[derive(Debug, Clone)]
struct Offer {
    repo: String,
    quant: String,
    mmproj: bool,
}

/// What became of a request to suggest a model.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The `model_suggestion` event sent to the session.
    Offered(SessionEvent),
    /// `models.suggest_downloads` is off.
    Off,
    /// This session already had a suggestion for the need.
    AlreadyAsked,
    /// No candidate fits this machine or the Hub could not be read; says why.
    Nothing(String),
}

impl Suggestions {
    pub fn new(enabled: bool) -> Self {
        Suggestions {
            enabled: AtomicBool::new(enabled),
            next: AtomicU64::new(0),
            sessions: Mutex::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, PerSession>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn asked(&self, session_id: &str, need: Need) -> bool {
        self.lock()
            .get(session_id)
            .is_some_and(|session| session.asked.contains(&need))
    }

    /// Records that the session is being asked about `need`; false if it already was.
    fn claim(&self, session_id: &str, need: Need) -> bool {
        self.lock()
            .entry(session_id.to_string())
            .or_default()
            .asked
            .insert(need)
    }

    fn take(&self, session_id: &str, id: &str) -> Option<Offer> {
        self.lock().get_mut(session_id)?.pending.remove(id)
    }

    fn put(&self, session_id: &str, id: String, offer: Offer) {
        self.lock()
            .entry(session_id.to_string())
            .or_default()
            .pending
            .insert(id, offer);
    }
}

/// Watches a session's root agent for the moments it needs a model: each turn's start (with the
/// user's message) and each model call's prompt size.
pub fn watch(
    state: &Arc<DaemonState>,
    session_id: String,
    mut events: broadcast::Receiver<AgentEvent>,
) {
    let daemon = Arc::downgrade(state);
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if event.agent_id != ROOT_AGENT {
                continue;
            }
            let (text, prompt_tokens) = match event.event {
                SessionEvent::TurnStarted { text, .. } => (Some(text), None),
                SessionEvent::ContextBreakdown {
                    prompt_tokens: Some(tokens),
                    ..
                } => (None, Some(tokens)),
                _ => continue,
            };
            let Some(state) = daemon.upgrade() else {
                return;
            };
            if !state.suggestions().enabled() {
                continue;
            }
            // The Hub listing is slow; the session's events keep flowing meanwhile.
            let session_id = session_id.clone();
            tokio::spawn(async move {
                if let Some(detected) =
                    need_now(&state, &session_id, text.as_deref(), prompt_tokens)
                {
                    let outcome = offer(&state, &session_id, detected).await;
                    if let Outcome::Nothing(why) = outcome {
                        tracing::info!(session = %session_id, %why, "no model to suggest");
                    }
                }
            });
        }
    });
}

/// What the session needs right now, if anything (see [`detect`]). Routing is consulted only
/// when the other rules found nothing and no general model was suggested yet.
fn need_now(
    state: &DaemonState,
    session_id: &str,
    text: Option<&str>,
    prompt_tokens: Option<u64>,
) -> Option<Detected> {
    let session = state.session(session_id)?;
    let model = session.model().unwrap_or_default();
    let (prefer_local, local) = state.suggestion_inputs();
    let session_local = model.starts_with("llamacpp:") || model.starts_with("vllm:");
    let signals = Signals {
        prefer_local,
        session_local,
        routed_local: true,
        local: &local,
        text,
        prompt_tokens,
    };
    if let Some(detected) = detect(&signals) {
        return Some(detected);
    }
    if !prefer_local || state.suggestions().asked(session_id, Need::Local) {
        return None;
    }
    let routed_local = state.routes_locally(&model)?;
    detect(&Signals {
        routed_local,
        ..signals
    })
}

/// Offers a model for `detected` to the session, unless suggestions are off or the session was
/// already asked about that need: lists the matching candidates on the Hub, picks the
/// quantisation that fits best, and sends a `model_suggestion` event.
pub async fn offer(state: &Arc<DaemonState>, session_id: &str, detected: Detected) -> Outcome {
    let suggestions = state.suggestions();
    if !suggestions.enabled() {
        return Outcome::Off;
    }
    if !suggestions.claim(session_id, detected.need) {
        return Outcome::AlreadyAsked;
    }
    let settings = state.settings();
    let candidates = candidates_for(
        &settings.models.suggestions,
        detected.need,
        detected.context_tokens,
    );
    if candidates.is_empty() {
        return Outcome::Nothing(format!(
            "models.suggestions has no repository for {}",
            detected.need.as_str()
        ));
    }
    let budget = match state.local_budget() {
        Some(budget) => budget,
        None => {
            let probing = Arc::clone(state);
            match tokio::task::spawn_blocking(move || {
                crate::local::probed_budget(&probing.settings())
            })
            .await
            {
                Ok(budget) => budget,
                Err(error) => return Outcome::Nothing(error.to_string()),
            }
        }
    };
    let hub = state.hub();
    let mut listed = Vec::new();
    let mut errors = Vec::new();
    for candidate in candidates {
        match hub.files(&candidate.repo, REVISION).await {
            Ok(files) => listed.push((candidate, files)),
            Err(error) => errors.push(format!("{}: {error}", candidate.repo)),
        }
    }
    let Some(chosen) = pick(&listed, budget) else {
        return Outcome::Nothing(if errors.is_empty() {
            "no suggested model fits this machine".into()
        } else {
            errors.join("; ")
        });
    };
    let Some(session) = state.session(session_id) else {
        return Outcome::Nothing("the session has ended".into());
    };
    let id = format!(
        "ms-{}",
        suggestions.next.fetch_add(1, Ordering::Relaxed) + 1
    );
    let event = SessionEvent::ModelSuggestion {
        id: id.clone(),
        need: detected.need.as_str().into(),
        repo: chosen.repo.clone(),
        quant: chosen.quant.clone(),
        model: format!("llamacpp:{}", chosen.model_id),
        size_bytes: chosen.size_bytes,
        fit: chosen.fit.as_str().into(),
        reason: detected.reason,
        mmproj: chosen.mmproj,
    };
    suggestions.put(
        session_id,
        id,
        Offer {
            repo: chosen.repo,
            quant: chosen.quant,
            mmproj: chosen.mmproj,
        },
    );
    session.notify(event.clone());
    Outcome::Offered(event)
}

/// A `model_suggestion_update` event.
fn update(
    id: &str,
    state: &str,
    job_id: Option<String>,
    model: Option<String>,
    message: Option<String>,
) -> SessionEvent {
    SessionEvent::ModelSuggestionUpdate {
        id: id.to_string(),
        state: state.to_string(),
        job_id,
        model,
        message,
    }
}

/// `session.suggestion_respond`.
pub async fn respond(
    state: Arc<DaemonState>,
    params: SuggestionRespondParams,
) -> Result<SuggestionRespondResult, ErrorObject> {
    let session = state.session(&params.session_id).ok_or_else(|| {
        ErrorObject::new(
            codes::SESSION_NOT_FOUND,
            format!("no session {:?}", params.session_id),
        )
    })?;
    if !matches!(params.action.as_str(), "download" | "dismiss" | "never") {
        return Err(ErrorObject::new(
            codes::INVALID_PARAMS,
            format!(
                "unknown action {:?}; use download, dismiss, or never",
                params.action
            ),
        ));
    }
    let suggestions = state.suggestions();
    let id = params.suggestion_id;
    let offer = suggestions.take(&params.session_id, &id).ok_or_else(|| {
        ErrorObject::new(
            codes::INVALID_PARAMS,
            format!("no suggestion {id:?} is waiting for an answer"),
        )
    })?;
    match params.action.as_str() {
        "dismiss" => {
            session.notify(update(&id, "dismissed", None, None, None));
            Ok(SuggestionRespondResult::default())
        }
        "never" => {
            suggestions.set_enabled(false);
            let saved = imperium_config::set_user_setting(
                state.paths(),
                SETTING,
                &serde_json::Value::Bool(false),
                std::env::vars(),
            );
            let message = match saved {
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(%error, "could not save models.suggest_downloads");
                    Some(format!(
                        "turned off until the daemon restarts; saving it failed: {error}"
                    ))
                }
            };
            session.notify(update(&id, "disabled", None, None, message));
            Ok(SuggestionRespondResult::default())
        }
        _ => {
            // Subscribed before the job starts, so its end cannot be missed.
            let announcements = state.announcements();
            let started = crate::downloads::start(
                Arc::clone(&state),
                ModelDownloadParams {
                    repo: offer.repo.clone(),
                    quant: Some(offer.quant.clone()),
                    file: None,
                    revision: None,
                    mmproj: offer.mmproj,
                },
            )
            .await;
            let job = match started {
                Ok(job) => job,
                Err(error) => {
                    // Still waiting: the user may try again.
                    suggestions.put(&params.session_id, id, offer);
                    return Err(error);
                }
            };
            session.notify(update(
                &id,
                "downloading",
                Some(job.job_id.clone()),
                None,
                None,
            ));
            tokio::spawn(follow(
                Arc::downgrade(&state),
                params.session_id,
                id,
                job.job_id.clone(),
                announcements,
            ));
            Ok(SuggestionRespondResult { job: Some(job) })
        }
    }
}

/// Waits for a suggested download to end, then tells the session: `ready` with the model to
/// switch to (the folders were rescanned first), or `failed` / `cancelled`.
async fn follow(
    daemon: Weak<DaemonState>,
    session_id: String,
    id: String,
    job_id: String,
    mut announcements: broadcast::Receiver<Notification>,
) {
    loop {
        let job: Option<DownloadJob> = match announcements.recv().await {
            Ok(notification) if notification.method == MODEL_DOWNLOAD_PROGRESS => {
                serde_json::from_value(notification.params).ok()
            }
            Ok(_) => None,
            Err(broadcast::error::RecvError::Lagged(_)) => daemon.upgrade().and_then(|state| {
                state
                    .downloads()
                    .list()
                    .into_iter()
                    .find(|job| job.job_id == job_id)
            }),
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let Some(job) = job.filter(|job| job.job_id == job_id) else {
            continue;
        };
        let event = match job.state.as_str() {
            "done" => update(&id, "ready", Some(job.job_id), Some(job.model), None),
            "failed" | "cancelled" => update(&id, &job.state, Some(job.job_id), None, job.message),
            _ => continue,
        };
        if let Some(session) = daemon
            .upgrade()
            .and_then(|state| state.session(&session_id))
        {
            session.notify(event);
        }
        return;
    }
}

/// The `suggest_local_model` tool: lets an agent propose a local model for its task.
#[derive(Debug)]
pub struct SuggestTool {
    pub daemon: Weak<DaemonState>,
    pub session_id: String,
}

const TOOL: &str = "suggest_local_model";

#[derive(Debug, serde::Deserialize)]
struct SuggestArgs {
    task: String,
    #[serde(default)]
    needs: Vec<String>,
    #[serde(default)]
    context_tokens: Option<u64>,
}

/// The most specific need named: vision, code, tools, context, then a general model.
fn strongest(needs: &[String]) -> Option<Need> {
    let parsed: Vec<Need> = needs.iter().filter_map(|need| Need::parse(need)).collect();
    [
        Need::Vision,
        Need::Code,
        Need::Tools,
        Need::Context,
        Need::Local,
    ]
    .into_iter()
    .find(|need| parsed.contains(need))
}

impl ToolProvider for SuggestTool {
    fn specs(&self) -> Vec<ToolSpec> {
        // The definition lives in the protocol's model-facing tool contract.
        let definition = imperium_protocol::tools::suggest_local_model();
        vec![ToolSpec {
            name: definition.name.into(),
            description: definition.description.into(),
            parameters: definition.parameters,
        }]
    }

    fn handles(&self, name: &str) -> bool {
        name == TOOL
    }

    fn check(&self, _name: &str, arguments: &str) -> Result<ToolCheck, String> {
        let args: SuggestArgs = parse_arguments(arguments)?;
        Ok(ToolCheck {
            summary: format!("suggest a local model for {}", args.task),
            // Only a proposal: the user consents to any download separately.
            ask: false,
            needs_context: false,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a ToolContext<'a>,
        _name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: SuggestArgs = parse_arguments(arguments)?;
            let need = strongest(&args.needs)
                .ok_or("needs must name chat, code, vision, tools, or context")?;
            let state = self.daemon.upgrade().ok_or("the daemon is stopping")?;
            let detected = Detected {
                need,
                reason: format!(
                    "the agent asked for a {} model: {}",
                    need.as_str(),
                    args.task
                ),
                context_tokens: (need == Need::Context)
                    .then_some(args.context_tokens)
                    .flatten(),
            };
            Ok(match offer(&state, &self.session_id, detected).await {
                Outcome::Offered(SessionEvent::ModelSuggestion {
                    repo,
                    quant,
                    size_bytes,
                    fit,
                    ..
                }) => format!(
                    "Suggested {repo} ({quant}, {:.1} GB, fits: {fit}) to the user, who decides whether to download it. Carry on with the current model meanwhile.",
                    size_bytes as f64 / 1e9
                ),
                Outcome::Offered(_) => "Suggested a model to the user.".into(),
                Outcome::Off => "Model download suggestions are turned off (models.suggest_downloads); carry on with the current model.".into(),
                Outcome::AlreadyAsked => "A model for this need was already suggested in this session; carry on with the current model.".into(),
                Outcome::Nothing(why) => format!("No model to suggest: {why}. Carry on with the current model."),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_need_is_claimed_once_per_session() {
        let suggestions = Suggestions::new(true);
        assert!(suggestions.claim("a", Need::Vision));
        assert!(!suggestions.claim("a", Need::Vision));
        assert!(suggestions.claim("a", Need::Local));
        assert!(suggestions.claim("b", Need::Vision));
        assert!(suggestions.asked("a", Need::Local));
    }

    #[test]
    fn the_most_specific_need_wins() {
        let needs = |list: &[&str]| list.iter().map(|need| need.to_string()).collect::<Vec<_>>();
        assert_eq!(strongest(&needs(&["chat", "vision"])), Some(Need::Vision));
        assert_eq!(strongest(&needs(&["tools", "code"])), Some(Need::Code));
        assert_eq!(strongest(&needs(&["nonsense"])), None);
    }

    /// The tool offered to models is the protocol's definition, and handled under its name.
    #[test]
    fn the_tool_is_the_protocol_definition() {
        let definition = imperium_protocol::tools::suggest_local_model();
        assert_eq!(TOOL, definition.name);
        let needs = definition.parameters["properties"]["needs"]["items"]["enum"]
            .as_array()
            .expect("needs enum");
        for need in needs {
            assert!(
                Need::parse(need.as_str().unwrap_or_default()).is_some(),
                "{need}"
            );
        }
    }
}
