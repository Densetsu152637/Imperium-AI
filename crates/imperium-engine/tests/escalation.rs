//! A subagent that reports low confidence is retried on a stronger route: once at
//! `routing.escalation_attempts = 1`, never onto a model whose budget blocks, never as a writer,
//! never from an unsure local model to a hosted one, and not while a budget waits for approval or
//! the daemon chooses the models.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{
    AcquireFuture, Availability, BudgetGate, BudgetVerdict, CatalogModel, LaunchFuture, LocalLease,
    LocalModels, ResolvedModel, Session, SessionConfig, SessionLauncher,
};
use imperium_models::Endpoint;
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;

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

/// Answers each request with the next of `script` (a tool call), then with `text`; counts them.
async fn endpoint(
    script: Vec<(&'static str, String)>,
    text: &'static str,
    requests: Arc<AtomicUsize>,
) -> Endpoint {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: axum::Json<serde_json::Value>| {
            let step = requests.fetch_add(1, Ordering::SeqCst);
            let reply = match script.get(step) {
                Some((name, arguments)) => call_body(name, arguments),
                None => text_body(text),
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

/// Blocks calls to one model spec.
#[derive(Debug)]
struct Blocks(Option<&'static str>);

impl BudgetGate for Blocks {
    fn check(&self, spec: &str) -> BudgetVerdict {
        if self.0 == Some(spec) {
            return BudgetVerdict::Blocked("the strong model's budget is used up".into());
        }
        BudgetVerdict::Go
    }

    fn confirm(&self, _key: &str) {}
}

/// Asks for approval at `weak:m` once it has answered a request, so its first run goes ahead and
/// the escalation that follows finds the budget waiting.
#[derive(Debug)]
struct AsksAfterFirst(Arc<AtomicUsize>);

impl BudgetGate for AsksAfterFirst {
    fn check(&self, spec: &str) -> BudgetVerdict {
        if spec == "weak:m" && self.0.load(Ordering::SeqCst) > 0 {
            return BudgetVerdict::Ask {
                message: "the weak model's budget is used up".into(),
                key: "weak".into(),
            };
        }
        BudgetVerdict::Go
    }

    fn confirm(&self, _key: &str) {}
}

/// A local runtime whose models are all served at `url`.
#[derive(Debug)]
struct Runtime {
    url: String,
}

impl LocalModels for Runtime {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Loaded
    }

    fn acquire<'a>(&'a self, _model: &'a str, _holder: &'a str) -> AcquireFuture<'a> {
        Box::pin(async move { Ok(LocalLease::new(self.url.clone(), Some(0), None, ())) })
    }
}

/// A launcher for an initiative turn, when the daemon chooses the models.
#[derive(Debug)]
struct Initiative;

impl SessionLauncher for Initiative {
    fn launch<'a>(&'a self, _brief: &'a str, _model: Option<&'a str>) -> LaunchFuture<'a> {
        Box::pin(async { Err("not used".to_string()) })
    }

    fn model_choice_ignored(&self) -> bool {
        true
    }
}

#[derive(Default)]
enum Gate {
    #[default]
    Open,
    /// Blocks calls to this model spec.
    Blocks(&'static str),
    /// Asks at the weak model once it has run.
    AsksAtWeak,
}

/// How a delegation to the weak model is set up.
#[derive(Default)]
struct Scenario {
    attempts: u32,
    access: &'static str,
    /// The weak model is a local one, served by a fake runtime.
    weak_local: bool,
    gate: Gate,
    /// The session runs an initiative turn.
    initiative: bool,
}

/// Runs one delegation to the weak model, which answers `CONFIDENCE: LOW`; returns how many
/// requests the weak and the strong model received and the notices the session gave.
async fn run(scenario: Scenario) -> (usize, usize, Vec<String>) {
    let (weak_requests, strong_requests) =
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let weak = endpoint(
        Vec::new(),
        "CONFIDENCE: LOW\nnot sure about this",
        Arc::clone(&weak_requests),
    )
    .await;
    let strong = endpoint(Vec::new(), "fixed", Arc::clone(&strong_requests)).await;
    let spawn = format!(
        r#"{{"brief":"do it","model":"weak","access":"{}"}}"#,
        scenario.access
    );
    let root = endpoint(
        vec![("spawn_agent", spawn), ("await_tasks", "{}".into())],
        "all done",
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let row = |id: &str, spec: &str, endpoint: &Endpoint, tier, local: bool| {
        let mut model = model(spec, endpoint);
        model.local = local;
        CatalogModel {
            id: id.into(),
            model,
            tier,
            location: if local {
                imperium_config::Location::Local
            } else {
                imperium_config::Location::Cloud
            },
            tags: Vec::new(),
            named_only: false,
            cpu_lane: false,
        }
    };
    let gate: Arc<dyn BudgetGate> = match scenario.gate {
        Gate::Open => Arc::new(Blocks(None)),
        Gate::Blocks(spec) => Arc::new(Blocks(Some(spec))),
        Gate::AsksAtWeak => Arc::new(AsksAfterFirst(Arc::clone(&weak_requests))),
    };
    let local: Option<Arc<dyn LocalModels>> = scenario.weak_local.then(|| {
        Arc::new(Runtime {
            url: weak.base_url.clone(),
        }) as Arc<dyn LocalModels>
    });
    let launcher: Option<Arc<dyn SessionLauncher>> = scenario
        .initiative
        .then(|| Arc::new(Initiative) as Arc<dyn SessionLauncher>);
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "test".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(model("fake:model", &root)),
        catalog: vec![
            row(
                "weak",
                "weak:m",
                &weak,
                imperium_config::Tier::Basic,
                scenario.weak_local,
            ),
            row(
                "strong",
                "strong:m",
                &strong,
                imperium_config::Tier::Strong,
                false,
            ),
        ],
        routing: imperium_config::RoutingSettings {
            escalation_attempts: scenario.attempts,
            ..imperium_config::RoutingSettings::default()
        },
        clients: Default::default(),
        local,
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher,
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
        journal_path: dir.path().join(".journal/test.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("delegate".into()).unwrap();
    let mut notices = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        match event.event {
            SessionEvent::Notice { message } => notices.push(message),
            SessionEvent::TurnCompleted { .. }
                if event.agent_id == imperium_protocol::session::ROOT_AGENT =>
            {
                break;
            }
            SessionEvent::TurnFailed { message } => panic!("the turn failed: {message}"),
            _ => {}
        }
    }
    (
        weak_requests.load(Ordering::SeqCst),
        strong_requests.load(Ordering::SeqCst),
        notices,
    )
}

async fn delegate(
    attempts: u32,
    blocked: Option<&'static str>,
    access: &'static str,
) -> (usize, usize, Vec<String>) {
    run(Scenario {
        attempts,
        access,
        gate: blocked.map_or(Gate::Open, Gate::Blocks),
        ..Scenario::default()
    })
    .await
}

#[tokio::test]
async fn an_unsure_subagent_is_retried_once_on_the_stronger_model() {
    let (weak, strong, notices) = delegate(1, None, "read").await;
    assert_eq!((weak, strong), (1, 1), "{notices:?}");
    assert!(
        notices.iter().any(|n| n.contains("retrying on strong:m")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn escalation_is_off_by_default_and_by_attempts_zero() {
    let (weak, strong, _) = delegate(0, None, "read").await;
    assert_eq!((weak, strong), (1, 0));
}

#[tokio::test]
async fn a_stronger_model_whose_budget_blocks_is_not_tried() {
    let (weak, strong, notices) = delegate(1, Some("strong:m"), "read").await;
    assert_eq!((weak, strong), (1, 0), "{notices:?}");
    assert!(
        notices
            .iter()
            .any(|n| n.contains("not retried on strong:m")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn a_writer_is_not_retried() {
    let (weak, strong, _) = delegate(1, None, "write").await;
    assert_eq!((weak, strong), (1, 0));
}

#[tokio::test]
async fn an_unsure_local_model_is_never_retried_on_a_hosted_one() {
    let (weak, strong, notices) = run(Scenario {
        attempts: 1,
        access: "read",
        weak_local: true,
        ..Scenario::default()
    })
    .await;
    assert_eq!((weak, strong), (1, 0), "{notices:?}");
    assert!(
        notices
            .iter()
            .any(|n| n.contains("is hosted, so it is not retried")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn a_budget_waiting_for_approval_at_the_model_that_ran_stops_the_escalation() {
    let (weak, strong, notices) = run(Scenario {
        attempts: 1,
        access: "read",
        gate: Gate::AsksAtWeak,
        ..Scenario::default()
    })
    .await;
    assert_eq!((weak, strong), (1, 0), "{notices:?}");
    assert!(
        notices
            .iter()
            .any(|n| n.contains("waiting for approval, so it is not retried")),
        "{notices:?}"
    );
}

#[tokio::test]
async fn nothing_is_escalated_on_an_initiative_turn() {
    let (weak, strong, notices) = run(Scenario {
        attempts: 1,
        access: "read",
        initiative: true,
        ..Scenario::default()
    })
    .await;
    assert_eq!((weak, strong), (1, 0), "{notices:?}");
    assert!(
        !notices.iter().any(|n| n.contains("retrying")),
        "{notices:?}"
    );
}
