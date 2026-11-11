//! Stopping a session: the root and every subagent halt at their next safe point (a model
//! request in flight is cancelled and sent again on resume), nothing progresses while stopped,
//! state is kept, queued messages wait, the stop survives a restart, and resuming completes
//! the work.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, AgentStatus, SendMode, SessionEvent, SessionStatus};
use imperium_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// A fake OpenAI-compatible endpoint that records every request. Root agent: `delegate`
/// spawns `sub work` and awaits it; `slow` answers after a pause. Subagent: `sub work` pauses,
/// then reads a.txt, then reports `sub done`. Anything else is answered `done`.
#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<Value>>,
}

impl Fake {
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn stream(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn text(content: &str) -> String {
    stream(json!({"choices": [{"delta": {"content": content}}]}))
}

fn call(id: &str, name: &str, arguments: Value) -> String {
    stream(
        json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": id,
        "function": {"name": name, "arguments": arguments.to_string()}}]}}]}),
    )
}

fn is_subagent(body: &Value) -> bool {
    body["messages"][0]["content"]
        .as_str()
        .is_some_and(|system| system.starts_with("You are an Imperium subagent"))
}

fn last_message(body: &Value) -> (String, String) {
    let last = body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .cloned()
        .unwrap_or_default();
    (
        last["role"].as_str().unwrap_or_default().into(),
        last["content"].as_str().unwrap_or_default().into(),
    )
}

async fn answer(body: &Value) -> String {
    let (role, said) = last_message(body);
    let pause = || tokio::time::sleep(Duration::from_millis(800));
    if is_subagent(body) {
        return match (role.as_str(), said.as_str()) {
            ("user", "sub work") => {
                pause().await;
                call("sub-read", "read_file", json!({"path": "a.txt"}))
            }
            _ => text("sub done"),
        };
    }
    match (role.as_str(), said.as_str()) {
        ("user", "delegate") => call("spawn", "spawn_agent", json!({"brief": "sub work"})),
        ("tool", said) if said.starts_with("started t1") => call("await", "await_tasks", json!({})),
        ("tool", said) if said.contains("## t1") => text("all done"),
        ("user", "slow") => {
            pause().await;
            text("done slowly")
        }
        _ => text("done"),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let fake = Arc::clone(&fake);
            async move {
                fake.requests.lock().unwrap().push(body.clone());
                ([("content-type", "text/event-stream")], answer(&body).await)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn start(url: &str, dir: &std::path::Path, resume: bool) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:m".into(),
            endpoint: Endpoint {
                base_url: url.into(),
                api_key: None,
            },
            model: "m".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "openai".into(),
            reasoning: None,
        }),
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        clients: Default::default(),
        local: None,
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        max_steps: 8,
        auto_compact: false,
        background: None,
        checkpoints: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".sessions/s.jsonl"),
    })
    .unwrap()
}

/// Waits for an event from `agent` matching `wanted` and returns it with the events before it.
async fn until(
    events: &mut broadcast::Receiver<AgentEvent>,
    agent: &str,
    wanted: impl Fn(&SessionEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
            .await
            .expect("an event within 15 s")
            .unwrap();
        let done = event.agent_id == agent && wanted(&event.event);
        seen.push(event);
        if done {
            return seen;
        }
    }
}

/// Collects whatever arrives within `wait`.
async fn drain(events: &mut broadcast::Receiver<AgentEvent>, wait: Duration) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
        seen.push(event);
    }
    seen
}

fn root_turn_ended(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
    )
}

/// Requests whose last message is the subagent's brief.
fn brief_requests(fake: &Fake) -> usize {
    fake.requests()
        .iter()
        .filter(|body| is_subagent(body) && last_message(body).1 == "sub work")
        .count()
}

#[tokio::test]
async fn a_stopped_session_halts_its_subagents_and_resuming_completes_the_work() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
    let session = start(&url, dir.path(), false);
    let mut events = session.subscribe();

    session.send("delegate".into()).unwrap();
    until(&mut events, "t1", |event| {
        matches!(event, SessionEvent::TurnStarted { .. })
    })
    .await;
    // Stopped while the subagent's first model call is answering: it is cancelled.
    tokio::time::sleep(Duration::from_millis(200)).await;
    session.stop();
    assert!(session.is_stopped());
    assert_eq!(session.status().0, SessionStatus::Paused);
    until(&mut events, "root", |event| {
        matches!(event, SessionEvent::WorkStopped {})
    })
    .await;
    // Let a tool call in flight finish and every agent reach its safe point.
    tokio::time::sleep(Duration::from_millis(200)).await;
    drain(&mut events, Duration::ZERO).await;
    let requests = fake.requests().len();

    // Nothing progresses while stopped: no new requests, no reports, no turn ends.
    let quiet = drain(&mut events, Duration::from_millis(1_500)).await;
    assert_eq!(fake.requests().len(), requests);
    assert!(!quiet.iter().any(|event| matches!(
        event.event,
        SessionEvent::TaskFinished { .. }
            | SessionEvent::TurnCompleted { .. }
            | SessionEvent::ToolStarted { .. }
    )));
    // The subagent is still working, its state kept to go on from.
    let t1 = session
        .agents()
        .into_iter()
        .find(|agent| agent.agent_id == "t1")
        .unwrap();
    assert_eq!(t1.status, AgentStatus::Running);
    let saved = std::fs::read_to_string(dir.path().join(".sessions/s.agents/t1.json")).unwrap();
    assert!(saved.contains("sub work"));
    // Stopping again changes nothing.
    session.stop();

    // Resumed: the cancelled request is sent again and the work completes.
    session.resume_work();
    assert!(!session.is_stopped());
    let turn = until(&mut events, "root", root_turn_ended).await;
    assert!(
        matches!(
            turn.last().unwrap().event,
            SessionEvent::TurnCompleted { .. }
        ),
        "{:?}",
        turn.last()
    );
    assert!(turn.iter().any(|event| matches!(
        &event.event,
        SessionEvent::TaskFinished { ok: true, report, .. } if report == "sub done"
    )));
    assert_eq!(brief_requests(&fake), 2, "the cancelled call is sent again");
    assert_eq!(session.status().0, SessionStatus::Idle);
}

#[tokio::test]
async fn a_stop_survives_a_restart_and_keeps_the_queue_waiting() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(&url, dir.path(), false);
    let mut events = session.subscribe();

    session.send("slow".into()).unwrap();
    until(&mut events, "root", |event| {
        matches!(event, SessionEvent::TurnStarted { .. })
    })
    .await;
    session.stop();
    let later = session
        .send_message("later".into(), SendMode::Queue, None)
        .unwrap();
    until(
        &mut events,
        "root",
        |event| matches!(event, SessionEvent::MessageQueued { id, .. } if *id == later),
    )
    .await;
    // The daemon stops while the session is stopped.
    drop(session);
    drop(events);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let requests = fake.requests().len();

    let resumed = start(&url, dir.path(), true);
    assert!(resumed.is_stopped());
    assert_eq!(resumed.status().0, SessionStatus::Paused);
    let mut events = resumed.subscribe();
    // A message sent to the stopped, idle session waits too.
    resumed.send("another".into()).unwrap();
    drain(&mut events, Duration::from_millis(800)).await;
    assert_eq!(fake.requests().len(), requests, "nothing ran while stopped");
    let queue: Vec<String> = resumed
        .queue(None)
        .unwrap()
        .into_iter()
        .map(|message| message.text)
        .collect();
    assert_eq!(queue, ["later", "another"]);

    // Resumed: the queue runs, in order.
    resumed.resume_work();
    let mut started = Vec::new();
    while started.len() < 2 {
        for event in until(&mut events, "root", root_turn_ended).await {
            if let SessionEvent::TurnStarted { text, .. } = event.event {
                started.push(text);
            }
        }
    }
    assert_eq!(started, ["later", "another"]);
    assert!(resumed.queue(None).unwrap().is_empty());
}
