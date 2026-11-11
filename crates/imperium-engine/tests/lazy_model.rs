//! Models load lazily: selecting a local model for a session (or creating a session on one)
//! starts no server and takes no lease; the first message loads it. A model that cannot load
//! fails that message's turn with the reason, and the session can switch to another model. The
//! local runtime and the model are fakes.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use imperium_api::Usage;
use imperium_engine::{
    AcquireFuture, Availability, ChatClient, ChatClients, LocalLease, LocalModels, LocalRequest,
    ResolvedModel, Session, SessionConfig,
};
use imperium_models::{ChatFuture, ChatRequest, Completion, Endpoint};
use imperium_protocol::session::{AgentEvent, SessionEvent, TokenCountMethod};
use imperium_tools::Workspace;
use tokio::sync::broadcast::Receiver;

/// The context the fake runtime would start its model with.
const PLANNED_CONTEXT: u64 = 8_192;

/// A local runtime with one model, `m`, that counts the leases taken on it (each one would
/// start its server if it were not running) and can be made to fail every load.
#[derive(Debug, Default)]
struct Runtime {
    leases: AtomicUsize,
    broken: AtomicBool,
}

impl LocalModels for Runtime {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Fits
    }

    fn has_model(&self, model: &str) -> bool {
        model == "m"
    }

    fn planned_context(&self, model: &str) -> Option<u64> {
        (model == "m").then_some(PLANNED_CONTEXT)
    }

    fn acquire<'a>(&'a self, model: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        self.acquire_with(model, holder, LocalRequest::default())
    }

    fn acquire_with<'a>(
        &'a self,
        _model: &'a str,
        _holder: &'a str,
        _request: LocalRequest,
    ) -> AcquireFuture<'a> {
        Box::pin(async move {
            if self.broken.load(Ordering::SeqCst) {
                return Err("llama-server was not found".to_string());
            }
            self.leases.fetch_add(1, Ordering::SeqCst);
            Ok(LocalLease::new("http://fake/v1".into(), Some(0), None, ()))
        })
    }
}

/// Answers every call with "done", recording the endpoint it was built for.
#[derive(Debug, Default)]
struct Script {
    calls: Mutex<Vec<String>>,
}

impl ChatClient for Script {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(request.model.to_string());
            on_text("done");
            Ok(Completion {
                text: "done".into(),
                usage: Some(Usage {
                    prompt_tokens: 100,
                    completion_tokens: 1,
                    cached_tokens: 0,
                }),
                finish_reason: Some("stop".into()),
                ..Completion::default()
            })
        })
    }
}

fn hosted() -> ResolvedModel {
    ResolvedModel {
        spec: "cloud:hosted".into(),
        endpoint: Endpoint {
            base_url: "http://hosted/v1".into(),
            api_key: None,
        },
        model: "hosted".into(),
        pricing: None,
        local: false,
        context_window: None,
        kind: "scripted".into(),
        reasoning: None,
    }
}

fn local() -> ResolvedModel {
    ResolvedModel {
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
    }
}

fn start(
    model: ResolvedModel,
    runtime: &Arc<Runtime>,
    script: &Arc<Script>,
    dir: &std::path::Path,
) -> Session {
    let mut clients = ChatClients::default();
    let shared = Arc::clone(script);
    clients.register("scripted", move |_http, _endpoint| {
        Arc::clone(&shared) as Arc<dyn ChatClient>
    });
    let local: Arc<dyn LocalModels> = runtime.clone();
    Session::spawn(SessionConfig {
        id: "lazy".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(model),
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
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join("journal.jsonl"),
    })
    .unwrap()
}

/// Sends `text` and returns the session's events up to the end of the turn.
async fn turn(session: &Session, events: &mut Receiver<AgentEvent>, text: &str) -> SessionEvent {
    session.send(text.into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if matches!(
            event,
            SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
        ) {
            return event;
        }
    }
}

#[tokio::test]
async fn selecting_a_local_model_loads_nothing_until_a_message_is_sent() {
    let (runtime, script) = (Arc::new(Runtime::default()), Arc::new(Script::default()));
    let dir = tempfile::tempdir().unwrap();
    let session = start(hosted(), &runtime, &script, dir.path());
    let mut events = session.subscribe();
    assert!(matches!(
        turn(&session, &mut events, "hello").await,
        SessionEvent::TurnCompleted { .. }
    ));

    let fit = session.set_model(local()).await.unwrap();
    // An estimate against the context the runtime would start the model with.
    assert_eq!(fit.method, TokenCountMethod::Estimated);
    assert_eq!(fit.context_window, Some(PLANNED_CONTEXT));
    assert!(fit.compaction.is_none());
    assert_eq!(session.model().as_deref(), Some("llamacpp:m"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        runtime.leases.load(Ordering::SeqCst),
        0,
        "selecting loads nothing"
    );

    // The first message loads it, and the turn runs on it.
    assert!(matches!(
        turn(&session, &mut events, "now locally").await,
        SessionEvent::TurnCompleted { .. }
    ));
    assert!(runtime.leases.load(Ordering::SeqCst) > 0);
    assert_eq!(
        script.calls.lock().unwrap().last().map(String::as_str),
        Some("m")
    );
}

#[tokio::test]
async fn a_session_created_on_a_local_model_loads_it_with_the_first_message() {
    let (runtime, script) = (Arc::new(Runtime::default()), Arc::new(Script::default()));
    let dir = tempfile::tempdir().unwrap();
    let session = start(local(), &runtime, &script, dir.path());
    let mut events = session.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runtime.leases.load(Ordering::SeqCst), 0);
    assert!(matches!(
        turn(&session, &mut events, "hello").await,
        SessionEvent::TurnCompleted { .. }
    ));
    assert!(runtime.leases.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn a_load_failure_fails_the_first_message_and_another_model_can_be_picked() {
    let (runtime, script) = (Arc::new(Runtime::default()), Arc::new(Script::default()));
    runtime.broken.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let session = start(hosted(), &runtime, &script, dir.path());
    let mut events = session.subscribe();

    // Selecting it succeeds: nothing is loaded yet.
    session.set_model(local()).await.unwrap();

    // Sending fails the turn with the reason, and no model is called.
    let SessionEvent::TurnFailed { message } = turn(&session, &mut events, "hello").await else {
        panic!("the turn fails");
    };
    assert!(
        message.contains("could not load local model m") && message.contains("llama-server"),
        "{message}"
    );
    assert!(script.calls.lock().unwrap().is_empty());

    // The session stays usable: on another model, the next message runs.
    session.set_model(hosted()).await.unwrap();
    assert!(matches!(
        turn(&session, &mut events, "hello again").await,
        SessionEvent::TurnCompleted { .. }
    ));
    assert_eq!(*script.calls.lock().unwrap(), ["hosted"]);
}
