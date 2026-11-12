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
    /// Requests that reached the endpoint so far.
    served: Arc<AtomicUsize>,
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
            && self.served.load(Ordering::SeqCst) >= self.allowed
        {
            return (self.then)();
        }
        BudgetVerdict::Go
    }

    fn confirm(&self, key: &str) {
        self.confirmed.lock().unwrap().push(key.into());
    }
}

fn gate(
    served: &Arc<AtomicUsize>,
    allowed: usize,
    then: fn() -> BudgetVerdict,
    refused: Option<&'static str>,
) -> Arc<Gate> {
    Arc::new(Gate {
        served: Arc::clone(served),
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
    start_with(endpoint, gate, dir, Vec::new())
}

fn start_with(
    endpoint: &Endpoint,
    gate: Arc<dyn BudgetGate>,
    dir: &std::path::Path,
    pre_approved: Vec<String>,
) -> Session {
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
        pre_approved,
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
    let session = start(&endpoint, gate(&requests, 1, blocked, None), dir.path());
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
        gate(&requests, usize::MAX, blocked, Some("openrouter:model")),
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
    let gate = gate(&requests, 1, ask, None);
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

/// Asks about each of its keys until the person approved it.
#[derive(Debug)]
struct Asks {
    keys: Vec<&'static str>,
    confirmed: Mutex<Vec<String>>,
}

impl BudgetGate for Asks {
    fn check(&self, _spec: &str) -> BudgetVerdict {
        let confirmed = self.confirmed.lock().unwrap();
        for key in &self.keys {
            if !confirmed.iter().any(|done| done == key) {
                return BudgetVerdict::Ask {
                    message: format!("{key} is used up."),
                    key: (*key).into(),
                };
            }
        }
        BudgetVerdict::Go
    }

    fn confirm(&self, key: &str) {
        self.confirmed.lock().unwrap().push(key.into());
    }
}

fn asks(keys: &[&'static str]) -> Arc<Asks> {
    Arc::new(Asks {
        keys: keys.to_vec(),
        confirmed: Mutex::new(Vec::new()),
    })
}

/// Runs a turn, answering every permission request with `decision`; returns the requests'
/// summaries and how the turn ended (`Err` with the failure message).
async fn run_answering(
    session: &Session,
    events: &mut broadcast::Receiver<AgentEvent>,
    decision: PermissionDecision,
) -> (Vec<String>, Result<(), String>) {
    session.send("go".into()).unwrap();
    let mut requests = Vec::new();
    let ended = loop {
        match next(events).await {
            (
                _,
                SessionEvent::PermissionRequested {
                    request_id,
                    summary,
                    ..
                },
            ) => {
                requests.push(summary);
                session.respond(request_id, decision).unwrap();
            }
            (_, SessionEvent::TurnCompleted { .. }) => break Ok(()),
            (_, SessionEvent::TurnFailed { message }) => break Err(message),
            _ => {}
        }
    };
    // Anything raised after the turn ended still counts.
    while let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(300), events.recv()).await
    {
        if let SessionEvent::PermissionRequested { summary, .. } = event.event {
            requests.push(summary);
        }
    }
    (requests, ended)
}

#[tokio::test]
async fn several_used_up_ask_budgets_are_each_asked() {
    let requests = Arc::new(AtomicUsize::new(0));
    let endpoint = endpoint(Vec::new(), Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    let gate = asks(&["k1", "k2"]);
    let session = start_with(&endpoint, gate.clone(), dir.path(), Vec::new());
    let mut events = session.subscribe();
    let (prompts, ended) = run_answering(&session, &mut events, PermissionDecision::Allow).await;
    assert!(ended.is_ok());
    assert_eq!(prompts, ["k1 is used up.", "k2 is used up."]);
    assert_eq!(*gate.confirmed.lock().unwrap(), ["k1", "k2"]);
}

#[tokio::test]
async fn a_declined_ask_fails_the_turn_before_the_call() {
    let requests = Arc::new(AtomicUsize::new(0));
    let endpoint = endpoint(Vec::new(), Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start_with(&endpoint, asks(&["k1"]), dir.path(), Vec::new());
    let mut events = session.subscribe();
    let (prompts, ended) = run_answering(&session, &mut events, PermissionDecision::Deny).await;
    assert_eq!(prompts.len(), 1);
    assert!(ended.unwrap_err().contains("not approved"));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pre_approved_tools_and_full_mode_do_not_skip_the_ask() {
    let requests = Arc::new(AtomicUsize::new(0));
    let endpoint = endpoint(Vec::new(), Arc::clone(&requests)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start_with(
        &endpoint,
        asks(&["k1"]),
        dir.path(),
        vec!["budget".into(), "list_agents".into()],
    );
    let mut events = session.subscribe();
    let (prompts, ended) = run_answering(&session, &mut events, PermissionDecision::Allow).await;
    assert!(ended.is_ok());
    assert_eq!(
        prompts.len(),
        1,
        "a pre-approved tool list does not skip it"
    );

    let dir = tempfile::tempdir().unwrap();
    let session = start_with(&endpoint, asks(&["k1"]), dir.path(), Vec::new());
    session
        .workspace()
        .permissions()
        .set_mode(imperium_tools::permissions::PermissionMode::Full);
    let mut events = session.subscribe();
    let (prompts, _) = run_answering(&session, &mut events, PermissionDecision::Allow).await;
    assert_eq!(prompts.len(), 1, "full mode does not skip it");
}

/// Lets the first check through, then asks like `Asks`.
#[derive(Debug)]
struct AfterFirst(Arc<Asks>, AtomicUsize);

impl BudgetGate for AfterFirst {
    fn check(&self, spec: &str) -> BudgetVerdict {
        if self.1.fetch_add(1, Ordering::SeqCst) == 0 {
            return BudgetVerdict::Go;
        }
        self.0.check(spec)
    }

    fn confirm(&self, key: &str) {
        self.0.confirm(key);
    }
}

#[tokio::test]
async fn parallel_subagents_raise_one_prompt() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: axum::Json<serde_json::Value>| {
            let step = app_requests.fetch_add(1, Ordering::SeqCst);
            let reply = if step == 0 {
                let calls: Vec<_> = (0..3)
                    .map(|index| {
                        serde_json::json!({"index":index,"id":format!("c{index}"),
                            "function":{"name":"spawn_agent",
                            "arguments":format!(r#"{{"brief":"task {index}"}}"#)}})
                    })
                    .collect();
                let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":calls}}]});
                format!("data: {chunk}\n\ndata: [DONE]\n\n")
            } else {
                text_body("done")
            };
            async move { ([("content-type", "text/event-stream")], reply) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        api_key: None,
    };
    tokio::spawn(async move { axum::serve(listener, app).await });
    let dir = tempfile::tempdir().unwrap();
    let inner = asks(&["k1"]);
    // Everything but the root's first call asks, the subagents' first calls included.
    let gate = Arc::new(AfterFirst(Arc::clone(&inner), AtomicUsize::new(0)));
    let session = start_with(&endpoint, gate, dir.path(), Vec::new());
    let mut events = session.subscribe();
    let (prompts, ended) = run_answering(&session, &mut events, PermissionDecision::Allow).await;
    assert!(ended.is_ok(), "{ended:?}");
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_eq!(*inner.confirmed.lock().unwrap(), ["k1"]);
}
