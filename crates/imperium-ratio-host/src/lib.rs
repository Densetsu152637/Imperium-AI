//! THROWAWAY SPIKE, never merged: Imperium's `RatioHost` against Ratio's stage 11a `Host`
//! trait (`feat/wt-s11a-host` at `7e73f9d`, `HOST_API_VERSION` 1), to validate the trait before
//! Ratio fixes it in a release.
//!
//! What it wires to Imperium's seams:
//!
//! - tools: Imperium's [`ToolProvider`]s (`imperium_api`) run the calls, after the frame's
//!   scope (from Imperium's own frame record), the per-call secrecy flag and Imperium's
//!   permission decision (`imperium_policy::permissions::decide_call` and the safety floor);
//!   the cancellation classes are Imperium's own [`ClassTable`];
//! - approvals: an [`ApprovalDesk`] adapter mirroring `imperium_core::approvals::Approvals`
//!   (that type needs the daemon's state), with withdrawal atomic with the answer;
//! - models: Imperium's [`ChatClient`]s (`imperium_models`) and the memory [`Embedder`]
//!   (`imperium_memory`);
//! - token counting: Imperium's estimate (the exact tokenizer is asynchronous; the trait is not);
//! - secret paths: the secret-holding patterns of Imperium's safety floor.
//!
//! Every asynchronous call runs as a task on Imperium's tokio runtime, and the returned future
//! awaits it: Ratio (and its contract suite) poll host futures from any executor, with no
//! tokio context, so nothing here may touch tokio's timers outside a spawned task.

// The host this spike was written for (chapter 10, "Versions").
const _: () = assert!(ratio::HOST_API_VERSION == 1);

pub mod approvals;
mod frames;
pub mod models;
pub mod tape;
pub mod tools;

use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Instant;

use imperium_api::ChatMessage;
use imperium_api::{PermissionMode, ToolContext, ToolEvent, ToolGroup, ToolProvider};
use imperium_models::ChatRequest;
use imperium_policy::permissions::{Decision, Refusal};
use ratio::frame::{FrameCtx, Location, Scope};
use ratio::host::{
    ApprovalKind, ApprovalRequest, CancelClass, CancelOutcome, ChatRequest as RatioChatRequest,
    ConvId, ConvSelector, Countable, DeniedBy, Digest, EffectCtx, FrameOutcome, Host, HostError,
    HostFuture, Labelled, MAX_DRAIN, ModelCard, ModelRef, ModelRequest, ModelResponse, Operation,
    RatioEvent, SinkRule, ToolClass, ToolInvocation, ToolOutcome, Usage, operation_rule, sink_rule,
};
use ratio::{Labels, Value};

pub use approvals::{Answer, ApprovalDesk, ApprovalInfo};
pub use imperium_memory::retrieval::Embedder;
pub use imperium_models::ChatClient;
pub use models::{EmbedderEntry, ModelEntry};
pub use tools::{ClassTable, DEFAULT_SECRET_PATHS};

use approvals::Pending;
use frames::FrameRecords;

/// How a session's `RatioHost` is built.
pub struct HostConfig {
    /// Imperium's runtime: every asynchronous call runs as a task on it.
    pub runtime: tokio::runtime::Handle,
    /// The root of the session's frame paths (`turn`) and the session's scope.
    pub root: String,
    pub scope: Scope,
    /// The project folder the tools work in.
    pub project_root: PathBuf,
    pub providers: Vec<Arc<dyn ToolProvider>>,
    pub classes: ClassTable,
    pub models: Vec<ModelEntry>,
    pub embedder: Option<EmbedderEntry>,
    /// `ratio.secret_paths`.
    pub secret_paths: Vec<String>,
    pub mode: PermissionMode,
}

impl HostConfig {
    /// A session with no tools or models, Imperium's built-in classes and secret paths.
    pub fn new(runtime: tokio::runtime::Handle, root: &str, scope: Scope) -> HostConfig {
        HostConfig {
            runtime,
            root: root.to_owned(),
            scope,
            project_root: PathBuf::from("."),
            providers: Vec::new(),
            classes: ClassTable::default(),
            models: Vec::new(),
            embedder: None,
            secret_paths: DEFAULT_SECRET_PATHS
                .iter()
                .map(|p| (*p).to_owned())
                .collect(),
            mode: PermissionMode::Default,
        }
    }
}

#[derive(Debug, Default)]
struct Remembered {
    allowed: HashSet<String>,
    denied: HashSet<String>,
}

/// Imperium's host for Ratio turns.
pub struct RatioHost {
    runtime: tokio::runtime::Handle,
    project_root: PathBuf,
    frames: FrameRecords,
    providers: Vec<Arc<dyn ToolProvider>>,
    classes: ClassTable,
    models: Vec<ModelEntry>,
    embedder: Option<EmbedderEntry>,
    secret_paths: Vec<String>,
    mode: PermissionMode,
    desk: ApprovalDesk,
    remembered: Mutex<Remembered>,
    stopping: AtomicBool,
    seed: [u8; 32],
    events: Mutex<Vec<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A turn's seed. The spike draws it from the standard library's per-process random keys; the
/// real host would use the OS generator.
fn fresh_seed() -> [u8; 32] {
    use std::hash::{BuildHasher, Hasher};
    let mut seed = [0u8; 32];
    for (i, chunk) in seed.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    seed
}

/// The text a labelled part is shown as, naming its label sources (as Ratio's guard shows it).
pub fn placeholder(labels: Labels) -> String {
    let sources: Vec<String> = (0..32)
        .filter(|i| labels.contains(Labels::source(*i)))
        .map(|i: u32| i.to_string())
        .collect();
    format!("[secret: source {}]", sources.join(", "))
}

fn ready<T: Send + 'static>(r: Result<T, HostError>) -> HostFuture<Result<T, HostError>> {
    Box::pin(std::future::ready(r))
}

fn timeout(what: &str) -> HostError {
    HostError::Timeout {
        message: format!("`{what}` passed its deadline or its declared bound"),
    }
}

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Resolves at `at`, or never. Only polled inside a task on Imperium's runtime.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending::<()>().await,
    }
}

/// A task on Imperium's runtime, awaited from whatever executor polls the host's future.
struct Task<T> {
    handle: tokio::task::JoinHandle<T>,
    abort_on_drop: bool,
}

impl<T> Future for Task<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.handle).poll(cx)
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if self.abort_on_drop {
            self.handle.abort();
        }
    }
}

enum Ended<T> {
    Done(T),
    Cancelled,
    Deadline,
}

/// A tool call that passed its synchronous checks.
struct ToolRun {
    frame: FrameCtx,
    cx: EffectCtx,
    call: ToolInvocation,
    provider: Arc<dyn ToolProvider>,
    arguments: String,
    gate: Option<Pending>,
    class: ToolClass,
}

fn labelled_args(args: &[(Arc<str>, Value)]) -> bool {
    args.iter().any(|(_, v)| !v.scan().is_empty())
}

impl RatioHost {
    pub fn new(config: HostConfig) -> Arc<RatioHost> {
        Arc::new(RatioHost {
            runtime: config.runtime,
            project_root: config.project_root,
            frames: FrameRecords::new(&config.root, config.scope),
            providers: config.providers,
            classes: config.classes,
            models: config.models,
            embedder: config.embedder,
            secret_paths: config.secret_paths,
            mode: config.mode,
            desk: ApprovalDesk::default(),
            remembered: Mutex::new(Remembered::default()),
            stopping: AtomicBool::new(false),
            seed: fresh_seed(),
            events: Mutex::new(Vec::new()),
        })
    }

    /// The session's approvals, for its clients.
    pub fn desk(&self) -> &ApprovalDesk {
        &self.desk
    }

    /// Whether `tool` is allowed for the session, and whether a denial of it was recorded.
    pub fn remembered(&self, tool: &str) -> (bool, bool) {
        let r = lock(&self.remembered);
        (r.allowed.contains(tool), r.denied.contains(tool))
    }

    /// `session.control stop`: from now on a cancelled call drains instead of aborting.
    pub fn stop_session(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    /// Imperium starts a harness at `path` (a harness load or `run_harness`).
    pub fn harness_started(&self, path: &str) {
        self.frames.harness_started(path);
    }

    /// The tools in Imperium's class table.
    pub fn tool_names(&self) -> Vec<String> {
        self.classes.names()
    }

    /// The output-only events shown so far, labelled parts redacted.
    pub fn events(&self) -> Vec<String> {
        lock(&self.events).clone()
    }

    fn spawn<T: Send + 'static>(
        &self,
        abort_on_drop: bool,
        work: impl Future<Output = Result<T, HostError>> + Send + 'static,
    ) -> HostFuture<Result<T, HostError>> {
        let task = Task {
            handle: self.runtime.spawn(work),
            abort_on_drop,
        };
        Box::pin(async move {
            task.await.unwrap_or_else(|e| {
                Err(HostError::Invalid {
                    message: format!("the host's task ended: {e}"),
                })
            })
        })
    }

    fn model(&self, model: &ModelRef) -> Result<&ModelEntry, HostError> {
        self.models
            .iter()
            .find(|m| *m.name == *model.0)
            .ok_or_else(|| HostError::NotFound {
                message: format!("no model `{}`", model.0),
            })
    }

    /// The frame's tools, from Imperium's own record of the frame.
    fn check_scope(&self, frame: &FrameCtx, tool: &str) -> Result<(), HostError> {
        match self.frames.scope_of(&frame.path) {
            Some(scope) if scope.tools.allows(tool) => Ok(()),
            Some(_) => Err(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!("`{tool}` is outside the frame's tools"),
            }),
            None => Err(HostError::Denied {
                by: DeniedBy::Attenuation,
                message: format!("Imperium has no record of the frame `{}`", frame.path),
            }),
        }
    }

    /// The sinks, enforced from the per-call secrecy flag (and the labels themselves, should a
    /// caller have left the flag unset), without relying on Ratio's guard.
    fn check_secrecy(&self, frame: &FrameCtx, call: &ToolInvocation) -> Result<(), HostError> {
        if !call.secret && !labelled_args(&call.args) {
            return Ok(());
        }
        match sink_rule(&call.builtin, &call.tool) {
            SinkRule::Open => Ok(()),
            SinkRule::Refuse => Err(HostError::secrecy(format!(
                "Imperium refuses a secret to `{}`",
                call.tool
            ))),
            SinkRule::UnlessSecretPath => {
                let path = call.arg("path");
                let path_labelled = path.is_some_and(|v| !v.scan().is_empty());
                let text = path.and_then(Value::as_text).unwrap_or_default();
                if !path_labelled && self.is_secret_path(frame, text) {
                    Ok(())
                } else {
                    Err(HostError::secrecy(format!(
                        "Imperium refuses a secret to `{}` outside `ratio.secret_paths`",
                        call.tool
                    )))
                }
            }
        }
    }

    fn provider_for(&self, tool: &str) -> Option<Arc<dyn ToolProvider>> {
        // Outside servers (MCP) are matched first, as the agent loop matches them.
        self.providers
            .iter()
            .find(|p| p.group() == ToolGroup::External && p.handles(tool))
            .or_else(|| self.providers.iter().find(|p| p.handles(tool)))
            .cloned()
    }

    /// Imperium's permission decision; `Some` pending approval when the user is asked.
    fn gate(
        &self,
        call: &ToolInvocation,
        provider: &dyn ToolProvider,
        arguments: &str,
    ) -> Result<Option<Pending>, HostError> {
        let check = provider
            .check(&call.tool, arguments)
            .map_err(|message| HostError::Invalid { message })?;
        let kind = tools::call_kind(&call.tool, &check);
        match tools::decide(self.mode, kind, &check, call, &self.project_root) {
            Decision::Run => Ok(None),
            Decision::Ask { sticky } => {
                if sticky && lock(&self.remembered).allowed.contains(&*call.tool) {
                    return Ok(None);
                }
                Ok(Some(self.desk.request("tool", check.summary)))
            }
            Decision::Refuse(Refusal::Rule) => Err(HostError::Denied {
                by: DeniedBy::Rule,
                message: format!("a deny rule refuses `{}`", call.tool),
            }),
            Decision::Refuse(Refusal::Plan) => Err(HostError::Denied {
                by: DeniedBy::Mode,
                message: format!("plan mode refuses `{}`", call.tool),
            }),
            Decision::Refuse(Refusal::Configuration) => Err(HostError::Denied {
                by: DeniedBy::Floor,
                message: format!("`{}` would write Imperium's configuration", call.tool),
            }),
        }
    }

    /// Waits for the user's answer, or withdraws the request (atomically with the answer)
    /// when the token fires or the deadline passes.
    async fn wait_for_answer(
        &self,
        pending: Pending,
        cx: &EffectCtx,
        what: &str,
    ) -> Result<Answer, HostError> {
        let Pending { id, mut receiver } = pending;
        let waited = tokio::select! {
            a = &mut receiver => Ended::Done(a.ok()),
            () = cx.cancel.cancelled() => Ended::Cancelled,
            () = sleep_until(cx.deadline) => Ended::Deadline,
        };
        let answer = match waited {
            Ended::Done(a) => a,
            Ended::Cancelled => {
                if self.desk.withdraw(id) {
                    // Withdrawn: it never runs, and nothing is remembered.
                    return Err(HostError::Cancelled(CancelOutcome::Withdrawn));
                }
                receiver.await.ok()
            }
            Ended::Deadline => {
                if self.desk.withdraw(id) {
                    return Err(timeout(what));
                }
                receiver.await.ok()
            }
        };
        answer.ok_or_else(|| HostError::Denied {
            by: DeniedBy::User,
            message: format!("the approval of `{what}` was dropped"),
        })
    }

    async fn run_tool(self: Arc<Self>, run: ToolRun) -> Result<ToolOutcome, HostError> {
        let ToolRun {
            frame,
            cx,
            call,
            provider,
            arguments,
            gate,
            class,
        } = run;
        let tool = call.tool.to_string();
        if let Some(pending) = gate {
            let answer = self.wait_for_answer(pending, &cx, &tool).await?;
            {
                let mut r = lock(&self.remembered);
                if answer.approve && answer.for_session {
                    r.allowed.insert(tool.clone());
                }
                if !answer.approve {
                    r.denied.insert(tool.clone());
                }
            }
            if !answer.approve {
                return Err(HostError::Denied {
                    by: DeniedBy::User,
                    message: format!("the user refused `{tool}`"),
                });
            }
        }
        let root = self.project_root.clone();
        let emit = |_: ToolEvent| {};
        let context = ToolContext {
            root: &root,
            emit: &emit,
            sampler: None,
        };
        let mut work = provider.call(&context, &tool, &arguments);
        let result = match class.class {
            CancelClass::Abort => {
                let ended = tokio::select! {
                    r = &mut work => Ended::Done(r),
                    () = cx.cancel.cancelled() => Ended::Cancelled,
                    () = sleep_until(cx.deadline) => Ended::Deadline,
                };
                match ended {
                    Ended::Done(r) => r,
                    Ended::Deadline => return Err(timeout(&tool)),
                    Ended::Cancelled if !self.stopping.load(Ordering::SeqCst) => {
                        return Err(HostError::Cancelled(if class.remote {
                            CancelOutcome::Abandoned
                        } else {
                            CancelOutcome::Aborted
                        }));
                    }
                    Ended::Cancelled => {
                        // After a session stop every in-flight call drains, bounded.
                        let limit = earliest(Some(Instant::now() + MAX_DRAIN), cx.deadline);
                        let drained = tokio::select! {
                            _ = &mut work => true,
                            () = sleep_until(limit) => false,
                        };
                        return Err(if drained {
                            HostError::Cancelled(CancelOutcome::Drained)
                        } else {
                            timeout(&tool)
                        });
                    }
                }
            }
            CancelClass::Drain => {
                // Runs to its end, its deadline or its declared bound (failed there).
                let bound = class.max_running.unwrap_or(MAX_DRAIN).min(MAX_DRAIN);
                let limit = earliest(Some(Instant::now() + bound), cx.deadline);
                let ended = tokio::select! {
                    r = &mut work => Ended::Done(r),
                    () = sleep_until(limit) => Ended::Deadline,
                };
                match ended {
                    // Cancelled while it ran: it ran to its end, and its result is discarded.
                    Ended::Done(_) if cx.cancel.is_cancelled() => {
                        return Err(HostError::Cancelled(CancelOutcome::Drained));
                    }
                    Ended::Done(r) => r,
                    Ended::Deadline | Ended::Cancelled => return Err(timeout(&tool)),
                }
            }
        };
        match result {
            Ok(text) => {
                let labels = self.source_labels(&frame, &call.builtin, &call.args);
                Ok(ToolOutcome {
                    value: Value::text(&text).with_labels(labels),
                })
            }
            Err(message) => Err(HostError::ToolFailed {
                message: Labelled::plain(message),
                data: Value::unit(),
            }),
        }
    }

    /// `session.ask`: an `@user` built-in, asked live even in replay. A labelled question is
    /// shown with its labelled parts as `[secret: source N]`.
    fn ask(self: Arc<Self>, cx: EffectCtx, op: &Operation) -> HostFuture<Result<Value, HostError>> {
        let question = op
            .args
            .iter()
            .find(|(n, _)| matches!(&**n, "question" | "text" | "prompt"))
            .or_else(|| op.args.first())
            .map(|(_, v)| v.clone())
            .unwrap_or_else(Value::unit);
        let labels = question.scan();
        let shown = if labels.is_empty() {
            question.as_text().unwrap_or_default().to_owned()
        } else {
            placeholder(labels)
        };
        let pending = self.desk.request("question", shown);
        let host = Arc::clone(&self);
        self.spawn(true, async move {
            let answer = host.wait_for_answer(pending, &cx, "session.ask").await?;
            Ok(Value::text(answer.text.as_deref().unwrap_or_default()))
        })
    }
}

impl Host for RatioHost {
    fn generate(
        self: Arc<Self>,
        _frame: FrameCtx,
        cx: EffectCtx,
        req: ModelRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        let entry = match self.model(&req.model) {
            Ok(e) => e.clone(),
            Err(e) => return ready(Err(e)),
        };
        let labels = req
            .options
            .iter()
            .fold(req.prompt.scan(), |acc, (_, v)| acc | v.scan());
        if (req.secret || !labels.is_empty()) && entry.location != Location::Local {
            return ready(Err(HostError::secrecy(format!(
                "Imperium refuses a secret to the hosted model `{}`",
                req.model.0
            ))));
        }
        // Rendering a `Prompt` is stage 12's (chapter 05); the spike sends text prompts only.
        let Some(prompt) = req.prompt.as_text().map(str::to_owned) else {
            return ready(Err(HostError::unsupported(
                "the spike renders text prompts only (prompt rendering is stage 12)",
            )));
        };
        let remote = entry.location != Location::Local;
        let max_tokens = req.max_tokens.map(|m| u32::try_from(m).unwrap_or(u32::MAX));
        self.spawn(true, async move {
            let messages = vec![ChatMessage::user(prompt)];
            let request = ChatRequest {
                model: &entry.model_id,
                messages: &messages,
                tools: &[],
                slot: None,
                max_tokens,
                effort: None,
            };
            let mut on_text = |_: &str| {};
            let mut work = entry.client.stream(request, &mut on_text);
            let ended = tokio::select! {
                r = &mut work => Ended::Done(r),
                () = cx.cancel.cancelled() => Ended::Cancelled,
                () = sleep_until(cx.deadline) => Ended::Deadline,
            };
            match ended {
                Ended::Done(Ok(c)) => {
                    let usage = c.usage.unwrap_or_default();
                    Ok(ModelResponse {
                        // A secret prompt reaches a local model only, and its reply is secret.
                        text: Labelled::new(c.text, labels),
                        usage: Usage {
                            input_tokens: usage.prompt_tokens,
                            output_tokens: usage.completion_tokens,
                            cost: 0,
                        },
                    })
                }
                Ended::Done(Err(e)) if e.is_usage_limit() => Err(HostError::RateLimited {
                    message: e.to_string(),
                }),
                Ended::Done(Err(e)) => Err(HostError::ModelFailed {
                    message: Labelled::plain(e.to_string()),
                }),
                Ended::Cancelled => Err(HostError::Cancelled(if remote {
                    CancelOutcome::Abandoned
                } else {
                    CancelOutcome::Aborted
                })),
                Ended::Deadline => Err(timeout("model.generate")),
            }
        })
    }

    fn embed(
        self: Arc<Self>,
        _frame: FrameCtx,
        cx: EffectCtx,
        texts: Vec<Labelled<String>>,
        _model: Option<ModelRef>,
    ) -> HostFuture<Result<Vec<Vec<f32>>, HostError>> {
        let Some(entry) = self.embedder.clone() else {
            return ready(Err(HostError::unsupported(
                "no embedding model is configured (`memory.embedding_model`)",
            )));
        };
        if texts.iter().any(Labelled::is_labelled) && entry.location != Location::Local {
            return ready(Err(HostError::secrecy(
                "Imperium refuses a secret to a hosted embedding model",
            )));
        }
        let texts: Vec<String> = texts.into_iter().map(|t| t.value).collect();
        let runtime = self.runtime.clone();
        self.spawn(true, async move {
            let embedder = Arc::clone(&entry.embedder);
            let mut work = runtime.spawn_blocking(move || embedder.embed(&texts));
            let ended = tokio::select! {
                r = &mut work => Ended::Done(r),
                () = cx.cancel.cancelled() => Ended::Cancelled,
                () = sleep_until(cx.deadline) => Ended::Deadline,
            };
            match ended {
                Ended::Done(Ok(Ok(v))) => Ok(v),
                Ended::Done(Ok(Err(message))) => Err(HostError::ModelFailed {
                    message: Labelled::plain(message),
                }),
                Ended::Done(Err(e)) => Err(HostError::ModelFailed {
                    message: Labelled::plain(e.to_string()),
                }),
                Ended::Cancelled => Err(HostError::Cancelled(CancelOutcome::Aborted)),
                Ended::Deadline => Err(timeout("model.embed")),
            }
        })
    }

    fn count_tokens(&self, _frame: &FrameCtx, what: &Countable) -> u64 {
        match what {
            Countable::Text(t) => models::estimate_tokens(&t.value),
            Countable::Prompt(v) => v
                .as_text()
                .map_or_else(|| v.weight().div_ceil(4), models::estimate_tokens),
        }
    }

    fn resolve_model(&self, _frame: &FrameCtx, model: &ModelRef) -> Result<ModelCard, HostError> {
        let entry = self.model(model)?;
        Ok(ModelCard {
            name: Arc::from(entry.name.as_str()),
            location: entry.location,
            card: entry.card,
        })
    }

    fn call_tool(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        call: ToolInvocation,
    ) -> HostFuture<Result<ToolOutcome, HostError>> {
        // Synchronous checks first, in Imperium's order: the frame's scope from its own
        // record, the secrecy flag, then the permission decision. An approval is queued here,
        // before the future is first polled.
        if let Err(e) = self
            .check_scope(&frame, &call.tool)
            .and_then(|()| self.check_secrecy(&frame, &call))
        {
            return ready(Err(e));
        }
        let Some(provider) = self.provider_for(&call.tool) else {
            return ready(Err(HostError::NotFound {
                message: format!("no tool `{}`", call.tool),
            }));
        };
        let arguments = tools::arguments_json(&call);
        let gate = match self.gate(&call, provider.as_ref(), &arguments) {
            Ok(g) => g,
            Err(e) => return ready(Err(e)),
        };
        let class = self.classes.class(&call.tool);
        let run = ToolRun {
            frame,
            cx,
            call,
            provider,
            arguments,
            gate,
            class,
        };
        // A drain call runs to its bound even if its future is dropped.
        let abort_on_drop = class.class == CancelClass::Abort;
        let host = Arc::clone(&self);
        self.spawn(abort_on_drop, host.run_tool(run))
    }

    fn tool_class(&self, _frame: &FrameCtx, tool: &str) -> ToolClass {
        self.classes.class(tool)
    }

    fn is_secret_path(&self, _frame: &FrameCtx, path: &str) -> bool {
        tools::matches_secret_path(&self.secret_paths, path)
    }

    fn source_labels(&self, frame: &FrameCtx, builtin: &str, args: &[(Arc<str>, Value)]) -> Labels {
        // A read of a secret path is secret (label source 0 is `ratio.secret_paths` here).
        let reads = matches!(
            builtin,
            "tools.read_file" | "tools.grep" | "tools.list_dir" | "tools.glob"
        );
        let path = args
            .iter()
            .find(|(n, _)| &**n == "path")
            .and_then(|(_, v)| v.as_text())
            .unwrap_or_default();
        if reads && self.is_secret_path(frame, path) {
            Labels::source(0)
        } else {
            Labels::NONE
        }
    }

    fn conversation(&self, _frame: &FrameCtx, which: ConvSelector) -> Result<ConvId, HostError> {
        // Conversations are the engine's (`imperium-engine`); the spike names two empty ones.
        Ok(ConvId(u64::from(which == ConvSelector::Main)))
    }

    fn conversation_digest(&self, _frame: &FrameCtx, conv: ConvId) -> Result<Digest, HostError> {
        if conv.0 > 1 {
            return Err(HostError::NotFound {
                message: format!("no conversation {}", conv.0),
            });
        }
        Ok(Digest([0; 32]))
    }

    fn conversation_labels(&self, _frame: &FrameCtx, conv: ConvId) -> Result<Labels, HostError> {
        if conv.0 > 1 {
            return Err(HostError::NotFound {
                message: format!("no conversation {}", conv.0),
            });
        }
        Ok(Labels::NONE)
    }

    fn chat(
        self: Arc<Self>,
        _frame: FrameCtx,
        _cx: EffectCtx,
        _conv: ConvId,
        req: RatioChatRequest,
    ) -> HostFuture<Result<ModelResponse, HostError>> {
        ready(Err(HostError::unsupported(format!(
            "`{}`: engine conversations are not wired in the spike",
            req.builtin
        ))))
    }

    fn open_frame(&self, parent: &FrameCtx, child: &FrameCtx) -> Result<(), HostError> {
        self.frames.open(parent, child)
    }

    fn close_frame(&self, frame: &FrameCtx, _outcome: FrameOutcome) {
        self.frames.close(&frame.path);
    }

    fn operate(
        self: Arc<Self>,
        _frame: FrameCtx,
        cx: EffectCtx,
        op: Operation,
    ) -> HostFuture<Result<Value, HostError>> {
        if &*op.builtin == "session.ask" {
            return self.ask(cx, &op);
        }
        if (op.secret || labelled_args(&op.args)) && operation_rule(&op.builtin) == SinkRule::Refuse
        {
            return ready(Err(HostError::secrecy(format!(
                "Imperium refuses a secret to `{}`",
                op.builtin
            ))));
        }
        ready(Err(HostError::unsupported(format!(
            "`{}` is not wired in the spike",
            op.builtin
        ))))
    }

    fn approve(
        self: Arc<Self>,
        frame: FrameCtx,
        cx: EffectCtx,
        req: ApprovalRequest,
    ) -> HostFuture<Result<bool, HostError>> {
        let attended = self
            .frames
            .scope_of(&frame.path)
            .is_some_and(|s| s.attended);
        if !attended {
            return ready(Err(HostError::Denied {
                by: DeniedBy::Unattended,
                message: "no one to ask in an unattended frame".to_owned(),
            }));
        }
        let kind = match req.kind {
            ApprovalKind::Declassify => "declassify",
            ApprovalKind::Request => "request",
        };
        // Redacted again here: the user is asked, but Imperium shows no secret.
        let summary = if req.summary.is_labelled() {
            placeholder(req.summary.labels)
        } else {
            req.summary.value.clone()
        };
        let pending = self.desk.request(kind, summary);
        let host = Arc::clone(&self);
        self.spawn(true, async move {
            let answer = host.wait_for_answer(pending, &cx, kind).await?;
            Ok(answer.approve)
        })
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn seed(&self) -> [u8; 32] {
        self.seed
    }

    fn timer(&self, at: Instant) -> HostFuture<()> {
        // Fires on Imperium's runtime, whoever polls it.
        let task = Task {
            handle: self
                .runtime
                .spawn(tokio::time::sleep_until(tokio::time::Instant::from_std(at))),
            abort_on_drop: true,
        };
        Box::pin(async move {
            let _ = task.await;
        })
    }

    fn emit(&self, _frame: &FrameCtx, event: RatioEvent) -> Result<(), HostError> {
        if (event.secret || labelled_args(&event.args))
            && operation_rule(&event.builtin) == SinkRule::Refuse
        {
            return Err(HostError::secrecy(format!(
                "Imperium refuses a secret to `{}`",
                event.builtin
            )));
        }
        let shown: Vec<String> = event
            .args
            .iter()
            .map(|(n, v)| {
                let labels = v.scan();
                if labels.is_empty() {
                    format!("{n}={}", v.as_text().unwrap_or(v.kind_name()))
                } else {
                    format!("{n}={}", placeholder(labels))
                }
            })
            .collect();
        lock(&self.events).push(format!("{} {}", event.builtin, shown.join(" ")));
        Ok(())
    }
}
