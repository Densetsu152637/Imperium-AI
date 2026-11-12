//! A subagent that reports low confidence is retried on a stronger route: once at
//! `routing.escalation_attempts = 1`, never onto a model whose budget blocks, never as a writer.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{
    BudgetGate, BudgetVerdict, CatalogModel, ResolvedModel, Session, SessionConfig,
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

/// Runs one delegation to the weak model, which answers `CONFIDENCE: LOW`; returns how many
/// requests the weak and the strong model received and the notices the session gave.
async fn delegate(
    attempts: u32,
    blocked: Option<&'static str>,
    access: &str,
) -> (usize, usize, Vec<String>) {
    let (weak_requests, strong_requests) =
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let weak = endpoint(
        Vec::new(),
        "CONFIDENCE: LOW\nnot sure about this",
        Arc::clone(&weak_requests),
    )
    .await;
    let strong = endpoint(Vec::new(), "fixed", Arc::clone(&strong_requests)).await;
    let spawn = format!(r#"{{"brief":"do it","model":"weak","access":"{access}"}}"#);
    let root = endpoint(
        vec![("spawn_agent", spawn), ("await_tasks", "{}".into())],
        "all done",
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let row = |id: &str, spec: &str, endpoint: &Endpoint, tier| CatalogModel {
        id: id.into(),
        model: model(spec, endpoint),
        tier,
        location: imperium_config::Location::Cloud,
        tags: Vec::new(),
        named_only: false,
        cpu_lane: false,
    };
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "test".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(model("fake:model", &root)),
        catalog: vec![
            row("weak", "weak:m", &weak, imperium_config::Tier::Basic),
            row("strong", "strong:m", &strong, imperium_config::Tier::Strong),
        ],
        routing: imperium_config::RoutingSettings {
            escalation_attempts: attempts,
            ..imperium_config::RoutingSettings::default()
        },
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
        budget_gate: Some(Arc::new(Blocks(blocked))),
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
            SessionEvent::TurnCompleted { .. } => break,
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
