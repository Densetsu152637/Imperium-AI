//! A local model's calls reserve tokens of its KV pool: the agent estimates each prompt, sends
//! the lease's `max_tokens`, and retries a reply that its reservation cut short (or a pool
//! overflow) once without a cap. The local runtime and the model are scripted fakes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use imperium_api::{FunctionCall, ToolCall, Usage};
use imperium_engine::{
    AcquireFuture, Availability, ChatClient, ChatClients, LocalLease, LocalModels, LocalRequest,
    ResolvedModel, Session, SessionConfig,
};
use imperium_models::{ChatFuture, ChatRequest, Completion, Endpoint, ModelError};
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;

/// The `max_tokens` every capped lease carries.
const CAP: u32 = 777;

/// A loaded model that records what each call asked for.
#[derive(Debug, Default)]
struct Pool {
    requests: Mutex<Vec<LocalRequest>>,
    /// Leases carry no cap, as for a model with no output cap or a prompt that fills the context.
    never_cap: bool,
}

impl LocalModels for Pool {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Loaded
    }

    fn acquire<'a>(&'a self, model: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        self.acquire_with(model, holder, LocalRequest::default())
    }

    fn acquire_with<'a>(
        &'a self,
        _model: &'a str,
        _holder: &'a str,
        request: LocalRequest,
    ) -> AcquireFuture<'a> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            let cap = (!request.uncapped && !self.never_cap).then_some(CAP);
            Ok(LocalLease::new("http://fake/v1".into(), Some(0), cap, ()))
        })
    }
}

/// Answers each request with the next scripted reply and records what it was sent.
#[derive(Debug, Default)]
struct Script {
    replies: Mutex<VecDeque<Result<Completion, ModelError>>>,
    /// The `max_tokens` and the messages (as JSON) of each request.
    seen: Mutex<Vec<(Option<u32>, String)>>,
}

impl ChatClient for Script {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((
                request.max_tokens,
                serde_json::to_string(request.messages).unwrap(),
            ));
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("a scripted reply");
            if let Ok(completion) = &reply {
                on_text(&completion.text);
            }
            reply
        })
    }
}

fn usage(prompt_tokens: u64, completion_tokens: u64) -> Option<Usage> {
    Some(Usage {
        prompt_tokens,
        completion_tokens,
        cached_tokens: 0,
    })
}

fn reply(text: &str, finish_reason: &str) -> Result<Completion, ModelError> {
    Ok(Completion {
        text: text.into(),
        finish_reason: Some(finish_reason.into()),
        usage: usage(900, 50),
        ..Completion::default()
    })
}

fn overflow() -> Result<Completion, ModelError> {
    Err(ModelError::Status {
        status: 400,
        body: "Context size has been exceeded.".into(),
        retry_after_s: None,
    })
}

/// A session on a local model whose server is `script`, behind `pool`.
fn start(pool: &Arc<Pool>, script: &Arc<Script>, dir: &std::path::Path) -> Session {
    let mut clients = ChatClients::default();
    let shared = Arc::clone(script);
    clients.register("scripted", move |_http, _endpoint| {
        Arc::clone(&shared) as Arc<dyn ChatClient>
    });
    let local: Arc<dyn LocalModels> = pool.clone();
    Session::spawn(SessionConfig {
        id: "pool".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "llamacpp:m".into(),
            endpoint: Endpoint {
                base_url: String::new(),
                api_key: None,
            },
            model: "m".into(),
            pricing: None,
            local: true,
            context_window: None,
            kind: "scripted".into(),
            reasoning: None,
        }),
        catalog: Vec::new(),
        clients,
        routing: imperium_config::RoutingSettings::default(),
        local: Some(local),
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        max_steps: 5,
        background: None,
        checkpoints: None,
        auto_compact: false,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join("journal.jsonl"),
    })
    .unwrap()
}

/// Sends `text` and returns the session's events up to the end of the turn.
async fn turn(
    session: &Session,
    events: &mut tokio::sync::broadcast::Receiver<imperium_protocol::session::AgentEvent>,
    text: &str,
) -> Vec<SessionEvent> {
    session.send(text.into()).unwrap();
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        let done = matches!(
            event,
            SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
        );
        seen.push(event);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn local_calls_reserve_their_estimate_and_cap_the_reply() {
    let pool = Arc::new(Pool::default());
    let script = Arc::new(Script::default());
    script.replies.lock().unwrap().extend([
        Ok(Completion {
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "await_tasks".into(),
                    arguments: "{}".into(),
                },
            }],
            finish_reason: Some("tool_calls".into()),
            usage: usage(900, 50),
            ..Completion::default()
        }),
        Ok(Completion {
            text: "done".into(),
            finish_reason: Some("stop".into()),
            usage: usage(1000, 10),
            ..Completion::default()
        }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let session = start(&pool, &script, dir.path());
    let mut events = session.subscribe();
    let seen = turn(&session, &mut events, "wait for nothing").await;
    assert!(matches!(
        seen.last(),
        Some(SessionEvent::TurnCompleted { .. })
    ));

    let requests = pool.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| !r.uncapped));
    // The first call counts every message and the tool specs, with the margin and overhead.
    let first = requests[0].prompt_tokens.expect("an estimate");
    assert!(first > 300, "{first}");
    // The second starts from the first call's exact usage (900 + 50) and adds the tool result.
    let second = requests[1].prompt_tokens.expect("an estimate");
    assert!((958..1100).contains(&second), "{second}");
    // The reply is capped at what the lease allows.
    let sent: Vec<_> = script.seen.lock().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(sent, [Some(CAP), Some(CAP)]);
}

#[tokio::test]
async fn a_local_reply_cut_by_its_reservation_is_retried_once_uncapped() {
    // A reply that stopped on its cap is asked for again with the whole context, after a
    // notice; only the retry's text is kept.
    let pool = Arc::new(Pool::default());
    let script = Arc::new(Script::default());
    script.replies.lock().unwrap().extend([
        reply("cut", "length"),
        reply("full answer", "stop"),
        reply("ok", "stop"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let session = start(&pool, &script, dir.path());
    let mut events = session.subscribe();
    let seen = turn(&session, &mut events, "write a lot").await;
    assert!(matches!(
        seen.last(),
        Some(SessionEvent::TurnCompleted { .. })
    ));
    let texts: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            SessionEvent::TextDelta { text } => Some(text.as_str()),
            SessionEvent::Notice { .. } => Some("<notice>"),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["cut", "<notice>", "full answer"]);
    let uncapped: Vec<_> = pool
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.uncapped)
        .collect();
    assert_eq!(uncapped, [false, true]);
    let caps: Vec<_> = script.seen.lock().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(caps, [Some(CAP), None]);
    // The next request carries the retry's reply in the conversation, not the cut one.
    turn(&session, &mut events, "again").await;
    let last = script.seen.lock().unwrap().last().unwrap().1.clone();
    assert!(
        last.contains("full answer") && !last.contains("\"cut\""),
        "{last}"
    );
}

#[tokio::test]
async fn a_pool_overflow_is_retried_once_uncapped_and_a_second_failure_is_not() {
    // "Context size has been exceeded." is retried the same way.
    let pool = Arc::new(Pool::default());
    let script = Arc::new(Script::default());
    script
        .replies
        .lock()
        .unwrap()
        .extend([overflow(), reply("recovered", "stop")]);
    let dir = tempfile::tempdir().unwrap();
    let session = start(&pool, &script, dir.path());
    let mut events = session.subscribe();
    let seen = turn(&session, &mut events, "go").await;
    assert!(matches!(
        seen.last(),
        Some(SessionEvent::TurnCompleted { .. })
    ));
    assert!(
        seen.iter()
            .any(|e| matches!(e, SessionEvent::Notice { .. }))
    );
    assert_eq!(pool.requests.lock().unwrap().len(), 2);

    // Failing again after the retry ends the turn: there is no third attempt.
    let pool = Arc::new(Pool::default());
    let script = Arc::new(Script::default());
    script
        .replies
        .lock()
        .unwrap()
        .extend([overflow(), overflow()]);
    let dir = tempfile::tempdir().unwrap();
    let session = start(&pool, &script, dir.path());
    let mut events = session.subscribe();
    let seen = turn(&session, &mut events, "go").await;
    assert!(matches!(seen.last(), Some(SessionEvent::TurnFailed { .. })));
    assert_eq!(pool.requests.lock().unwrap().len(), 2);

    // A reply cut off while already uncapped is accepted as it is.
    let pool = Arc::new(Pool::default());
    let script = Arc::new(Script::default());
    script
        .replies
        .lock()
        .unwrap()
        .extend([reply("cut", "length"), reply("still cut", "length")]);
    let dir = tempfile::tempdir().unwrap();
    let session = start(&pool, &script, dir.path());
    let mut events = session.subscribe();
    let seen = turn(&session, &mut events, "go").await;
    assert!(matches!(
        seen.last(),
        Some(SessionEvent::TurnCompleted { .. })
    ));
    assert_eq!(pool.requests.lock().unwrap().len(), 2);

    // An overflow of a call that was never capped is not retried: it would be sent the same.
    let pool = Arc::new(Pool {
        never_cap: true,
        ..Pool::default()
    });
    let script = Arc::new(Script::default());
    script.replies.lock().unwrap().extend([overflow()]);
    let dir = tempfile::tempdir().unwrap();
    let session = start(&pool, &script, dir.path());
    let mut events = session.subscribe();
    let seen = turn(&session, &mut events, "go").await;
    assert!(matches!(seen.last(), Some(SessionEvent::TurnFailed { .. })));
    assert_eq!(pool.requests.lock().unwrap().len(), 1);
}
