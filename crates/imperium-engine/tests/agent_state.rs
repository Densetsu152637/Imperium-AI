//! Agent state: a finished subagent stays idle with its conversation, and is resumed with a
//! follow-up (by the root's `resume_agent` or by the user) that its model reads after its
//! earlier messages; resumed agents take queued messages; usage adds up per agent; and the
//! state survives a restart, where a subagent the restart cut off is marked interrupted and
//! resumes too.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, AgentStatus, SendMode, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// A fake endpoint recording every request. Every answer reports 10 prompt and 2 completion
/// tokens. Root: `delegate` spawns a subagent on `explore`; `follow up` lists the agents and
/// then resumes t1 with `now summarise`; a spawn or resume result leads to `await_tasks`; the
/// reports end the turn. Subagent: `explore` reads a.txt, then answers `explored`; `slow more`
/// waits, then answers `slow done`; anything else is answered `sub: <message>`.
#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<Value>>,
}

impl Fake {
    fn subagent_requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|body| is_subagent(body))
            .cloned()
            .collect()
    }
}

fn stream(delta: Value) -> String {
    let chunk = json!({"choices": [{"delta": delta}],
                       "usage": {"prompt_tokens": 10, "completion_tokens": 2}});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn text(content: &str) -> String {
    stream(json!({"content": content}))
}

fn call(id: &str, name: &str, arguments: Value) -> String {
    stream(json!({"tool_calls": [{"index": 0, "id": id,
        "function": {"name": name, "arguments": arguments.to_string()}}]}))
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
    let said = last["content"].as_str().unwrap_or_default().to_string();
    let role = last["role"].as_str().unwrap_or_default();
    if is_subagent(body) {
        return match (role, said.as_str()) {
            ("user", "explore") => call("sub-read", "read_file", json!({"path": "a.txt"})),
            ("tool", _) => text("explored"),
            ("user", "slow more") => {
                tokio::time::sleep(Duration::from_millis(700)).await;
                text("slow done")
            }
            _ => text(&format!("sub: {}", said.lines().last().unwrap_or_default())),
        };
    }
    match (role, said.as_str()) {
        ("user", "delegate") => call("spawn", "spawn_agent", json!({"brief": "explore"})),
        ("user", "follow up") => call("list", "list_agents", json!({})),
        ("tool", said) if said.contains("- t1 (idle, resumable") => call(
            "resume",
            "resume_agent",
            json!({"agent_id": "t1", "text": "now summarise"}),
        ),
        ("tool", said) if said.starts_with("started ") || said.starts_with("resumed ") => {
            call("await", "await_tasks", json!({}))
        }
        ("tool", said) if said.starts_with("## ") => text("all done"),
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
        keep_agent_state: true,
        background: None,
        effort: Default::default(),
        journal_path: dir.join(".sessions/s.jsonl"),
    })
    .unwrap()
}

/// Waits for an event from `agent` matching `wanted`; returns the events up to it.
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

fn turn_ended(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
    )
}

fn task_finished(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::TaskFinished { .. })
}

fn roles(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["role"].as_str().unwrap().to_string())
        .collect()
}

fn contents(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Runs `delegate`: t1 explores and finishes.
async fn delegate(session: &Session, events: &mut broadcast::Receiver<AgentEvent>) {
    session.send("delegate".into()).unwrap();
    let seen = until(events, "root", turn_ended).await;
    assert!(
        matches!(
            seen.last().unwrap().event,
            SessionEvent::TurnCompleted { .. }
        ),
        "{:?}",
        seen.last()
    );
}

#[tokio::test]
async fn a_finished_subagent_is_resumed_with_its_conversation() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
    let session = start(&url, dir.path(), false);
    let mut events = session.subscribe();
    delegate(&session, &mut events).await;

    // Finished, t1 is idle with its conversation kept.
    let agents = session.agents();
    assert_eq!(agents[0].agent_id, "root");
    let t1 = &agents[1];
    assert_eq!(
        (t1.agent_id.as_str(), t1.status, t1.resumable),
        ("t1", AgentStatus::Idle, true)
    );
    assert_eq!(t1.parent.as_deref(), Some("root"));
    // Titled from its brief, since the root named none.
    assert_eq!((t1.title.as_str(), t1.role.as_str()), ("Explore", "leaf"));
    assert_eq!(t1.last_report.as_deref(), Some("explored"));
    assert_eq!(t1.messages, 4);

    // The root lists its agents and resumes t1, then collects its report with await_tasks.
    session.send("follow up".into()).unwrap();
    let seen = until(&mut events, "root", turn_ended).await;
    assert!(seen.iter().any(|event| matches!(&event.event,
        SessionEvent::TaskResumed { task_id, text, model, .. }
            if task_id == "t1" && text == "now summarise" && model == "fake:m")));
    let report = seen.iter().find_map(|event| match &event.event {
        SessionEvent::ToolFinished { output, .. } if output.starts_with("## t1") => Some(output),
        _ => None,
    });
    assert!(report.unwrap().contains("sub: now summarise"), "{report:?}");
    // The resumed run's report counts only its own model call.
    let resumed_usage = seen.iter().rev().find_map(|event| match &event.event {
        SessionEvent::TaskFinished { task_id, usage, .. } if task_id == "t1" => Some(*usage),
        _ => None,
    });
    assert_eq!(resumed_usage.unwrap().prompt_tokens, 10);

    // Its model read the whole earlier conversation before the follow-up.
    let requests = fake.subagent_requests();
    let resumed = requests.last().unwrap();
    assert_eq!(
        roles(resumed),
        ["system", "user", "assistant", "tool", "assistant", "user"]
    );
    let said = contents(resumed);
    assert_eq!(said[1], "explore");
    assert!(said[3].contains("alpha"));
    assert_eq!(said[4], "explored");
    assert_eq!(said[5], "now summarise");

    // Per-agent usage adds up across runs: two calls exploring, one resumed.
    let t1 = session
        .agents()
        .into_iter()
        .find(|agent| agent.agent_id == "t1")
        .unwrap();
    assert_eq!((t1.status, t1.turns), (AgentStatus::Idle, 2));
    assert_eq!(
        (t1.usage.prompt_tokens, t1.usage.completion_tokens),
        (30, 6)
    );

    // The user resumes it too; while it works, a queued message runs as its next turn.
    assert_eq!(
        session.resume_agent("t1", "slow more".into()).unwrap(),
        "fake:m"
    );
    until(&mut events, "t1", |event| {
        matches!(event, SessionEvent::TurnStarted { .. })
    })
    .await;
    assert!(
        session
            .resume_agent("t1", "again".into())
            .unwrap_err()
            .contains("still working")
    );
    session
        .send_message("and one more".into(), SendMode::Queue, Some("t1"))
        .unwrap();
    let seen = until(&mut events, "t1", task_finished).await;
    let turns: Vec<&String> = seen
        .iter()
        .filter(|event| event.agent_id == "t1")
        .filter_map(|event| match &event.event {
            SessionEvent::TurnStarted { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(turns, ["and one more"]);
    assert!(matches!(&seen.last().unwrap().event,
        SessionEvent::TaskFinished { ok: true, report, .. } if report == "sub: and one more"));
    let last = fake.subagent_requests().last().unwrap().clone();
    assert_eq!(roles(&last).len(), 10);

    // Neither the root nor an unknown agent can be resumed, nor with nothing to say.
    assert!(session.resume_agent("root", "hi".into()).is_err());
    assert!(session.resume_agent("t9", "hi".into()).is_err());
    assert!(session.resume_agent("t1", "  ".into()).is_err());
}

#[tokio::test]
async fn agent_state_survives_a_restart() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
    let first = start(&url, dir.path(), false);
    let mut events = first.subscribe();
    delegate(&first, &mut events).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let kept = dir.path().join(".sessions/s.agents");
    assert!(kept.join("t1.json").is_file());
    assert!(kept.join("root.json").is_file());

    let resumed = start(&url, dir.path(), true);
    let mut events = resumed.subscribe();
    let agents = resumed.agents();
    let ids: Vec<&str> = agents.iter().map(|agent| agent.agent_id.as_str()).collect();
    assert_eq!(ids, ["root", "t1"]);
    assert_eq!(
        (agents[1].status, agents[1].resumable),
        (AgentStatus::Idle, true)
    );

    // Resumed after the restart, t1 still has its conversation.
    let mut model = None;
    for _ in 0..50 {
        match resumed.resume_agent("t1", "after the restart".into()) {
            Ok(spec) => {
                model = Some(spec);
                break;
            }
            Err(error) if error.contains("still starting") => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("{error}"),
        }
    }
    assert_eq!(model.as_deref(), Some("fake:m"));
    until(&mut events, "t1", task_finished).await;
    let last = fake.subagent_requests().last().unwrap().clone();
    let said = contents(&last);
    assert_eq!(said[1], "explore");
    assert!(said[3].contains("alpha"));
    assert_eq!(said[4..], ["explored", "after the restart"]);

    // New tasks continue the numbering.
    resumed.send("delegate".into()).unwrap();
    let seen = until(&mut events, "root", turn_ended).await;
    assert!(seen.iter().any(|event| matches!(&event.event,
        SessionEvent::TaskSpawned { task_id, .. } if task_id == "t2")));
}

#[tokio::test]
async fn a_subagent_the_restart_cut_off_resumes() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
    let first = start(&url, dir.path(), false);
    let mut events = first.subscribe();
    delegate(&first, &mut events).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // As if the daemon stopped while t1 worked on a follow-up: its state says running and the
    // journal ends inside its turn.
    let record = dir.path().join(".sessions/s.agents/t1.json");
    let mut state: Value =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    state["status"] = json!("running");
    std::fs::write(&record, state.to_string()).unwrap();
    let journal = dir.path().join(".sessions/s.jsonl");
    let mut text = std::fs::read_to_string(&journal).unwrap();
    text.push_str(concat!(
        r#"{"at_ms":0,"agent_id":"root","event":{"type":"task_resumed","task_id":"t1","text":"check b","model":"fake:m"}}"#,
        "\n",
        r#"{"at_ms":0,"agent_id":"t1","event":{"type":"turn_started","text":"check b"}}"#,
        "\n"
    ));
    std::fs::write(&journal, text).unwrap();

    let resumed = start(&url, dir.path(), true);
    let mut events = resumed.subscribe();
    // The cut-off task is reported as ended, and t1 is interrupted.
    let mut interrupted = false;
    for _ in 0..50 {
        let agents = resumed.agents();
        if agents.iter().any(|agent| {
            agent.agent_id == "t1" && agent.status == AgentStatus::Interrupted && agent.resumable
        }) {
            interrupted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(interrupted);
    let (history, _) = resumed.subscribe_with_history();
    assert!(history.iter().any(|event| event.agent_id == "t1"
        && matches!(&event.event, SessionEvent::TaskFinished { ok: false, report, .. }
            if report.contains("daemon stopped"))));

    resumed.resume_agent("t1", "carry on".into()).unwrap();
    until(&mut events, "t1", |event| {
        matches!(event, SessionEvent::TaskFinished { ok: true, .. })
    })
    .await;
    let last = fake.subagent_requests().last().unwrap().clone();
    let follow_up = contents(&last).last().unwrap().clone();
    assert!(follow_up.contains("The daemon stopped while you were working"));
    assert!(follow_up.ends_with("carry on"));
    let t1 = resumed
        .agents()
        .into_iter()
        .find(|agent| agent.agent_id == "t1")
        .unwrap();
    assert_eq!(t1.status, AgentStatus::Idle);
}
