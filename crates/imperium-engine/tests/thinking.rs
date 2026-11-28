//! A thinking model's reasoning streams as `thinking_delta` events closed by
//! `thinking_finished`, and stays display-only: it never goes back into a later request.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use imperium_api::Usage;
use imperium_engine::{ChatClient, ChatClients, ResolvedModel, Session, SessionConfig};
use imperium_models::{ChatFuture, ChatRequest, Completion, Endpoint, StreamDelta};
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;

const SECRET: &str = "the zanzibar chain of thought";

/// Thinks, then answers; remembers what each request carried.
#[derive(Debug, Default)]
struct Thinker {
    requests: Mutex<Vec<String>>,
}

impl ChatClient for Thinker {
    fn stream<'a>(
        &'a self,
        _request: ChatRequest<'a>,
        _on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        unreachable!("the engine streams through stream_deltas")
    }

    fn stream_deltas<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_delta: &'a mut (dyn FnMut(StreamDelta<'_>) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push(format!("{:?}", request.messages));
            on_delta(StreamDelta::Thinking("the zanzibar "));
            on_delta(StreamDelta::Thinking("chain of thought"));
            tokio::time::sleep(Duration::from_millis(20)).await;
            on_delta(StreamDelta::Text("The answer."));
            Ok(Completion {
                text: "The answer.".into(),
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 4,
                    cached_tokens: 0,
                }),
                ..Completion::default()
            })
        })
    }
}

fn spawn(thinker: Arc<Thinker>, dir: &std::path::Path) -> Session {
    let mut clients = ChatClients::default();
    clients.register("thinker", move |_http, _endpoint| {
        Arc::clone(&thinker) as Arc<dyn ChatClient>
    });
    Session::spawn(SessionConfig {
        id: "thinking".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "custom:model-x".into(),
            endpoint: Endpoint {
                base_url: "http://unreachable.invalid/v1".into(),
                api_key: None,
            },
            model: "model-x".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "thinker".into(),
            reasoning: Some(true),
        }),
        catalog: Vec::new(),
        clients,
        routing: imperium_config::RoutingSettings::default(),
        local: None,
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
        personality: None,
        max_steps: 3,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness: Default::default(),
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
        journal_path: dir.join("journal.jsonl"),
    })
    .unwrap()
}

/// The events of one turn, up to its completion.
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
        let done = matches!(event, SessionEvent::TurnCompleted { .. });
        if let SessionEvent::TurnFailed { message } = &event {
            panic!("turn failed: {message}");
        }
        seen.push(event);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn reasoning_streams_then_finishes_before_the_reply_and_is_journaled() {
    let thinker = Arc::new(Thinker::default());
    let dir = tempfile::tempdir().unwrap();
    let session = spawn(Arc::clone(&thinker), dir.path());
    let mut events = session.subscribe();
    let seen = turn(&session, &mut events, "think about it").await;

    let kinds: Vec<&str> = seen
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ThinkingDelta { .. } => Some("thinking"),
            SessionEvent::ThinkingFinished { .. } => Some("finished"),
            SessionEvent::TextDelta { .. } => Some("text"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["thinking", "thinking", "finished", "text"]);
    let thought: String = seen
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ThinkingDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thought, SECRET);
    let duration = seen.iter().find_map(|event| match event {
        SessionEvent::ThinkingFinished { duration_ms } => Some(*duration_ms),
        _ => None,
    });
    assert!(duration.is_some_and(|ms| ms >= 15), "{duration:?}");

    // The session's recorded history (what a reopened session replays) holds them too.
    let (history, _) = session.subscribe_with_history();
    let recorded =
        |wanted: fn(&SessionEvent) -> bool| history.iter().any(|entry| wanted(&entry.event));
    assert!(recorded(|event| matches!(
        event,
        SessionEvent::ThinkingDelta { .. }
    )));
    assert!(recorded(|event| matches!(
        event,
        SessionEvent::ThinkingFinished { .. }
    )));
}

#[tokio::test]
async fn reasoning_is_never_sent_back_to_the_model() {
    let thinker = Arc::new(Thinker::default());
    let dir = tempfile::tempdir().unwrap();
    let session = spawn(Arc::clone(&thinker), dir.path());
    let mut events = session.subscribe();
    turn(&session, &mut events, "first").await;
    turn(&session, &mut events, "second").await;

    let requests = thinker.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    // The second request carries the first reply, not the reasoning behind it.
    assert!(requests[1].contains("The answer."));
    assert!(requests.iter().all(|sent| !sent.contains("zanzibar")));
}
