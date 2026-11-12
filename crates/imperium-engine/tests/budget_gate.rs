//! A budget gate stops spend inside a running turn: before each model call, and at a spawn.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{
    BudgetGate, BudgetVerdict, CatalogModel, ResolvedModel, Session, SessionConfig,
};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use tokio::sync::broadcast;

fn text_body(text: &str) -> String {
    let chunk = serde_json::json!({"choices":[{"delta":{"content":text}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":10}}"#
    )
}

fn call_body(name: &str, arguments: &str) -> String {
    let call =
        serde_json::json!([{"index":0,"id":"a","function":{"name":name,"arguments":arguments}}]);
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":call}}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// The root runs `tools` one step each, then answers; counts every request that reaches it.
async fn endpoint(tools: Vec<(&'static str, String)>, requests: Arc<AtomicUsize>) -> Endpoint {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: axum::Json<serde_json::Value>| {
            let step = requests.fetch_add(1, Ordering::SeqCst);
            let reply = match tools.get(step) {
                Some((name, arguments)) => call_body(name, arguments),
                None => text_body("done"),
            };
            async move { ([("content-type", "text/event-stream")], reply) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    Endpoint {
        base_url,
        api_key: None,
    }
}

fn model(spec: &str, endpoint: &Endpoint) -> ResolvedModel {
    ResolvedModel {
        spec: spec.into(),
        endpoint: endpoint.clone(),
        model: "model".into(),
        local: false,
        context_window: None,
        kind: "openai".into(),
        reasoning: None,
        pricing: None,
    }
}

/// Lets `allowed` calls to the root's model through, then answers `then`; refuses `refused`.
#[derive(Debug)]
struct Gate {
    seen: AtomicUsize,
    allowed: usize,
    then: fn() -> BudgetVerdict,
    refused: Option<&'static str>,
    confirmed: Mutex<Vec<String>>,
}

impl BudgetGate for Gate {
    fn check(&self, spec: &str) -> BudgetVerdict {
        if self.refused == Some(spec) {
            return BudgetVerdict::Blocked("openrouter is used up; calls are blocked.".into());
        }
        if spec != "fake:model" {
            return BudgetVerdict::Go;
        }
        if self.confirmed.lock().unwrap().is_empty()
            && self.seen.fetch_add(1, Ordering::SeqCst) >= self.allowed
        {
            return (self.then)();
        }
        BudgetVerdict::Go
    }

    fn confirm(&self, key: &str) {
        self.confirmed.lock().unwrap().push(key.into());
    }
}

fn gate(allowed: usize, then: fn() -> BudgetVerdict, refused: Option<&'static str>) -> Arc<Gate> {
    Arc::new(Gate {
        seen: AtomicUsize::new(0),
        allowed,
        then,
        refused,
        confirmed: Mutex::new(Vec::new()),
    })
}

fn blocked() -> BudgetVerdict {
    BudgetVerdict::Blocked("Daily is used up; new turns and model calls are blocked.".into())
}

fn ask() -> BudgetVerdict {
    BudgetVerdict::Ask {
        message: "Daily is used up. Go on?".into(),
        key: "k".into(),
    }
}

fn start(endpoint: &Endpoint, gate: Arc<Gate>, dir: &std::path::Path) -> Session {
    let other = CatalogModel {
        id: "other".into(),
        model: model("openrouter:model", endpoint),
        tier: Default::default(),
        location: imperium_config::Location::Cloud,
        tags: Vec::new(),
        named_only: false,
    };
    Session::spawn(SessionConfig {
        id: "test".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(model("fake:model", endpoint)),
        catalog: vec![other],
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
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 5,
        background: None,
        checkpoints: None,
        budget_gate: Some(gate),
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".journal/test.jsonl"),
    })
    .unwrap()
}

async fn next(events: &mut broadcast::Receiver<AgentEvent>) -> (String, SessionEvent) {
    let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("an event within 10 s")
        .unwrap();
    (event.agent_id, event.event)
}

#[tokio::test]
async fn a_budget_used_up_mid_turn_stops_the_next_model_call() {
    let requests = Arc::new(AtomicUsize::new(0));
    let endpoint = endpoint(vec![("list_agents", "{}".into())], Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(&endpoint, gate(1, blocked, None), dir.path());
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    loop {
        match next(&mut events).await {
            (_, SessionEvent::TurnFailed { message }) => {
                assert!(message.contains("model calls are blocked"), "{message}");
                break;
            }
            (_, SessionEvent::TurnCompleted { .. }) => panic!("the turn ran to the end"),
            _ => {}
        }
    }
    // The first call went out; the second never reached the endpoint.
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_spawn_onto_a_blocked_model_is_refused() {
    let requests = Arc::new(AtomicUsize::new(0));
    let spawn = r#"{"brief":"do it","model":"other"}"#;
    let endpoint = endpoint(vec![("spawn_agent", spawn.into())], Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(
        &endpoint,
        gate(usize::MAX, blocked, Some("openrouter:model")),
        dir.path(),
    );
    let mut events = session.subscribe();
    session.send("delegate".into()).unwrap();
    let mut refused = false;
    loop {
        match next(&mut events).await {
            (_, SessionEvent::TaskSpawned { .. }) => panic!("the subagent was spawned"),
            (_, SessionEvent::ToolFinished { ok, output, .. }) => {
                assert!(!ok);
                assert!(output.contains("is used up"), "{output}");
                refused = true;
            }
            (_, SessionEvent::TurnCompleted { .. }) => break,
            _ => {}
        }
    }
    assert!(refused);
    // Only the root's two calls went out: the spawn and the answer after it.
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_ask_budget_pauses_for_approval_then_goes_on() {
    let requests = Arc::new(AtomicUsize::new(0));
    let endpoint = endpoint(vec![("list_agents", "{}".into())], Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    let gate = gate(1, ask, None);
    let session = start(&endpoint, Arc::clone(&gate), dir.path());
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    loop {
        match next(&mut events).await {
            (
                _,
                SessionEvent::PermissionRequested {
                    request_id,
                    summary,
                    ..
                },
            ) => {
                assert!(summary.contains("Go on?"));
                // Paused: the second call has not gone out.
                assert_eq!(requests.load(Ordering::SeqCst), 1);
                session
                    .respond(request_id, PermissionDecision::Allow)
                    .unwrap();
            }
            (_, SessionEvent::TurnCompleted { .. }) => break,
            (_, SessionEvent::TurnFailed { message }) => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(*gate.confirmed.lock().unwrap(), ["k"]);
}
