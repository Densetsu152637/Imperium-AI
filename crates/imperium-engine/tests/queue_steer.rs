//! Queueing and steering: messages sent while a turn runs wait and run in order afterwards (and
//! can be edited or removed first); a steering message joins the running turn before its next
//! model call, after the tool round, without breaking tool-call pairs; the root agent steers
//! and queues messages for its subagents with tools; and a resumed session restores its queue.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{QueueError, ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, QueuedMessage, SendMode, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// A fake OpenAI-compatible endpoint that records every request and answers by the last
/// message. Root agent: `slow` sleeps then answers; `slow tool` sleeps then reads a.txt;
/// `delegate` spawns a subagent, then steers it, queues a follow-up for it, and awaits it.
/// Subagent: its brief sleeps then reads a.txt; a follow-up answers `follow-up done`. A
/// steering message is answered `steered`; anything else `done`.
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

async fn answer(body: &Value) -> String {
    let last = body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .cloned()
        .unwrap_or_default();
    let said = last["content"].as_str().unwrap_or_default();
    let pause = || tokio::time::sleep(Duration::from_millis(700));
    if last["role"] == "user" && said.contains("<steering>") {
        return text("steered");
    }
    if is_subagent(body) {
        return match (last["role"].as_str(), said) {
            (Some("user"), "sub work") => {
                pause().await;
                call("sub-read", "read_file", json!({"path": "a.txt"}))
            }
            (Some("user"), "also check c") => text("follow-up done"),
            _ => text("sub done"),
        };
    }
    match (last["role"].as_str(), said) {
        (Some("user"), "slow") => {
            pause().await;
            text("done slowly")
        }
        (Some("user"), "slow tool") => {
            pause().await;
            call("call-1", "read_file", json!({"path": "a.txt"}))
        }
        (Some("user"), "delegate") => call("spawn", "spawn_agent", json!({"brief": "sub work"})),
        (Some("tool"), said) if said.starts_with("started t1") => call(
            "steer",
            "steer_agent",
            json!({"agent_id": "t1", "text": "focus on b"}),
        ),
        (Some("tool"), said) if said.contains("reaches t1") => call(
            "queue",
            "queue_agent",
            json!({"agent_id": "t1", "text": "also check c"}),
        ),
        (Some("tool"), said) if said.contains("is queued for t1") => {
            call("await", "await_tasks", json!({}))
        }
        (Some("tool"), said) if said.contains("## t1") => text("all done"),
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

fn model(url: &str) -> ResolvedModel {
    ResolvedModel {
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
    }
}

fn start(url: &str, dir: &std::path::Path, resume: bool) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(model(url)),
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

fn root_turn_ended(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
    )
}

fn started_texts(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event.agent_id == "root")
        .filter_map(|event| match &event.event {
            SessionEvent::TurnStarted { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The last user message of each request the root agent made.
fn root_prompts(fake: &Fake) -> Vec<String> {
    fake.requests()
        .iter()
        .filter(|body| !is_subagent(body))
        .filter_map(|body| {
            let last = body["messages"].as_array()?.last()?.clone();
            (last["role"] == "user").then(|| last["content"].as_str().unwrap_or_default().into())
        })
        .collect()
}

#[tokio::test]
async fn messages_sent_during_a_turn_wait_and_run_in_order() {
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
    let first = session
        .send_message("first".into(), SendMode::Queue, None)
        .unwrap();
    let second = session
        .send_message("second".into(), SendMode::Queue, None)
        .unwrap();
    let third = session
        .send_message("third".into(), SendMode::Queue, None)
        .unwrap();
    let queued = until(
        &mut events,
        "root",
        |event| matches!(event, SessionEvent::MessageQueued { id, .. } if *id == third),
    )
    .await;
    assert!(queued.iter().any(|event| event.event
        == SessionEvent::MessageQueued {
            id: first.clone(),
            text: "first".into(),
            steer: false,
        }));

    // Before delivery, a queued message can be edited or removed.
    session
        .edit_queued(None, &second, "second, edited".into())
        .unwrap();
    session.remove_queued(None, &third).unwrap();
    assert_eq!(
        session.remove_queued(None, "q99"),
        Err(QueueError::UnknownMessage("q99".into()))
    );
    assert_eq!(
        session.edit_queued(None, &first, " ".into()),
        Err(QueueError::Empty)
    );
    assert_eq!(
        session.queue(None).unwrap(),
        vec![
            QueuedMessage {
                id: first.clone(),
                text: "first".into(),
                steer: false
            },
            QueuedMessage {
                id: second.clone(),
                text: "second, edited".into(),
                steer: false
            },
        ]
    );
    assert_eq!(
        session.send_message("x".into(), SendMode::Queue, Some("t7")),
        Err(QueueError::UnknownAgent("t7".into()))
    );

    // The running turn finishes, then the queue runs in order, one turn each.
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.extend(until(&mut events, "root", root_turn_ended).await);
    }
    assert_eq!(started_texts(&seen), ["first", "second, edited"]);
    assert!(session.queue(None).unwrap().is_empty());
    assert_eq!(root_prompts(&fake), ["slow", "first", "second, edited"]);
    // Delivered messages can no longer be changed.
    assert_eq!(
        session.remove_queued(None, &first),
        Err(QueueError::UnknownMessage(first.clone()))
    );
    // The journal's latest queue_changed is the (now empty) queue.
    let history = session.subscribe_with_history().0;
    let last_queue = history
        .iter()
        .rev()
        .find_map(|event| match &event.event {
            SessionEvent::QueueChanged { messages } => Some(messages.clone()),
            _ => None,
        })
        .unwrap();
    assert!(last_queue.is_empty());
}

#[tokio::test]
async fn steering_joins_the_running_turn_after_its_tool_round() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
    let session = start(&url, dir.path(), false);
    let mut events = session.subscribe();

    session.send("slow tool".into()).unwrap();
    until(&mut events, "root", |event| {
        matches!(event, SessionEvent::TurnStarted { .. })
    })
    .await;
    // Sent while the first model call is still answering.
    let id = session
        .send_message("use b.txt instead".into(), SendMode::Steer, None)
        .unwrap();
    let turn = until(&mut events, "root", root_turn_ended).await;
    let kinds: Vec<&SessionEvent> = turn.iter().map(|event| &event.event).collect();
    assert!(kinds.contains(&&SessionEvent::MessageQueued {
        id: id.clone(),
        text: "use b.txt instead".into(),
        steer: true,
    }));
    let steered = kinds
        .iter()
        .position(|event| {
            **event
                == SessionEvent::Steered {
                    id: id.clone(),
                    text: "use b.txt instead".into(),
                }
        })
        .expect("the steering message was delivered");
    let tool_done = kinds
        .iter()
        .position(|event| matches!(event, SessionEvent::ToolFinished { .. }))
        .unwrap();
    assert!(tool_done < steered, "steering waits for the tool round");
    assert!(
        matches!(
            turn.last().unwrap().event,
            SessionEvent::TurnCompleted { steps: 2, .. }
        ),
        "one turn, two model calls: {:?}",
        turn.last()
    );
    // No new turn started for it.
    assert!(started_texts(&turn).is_empty());

    // The second request of the same turn reads the steering message after the complete tool
    // pair: assistant tool call, its result, then the marked user message.
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    let roles: Vec<&str> = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool", "user"]);
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call-1");
    assert_eq!(messages[3]["tool_call_id"], "call-1");
    let steering = messages[4]["content"].as_str().unwrap();
    assert!(steering.starts_with("<steering>"));
    assert!(steering.contains("use b.txt instead"));
    assert!(session.queue(None).unwrap().is_empty());

    // Steering that arrives during a final answer keeps the turn going.
    session.send("slow".into()).unwrap();
    until(&mut events, "root", |event| {
        matches!(event, SessionEvent::TurnStarted { .. })
    })
    .await;
    session
        .send_message("one more thing".into(), SendMode::Steer, None)
        .unwrap();
    let turn = until(&mut events, "root", root_turn_ended).await;
    assert!(matches!(
        turn.last().unwrap().event,
        SessionEvent::TurnCompleted { steps: 2, .. }
    ));
    let last = fake.requests().last().unwrap().clone();
    let messages = last["messages"].as_array().unwrap();
    assert_eq!(messages[messages.len() - 2]["content"], "done slowly");
    assert!(
        messages[messages.len() - 1]["content"]
            .as_str()
            .unwrap()
            .contains("one more thing")
    );

    // With no turn running, steering is an ordinary message.
    session
        .send_message("hello".into(), SendMode::Steer, None)
        .unwrap();
    let turn = until(&mut events, "root", root_turn_ended).await;
    assert_eq!(started_texts(&turn), ["hello"]);
    assert!(
        !turn
            .iter()
            .any(|event| matches!(event.event, SessionEvent::Steered { .. }))
    );
}

#[tokio::test]
async fn the_root_agent_steers_and_queues_for_its_subagent() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
    let session = start(&url, dir.path(), false);
    let mut events = session.subscribe();

    session.send("delegate".into()).unwrap();
    let seen = until(&mut events, "root", root_turn_ended).await;
    assert!(
        matches!(
            seen.last().unwrap().event,
            SessionEvent::TurnCompleted { .. }
        ),
        "{:?}",
        seen.last()
    );
    let sub: Vec<&SessionEvent> = seen
        .iter()
        .filter(|event| event.agent_id == "t1")
        .map(|event| &event.event)
        .collect();
    assert!(sub.iter().any(|event| matches!(
        event,
        SessionEvent::MessageQueued { text, steer: true, .. } if text == "focus on b"
    )));
    assert!(sub.iter().any(|event| matches!(
        event,
        SessionEvent::Steered { text, .. } if text == "focus on b"
    )));
    // The follow-up ran as the subagent's second turn, and its report is the follow-up's.
    let turns: Vec<&String> = sub
        .iter()
        .filter_map(|event| match event {
            SessionEvent::TurnStarted { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(turns, ["sub work", "also check c"]);
    assert!(sub.iter().any(|event| matches!(
        event,
        SessionEvent::TaskFinished { ok: true, report, .. } if report == "follow-up done"
    )));

    // The subagent's second request carried the steering after its complete tool pair.
    let sub_requests: Vec<Value> = fake.requests().into_iter().filter(is_subagent).collect();
    let messages = sub_requests[1]["messages"].as_array().unwrap();
    let roles: Vec<&str> = messages
        .iter()
        .map(|message| message["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool", "user"]);
    assert_eq!(messages[3]["tool_call_id"], "sub-read");
    assert!(
        messages[4]["content"]
            .as_str()
            .unwrap()
            .contains("focus on b")
    );
    // The root collected the follow-up's report.
    let last_root = fake
        .requests()
        .into_iter()
        .rfind(|body| !is_subagent(body))
        .unwrap();
    let await_result = last_root["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(await_result.contains("follow-up done"), "{await_result}");

    // The finished subagent takes no more messages.
    assert_eq!(
        session.send_message("late".into(), SendMode::Steer, Some("t1")),
        Err(QueueError::UnknownAgent("t1".into()))
    );
}

#[tokio::test]
async fn a_resumed_session_restores_and_runs_its_queue() {
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
    session
        .send_message("after one".into(), SendMode::Queue, None)
        .unwrap();
    let last = session
        .send_message("after two".into(), SendMode::Queue, None)
        .unwrap();
    until(
        &mut events,
        "root",
        |event| matches!(event, SessionEvent::MessageQueued { id, .. } if *id == last),
    )
    .await;
    // The daemon stops: the session ends after its current turn, before the queue runs.
    drop(session);
    until(&mut events, "root", root_turn_ended).await;
    drop(events);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(root_prompts(&fake), ["slow"]);

    let resumed = start(&url, dir.path(), true);
    let queue = resumed.queue(None).unwrap();
    assert_eq!(
        queue
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["after one", "after two"]
    );
    let mut events = resumed.subscribe();
    let mut seen = Vec::new();
    for _ in 0..2 {
        seen.extend(until(&mut events, "root", root_turn_ended).await);
    }
    assert_eq!(started_texts(&seen), ["after one", "after two"]);
    assert!(resumed.queue(None).unwrap().is_empty());
    // Ids keep counting after the restored ones.
    let next = resumed
        .send_message("later".into(), SendMode::Queue, None)
        .unwrap();
    assert_eq!(next, "q4");
}
