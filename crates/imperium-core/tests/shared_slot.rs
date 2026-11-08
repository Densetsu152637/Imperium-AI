//! Stage 2c: one loaded local model with a single slot serves many sessions. An agent holds the
//! slot only for the length of a model call, so roots asleep in `await_tasks` leave it to their
//! subagents and to each other. With a lease per turn this test would deadlock: every root would
//! hold the only slot while waiting for a subagent that cannot get one.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use imperium_core::{
    AcquireFuture, Availability, LocalLease, LocalModels, ResolvedModel, Session, SessionConfig,
};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, broadcast};

const SESSIONS: usize = 3;

/// A loaded model with one slot, handed out first-come, first-served like the supervisor's.
#[derive(Debug)]
struct OneSlot {
    base_url: String,
    permits: Arc<Semaphore>,
    /// Leases alive right now.
    held: Arc<AtomicUsize>,
    /// The holder of each acquisition, in order.
    holders: Mutex<Vec<String>>,
}

/// Counts a lease while it lives; the permit frees the slot after it.
struct Held {
    held: Arc<AtomicUsize>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.held.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LocalModels for OneSlot {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Loaded
    }

    fn acquire<'a>(&'a self, _model: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        Box::pin(async move {
            let permit = Arc::clone(&self.permits)
                .acquire_owned()
                .await
                .map_err(|error| error.to_string())?;
            self.held.fetch_add(1, Ordering::SeqCst);
            self.holders.lock().unwrap().push(holder.to_string());
            Ok(LocalLease::new(
                self.base_url.clone(),
                Some(0),
                Held {
                    held: Arc::clone(&self.held),
                    _permit: permit,
                },
            ))
        })
    }
}

/// What the fake llama.cpp server saw.
#[derive(Default)]
struct Seen {
    requests: AtomicUsize,
    /// Requests that carried `id_slot: 0` and `cache_prompt: true`.
    pinned: AtomicUsize,
    /// The most leases alive when a request arrived.
    most_held: AtomicUsize,
    /// Roots that called `await_tasks`.
    awaits: AtomicUsize,
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn call(name: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
    )
}

/// A server that plays root (delegate, wait, finish) or subagent (report) by its system prompt.
async fn model_server(seen: Arc<Seen>, held: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let seen = Arc::clone(&seen);
            let held = Arc::clone(&held);
            async move {
                seen.requests.fetch_add(1, Ordering::SeqCst);
                if body["id_slot"] == 0 && body["cache_prompt"] == true {
                    seen.pinned.fetch_add(1, Ordering::SeqCst);
                }
                seen.most_held
                    .fetch_max(held.load(Ordering::SeqCst), Ordering::SeqCst);
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let subagent = messages[0]["content"]
                    .as_str()
                    .is_some_and(|system| system.contains("subagent"));
                let tool_results = messages.iter().filter(|m| m["role"] == "tool").count();
                // Take a moment, so the other sessions' calls queue behind this one.
                tokio::time::sleep(Duration::from_millis(40)).await;
                let reply = if subagent {
                    sse(json!({"choices":[{"delta":{"content":"report"}}]}))
                } else {
                    match tool_results {
                        0 => call("spawn_agent", json!({"brief": "look around"})),
                        1 => {
                            seen.awaits.fetch_add(1, Ordering::SeqCst);
                            call("await_tasks", json!({}))
                        }
                        _ => sse(json!({"choices":[{"delta":{"content":"all done"}}]})),
                    }
                };
                ([("content-type", "text/event-stream")], reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn start(index: usize, local: Arc<dyn LocalModels>, dir: &std::path::Path) -> Session {
    Session::spawn(SessionConfig {
        id: format!("session-{index}"),
        workspace: Workspace::new(dir).unwrap(),
        model: ResolvedModel {
            spec: "llamacpp:q".into(),
            endpoint: Endpoint {
                base_url: String::new(),
                api_key: None,
            },
            model: "q".into(),
            pricing: None,
            local: true,
            context_window: None,
            kind: "llamacpp".into(),
        },
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        clients: Default::default(),
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
        resume_policy: imperium_core::default_resume_policy(),
        instructions: None,
        max_steps: 6,
        background: None,
        journal_path: dir.join(".journal.jsonl"),
    })
    .unwrap()
}

/// Waits for the root's turn to end; returns whether it completed and the queue waits it
/// reported for its model calls.
async fn finish(
    history: Vec<AgentEvent>,
    mut events: broadcast::Receiver<AgentEvent>,
) -> (bool, Vec<Option<u64>>) {
    let mut waits = Vec::new();
    let mut pending = history;
    loop {
        for event in pending.drain(..) {
            // Subagents' events carry their own agent ids; only the root's count here.
            if event.agent_id != "root" {
                continue;
            }
            match event.event {
                SessionEvent::ModelCallCompleted { queue_wait_ms, .. } => waits.push(queue_wait_ms),
                SessionEvent::TurnCompleted { .. } => return (true, waits),
                SessionEvent::TurnFailed { .. } => return (false, waits),
                _ => {}
            }
        }
        pending.push(events.recv().await.unwrap());
    }
}

#[tokio::test]
async fn shared_slot() {
    let seen = Arc::new(Seen::default());
    let held = Arc::new(AtomicUsize::new(0));
    let base_url = model_server(Arc::clone(&seen), Arc::clone(&held)).await;
    let local = Arc::new(OneSlot {
        base_url,
        permits: Arc::new(Semaphore::new(1)),
        held: Arc::clone(&held),
        holders: Mutex::new(Vec::new()),
    });
    let dirs: Vec<_> = (0..SESSIONS)
        .map(|_| tempfile::tempdir().unwrap())
        .collect();
    let sessions: Vec<_> = dirs
        .iter()
        .enumerate()
        .map(|(index, dir)| start(index, local.clone(), dir.path()))
        .collect();
    // Subscribe before sending, so no event is missed.
    let waiting: Vec<_> = sessions
        .iter()
        .map(|session| {
            let (history, events) = session.subscribe_with_history();
            tokio::spawn(finish(history, events))
        })
        .collect();
    for session in &sessions {
        session.send("work on it".into()).unwrap();
    }
    let mut results = Vec::new();
    for turn in waiting {
        let result = tokio::time::timeout(Duration::from_secs(30), turn)
            .await
            .expect("every root finishes: a root waiting on its subagent must not hold the slot");
        results.push(result.unwrap());
    }

    for (completed, waits) in &results {
        assert!(completed, "a turn failed");
        // spawn_agent, await_tasks, and the final answer: three calls, each with a wait reported.
        assert_eq!(waits.len(), 3);
        assert!(waits.iter().all(Option::is_some));
    }
    // Every root slept in `await_tasks` while the one slot served someone else.
    assert_eq!(seen.awaits.load(Ordering::SeqCst), SESSIONS);
    // Three roots with three calls each, and a one-call subagent for each.
    assert_eq!(seen.requests.load(Ordering::SeqCst), SESSIONS * 4);
    assert_eq!(
        seen.most_held.load(Ordering::SeqCst),
        1,
        "one slot, one request at a time"
    );
    // Each request was pinned to the lease's slot and asked to keep its prompt cache.
    assert_eq!(seen.pinned.load(Ordering::SeqCst), SESSIONS * 4);
    // Calls name their agent, per session, so affinity does not mix sessions up.
    let holders = local.holders.lock().unwrap().clone();
    for index in 0..SESSIONS {
        let root = format!("session-{index}/root");
        assert_eq!(holders.iter().filter(|holder| **holder == root).count(), 3);
    }
    assert_eq!(held.load(Ordering::SeqCst), 0, "no lease outlives its call");
}

fn one_slot(base_url: String) -> (Arc<OneSlot>, Arc<AtomicUsize>) {
    let held = Arc::new(AtomicUsize::new(0));
    let local = Arc::new(OneSlot {
        base_url,
        permits: Arc::new(Semaphore::new(1)),
        held: Arc::clone(&held),
        holders: Mutex::new(Vec::new()),
    });
    (local, held)
}

/// The text of the latest user message in a request body.
fn user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn text_reply() -> axum::response::Response {
    let chunk = json!({"choices":[{"delta":{"content":"done"}}]});
    (
        [("content-type", "text/event-stream")],
        format!(
            "data: {chunk}

data: [DONE]

"
        ),
    )
        .into_response()
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

async fn next_event(events: &mut broadcast::Receiver<AgentEvent>) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("an event within 10 s")
        .unwrap()
        .event
}

/// Waits until the root's turn ends; true when it completed.
async fn turn_ends(events: &mut broadcast::Receiver<AgentEvent>) -> bool {
    loop {
        match next_event(events).await {
            SessionEvent::TurnCompleted { .. } => return true,
            SessionEvent::TurnFailed { .. } => return false,
            _ => {}
        }
    }
}

#[tokio::test]
async fn shared_slot_a_usage_limit_pause_holds_no_slot() {
    // The "limited" session is refused once, with a one-hour wait; everything else answers.
    let refused = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let refused = Arc::clone(&refused);
            move |axum::Json(body): axum::Json<Value>| {
                let refused = Arc::clone(&refused);
                async move {
                    if user_text(&body).contains("limited")
                        && refused.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        return (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "3600")],
                            "usage limit reached",
                        )
                            .into_response();
                    }
                    text_reply()
                }
            }
        }),
    );
    let (local, held) = one_slot(serve(app).await);
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let limited = start(0, local.clone(), dirs[0].path());
    let other = start(1, local.clone(), dirs[1].path());
    let mut limited_events = limited.subscribe();
    let mut other_events = other.subscribe();

    limited.send("limited".into()).unwrap();
    loop {
        if matches!(
            next_event(&mut limited_events).await,
            SessionEvent::Paused { .. }
        ) {
            break;
        }
    }
    // Paused for an hour, the root holds nothing: the one slot is free...
    assert_eq!(held.load(Ordering::SeqCst), 0);
    // ...so another session runs to completion meanwhile.
    other.send("work".into()).unwrap();
    assert!(turn_ends(&mut other_events).await);
    // Then the paused one resumes and finishes.
    limited.resume_now();
    assert!(turn_ends(&mut limited_events).await);
    assert_eq!(held.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn shared_slot_a_failed_stream_releases_the_slot() {
    // The "broken" session's stream is malformed; everything else answers.
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            if user_text(&body).contains("broken") {
                return (
                    [("content-type", "text/event-stream")],
                    "data: not json

"
                    .to_string(),
                )
                    .into_response();
            }
            text_reply()
        }),
    );
    let (local, held) = one_slot(serve(app).await);
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let broken = start(0, local.clone(), dirs[0].path());
    let other = start(1, local.clone(), dirs[1].path());
    let mut broken_events = broken.subscribe();
    let mut other_events = other.subscribe();

    broken.send("broken".into()).unwrap();
    assert!(
        !turn_ends(&mut broken_events).await,
        "the stream error fails the turn"
    );
    assert_eq!(
        held.load(Ordering::SeqCst),
        0,
        "the failed call released its slot"
    );
    other.send("work".into()).unwrap();
    assert!(turn_ends(&mut other_events).await);
    assert_eq!(held.load(Ordering::SeqCst), 0);
}
