//! Stage 2g: a background session (started by a schedule or a project agent) gives way to the
//! user. When the machine turns busy it finishes its model call, pauses at the next step
//! boundary holding no slot and spending no tokens, and carries on when the machine is quiet.
//! A person can also run it now, which ends the wait and stops it giving way again.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{
    AcquireFuture, Availability, Background, Load, LoadView, LocalLease, LocalModels,
    LocalPresence, Priority, ResolvedModel, Session, SessionConfig,
};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, PauseCause, SessionEvent, SessionStatus};
use imperium_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, broadcast};

/// A load view the test sets.
#[derive(Debug)]
struct Manual(Mutex<Load>);

impl LoadView for Manual {
    fn level(&self) -> Load {
        *self.0.lock().unwrap()
    }
}

/// A loaded model with one slot that records who asked and at what priority.
#[derive(Debug)]
struct OneSlot {
    base_url: String,
    permits: Arc<Semaphore>,
    held: Arc<AtomicUsize>,
    priorities: Mutex<Vec<Priority>>,
    /// Agents attached to the model for a turn right now.
    attached: Arc<AtomicUsize>,
}

struct Attached(Arc<AtomicUsize>);

impl Drop for Attached {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Held {
    held: Arc<AtomicUsize>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.held.fetch_sub(1, Ordering::SeqCst);
    }
}

impl OneSlot {
    async fn take(&self) -> Result<LocalLease, String> {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|error| error.to_string())?;
        self.held.fetch_add(1, Ordering::SeqCst);
        Ok(LocalLease::new(
            self.base_url.clone(),
            Some(0),
            None,
            Held {
                held: Arc::clone(&self.held),
                _permit: permit,
            },
        ))
    }
}

impl LocalModels for OneSlot {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Loaded
    }

    fn acquire<'a>(&'a self, _model: &'a str, _holder: &'a str) -> AcquireFuture<'a> {
        Box::pin(self.take())
    }

    fn attach(&self, _model: &str, _holder: &str) -> LocalPresence {
        self.attached.fetch_add(1, Ordering::SeqCst);
        LocalPresence::new(Attached(Arc::clone(&self.attached)))
    }

    fn acquire_as<'a>(
        &'a self,
        _model: &'a str,
        _holder: &'a str,
        priority: Priority,
    ) -> AcquireFuture<'a> {
        self.priorities.lock().unwrap().push(priority);
        Box::pin(self.take())
    }
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Two calls: a plan update (a tool call, so a second call follows), then the answer. Counts
/// requests and, on the first, turns the machine busy.
async fn model_server(requests: Arc<AtomicUsize>, load: Option<Arc<Manual>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let requests = Arc::clone(&requests);
            let load = load.clone();
            async move {
                let nth = requests.fetch_add(1, Ordering::SeqCst);
                if nth == 0
                    && let Some(load) = &load
                {
                    *load.0.lock().unwrap() = Load::Busy;
                }
                // Long enough that the machine is already busy when the call completes.
                tokio::time::sleep(Duration::from_millis(30)).await;
                let reply = if nth == 0 {
                    let arguments = json!({"steps": [{"title": "look", "status": "in_progress"}]});
                    sse(
                        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
                        "function":{"name":"update_plan","arguments":arguments.to_string()}}]}}]}),
                    )
                } else {
                    sse(json!({"choices":[{"delta":{"content":"all done"}}]}))
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

fn start(local: Arc<OneSlot>, load: Arc<Manual>, dir: &std::path::Path) -> Session {
    Session::spawn(SessionConfig {
        id: "background-1".into(),
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
        background: Some(Background { load }),
        journal_path: dir.join(".journal.jsonl"),
    })
    .unwrap()
}

/// Reads events until one satisfies `want`, within `within`.
async fn until(
    events: &mut broadcast::Receiver<AgentEvent>,
    within: Duration,
    mut want: impl FnMut(&SessionEvent) -> bool,
) -> SessionEvent {
    tokio::time::timeout(within, async {
        loop {
            let event = events.recv().await.unwrap();
            if event.agent_id == "root" && want(&event.event) {
                return event.event;
            }
        }
    })
    .await
    .expect("the event arrives in time")
}

fn paused_for_load(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Paused {
            cause: PauseCause::Load,
            ..
        }
    )
}

#[tokio::test]
async fn background_pause_at_a_step_boundary_when_busy_and_resume_when_quiet() {
    let load = Arc::new(Manual(Mutex::new(Load::Quiet)));
    let requests = Arc::new(AtomicUsize::new(0));
    let url = model_server(Arc::clone(&requests), Some(Arc::clone(&load))).await;
    let held = Arc::new(AtomicUsize::new(0));
    let local = Arc::new(OneSlot {
        base_url: url,
        permits: Arc::new(Semaphore::new(1)),
        held: Arc::clone(&held),
        priorities: Mutex::new(Vec::new()),
        attached: Arc::new(AtomicUsize::new(0)),
    });
    let dir = tempfile::tempdir().unwrap();
    let session = start(local.clone(), load.clone(), dir.path());
    let mut events = session.subscribe();
    session.send("work on it".into()).unwrap();

    // The first call turns the machine busy while it runs; it finishes, and the next step
    // boundary is where the session waits.
    let paused = until(&mut events, Duration::from_secs(10), paused_for_load).await;
    let SessionEvent::Paused {
        until_unix_ms,
        reason,
        ..
    } = paused
    else {
        unreachable!()
    };
    assert_eq!(until_unix_ms, 0, "it waits for the machine, not for a time");
    assert!(reason.contains("busy"), "{reason}");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "the call in flight finished"
    );
    assert_eq!(
        held.load(Ordering::SeqCst),
        0,
        "a paused session holds no slot"
    );
    // Nor does it keep its model claimed: a paused agent is detached from it, so a model
    // another session asks for can swap it out without waiting for this one.
    assert_eq!(local.attached.load(Ordering::SeqCst), 0);
    // Nothing else holds it up: another caller takes the one slot at once.
    drop(
        tokio::time::timeout(Duration::from_millis(500), local.acquire("q", "other"))
            .await
            .expect("the slot is free")
            .unwrap(),
    );
    let hold = session.hold().expect("the pause is shown as a hold");
    assert_eq!(hold.cause, PauseCause::Load);
    assert_eq!(session.status().0, SessionStatus::Paused);
    assert!(session.is_background());

    // It stays paused, using no tokens, for as long as the machine is busy or saturated.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    *load.0.lock().unwrap() = Load::Saturated;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "no model calls while paused"
    );
    assert_eq!(session.status().0, SessionStatus::Paused);

    // Quiet again: it carries on and finishes.
    *load.0.lock().unwrap() = Load::Quiet;
    until(&mut events, Duration::from_secs(10), |event| {
        matches!(event, SessionEvent::Resumed {})
    })
    .await;
    // Attached to its model again for the rest of the turn.
    assert_eq!(local.attached.load(Ordering::SeqCst), 1);
    until(&mut events, Duration::from_secs(10), |event| {
        matches!(event, SessionEvent::TurnCompleted { .. })
    })
    .await;
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert!(session.hold().is_none());
    assert_eq!(held.load(Ordering::SeqCst), 0);
    // Every model call was a background one.
    let priorities = local.priorities.lock().unwrap().clone();
    assert_eq!(priorities, [Priority::Background; 2]);
}

#[tokio::test]
async fn background_pause_ends_when_a_person_runs_it_now() {
    let load = Arc::new(Manual(Mutex::new(Load::Saturated)));
    let requests = Arc::new(AtomicUsize::new(0));
    let url = model_server(Arc::clone(&requests), None).await;
    let local = Arc::new(OneSlot {
        base_url: url,
        permits: Arc::new(Semaphore::new(1)),
        held: Arc::new(AtomicUsize::new(0)),
        priorities: Mutex::new(Vec::new()),
        attached: Arc::new(AtomicUsize::new(0)),
    });
    let dir = tempfile::tempdir().unwrap();
    let session = start(local.clone(), load, dir.path());
    let mut events = session.subscribe();
    session.send("work on it".into()).unwrap();

    // The machine is already saturated, so it waits before its first call.
    until(&mut events, Duration::from_secs(10), paused_for_load).await;
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    session.resume_now();
    until(&mut events, Duration::from_secs(10), |event| {
        matches!(event, SessionEvent::TurnCompleted { .. })
    })
    .await;
    // Both calls ran though the machine stayed saturated: run now stops it giving way.
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert!(session.hold().is_none());
    // Its local calls rank as the user's, so a saturated machine does not hold them back
    // in the runtime either.
    let priorities = local.priorities.lock().unwrap().clone();
    assert_eq!(priorities, [Priority::Interactive; 2]);
}
