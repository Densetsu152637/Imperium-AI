//! The checkpoint of a session's files is taken beside a turn's first model call, not before
//! it: reading every file of a large folder takes seconds to minutes, and a turn that waited
//! for it showed nothing meanwhile. A tool call, which could change a file, still waits for it,
//! and so does the end of the turn.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use imperium_api::{StartFuture, ToolCheck, ToolContext, ToolFuture, ToolProvider, ToolSpec};
use imperium_engine::{ResolvedModel, Session, SessionConfig, TurnCheckpoints};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

/// A checkpoint that takes as long as the test says.
#[derive(Debug)]
struct SlowCheckpoints {
    release: Mutex<mpsc::Receiver<()>>,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl TurnCheckpoints for SlowCheckpoints {
    fn before_turn(&self, _prompt: &str) {
        let _ = self.release.lock().unwrap().recv();
        self.order.lock().unwrap().push("checkpoint taken");
    }

    fn after_turn(&self) {
        self.order.lock().unwrap().push("turn ended");
    }
}

/// A tool that notes when it ran.
#[derive(Debug)]
struct Marker {
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl ToolProvider for Marker {
    fn start(&self) -> StartFuture<'_> {
        Box::pin(async { Vec::new() })
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "mark".into(),
            description: "Change a file.".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }]
    }

    fn handles(&self, name: &str) -> bool {
        name == "mark"
    }

    fn check(&self, _name: &str, _arguments: &str) -> Result<ToolCheck, String> {
        Ok(ToolCheck {
            summary: "change a file".into(),
            ask: false,
            needs_context: false,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a ToolContext<'a>,
        _name: &'a str,
        _arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            self.order.lock().unwrap().push("tool ran");
            Ok("done".to_string())
        })
    }
}

fn sse(chunk: Value) -> String {
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20}}"#
    )
}

/// A session on the model at `base_url`, its checkpoint held until the returned sender
/// releases it.
fn session_with(
    order: &Arc<Mutex<Vec<&'static str>>>,
    base_url: String,
) -> (Session, mpsc::Sender<()>, tempfile::TempDir) {
    let (release, wait) = mpsc::channel();
    let checkpoints = Arc::new(SlowCheckpoints {
        release: Mutex::new(wait),
        order: Arc::clone(order),
    });
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "checkpoint".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url,
                api_key: None,
            },
            model: "model".into(),
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
        tools: vec![Arc::new(Marker {
            order: Arc::clone(order),
        }) as Arc<dyn ToolProvider>],
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
        checkpoints: Some(checkpoints),
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
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    (session, release, dir)
}

/// Serves `bodies` one per request (the last repeats); returns the base URL and the request count.
async fn serve(bodies: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let requests = Arc::clone(&requests);
            move |Json(_): Json<Value>| {
                let index = requests
                    .fetch_add(1, Ordering::SeqCst)
                    .min(bodies.len() - 1);
                let body = bodies[index].clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (base_url, requests)
}

async fn until_asked(requests: &AtomicUsize) {
    let started = std::time::Instant::now();
    while requests.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the model was not asked while the checkpoint was being taken"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_order(order: &Mutex<Vec<&'static str>>, len: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while order.lock().unwrap().len() < len {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the turn's end is recorded");
}

async fn until_completed(events: &mut tokio::sync::broadcast::Receiver<AgentEvent>) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("turn failed: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_slow_checkpoint_delays_neither_the_turn_nor_its_model_call_but_the_first_tool() {
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let (base_url, requests) = serve(vec![
        sse(
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
            "function":{"name":"mark","arguments":"{}"}}]}}]}),
        ),
        sse(json!({"choices":[{"delta":{"content":"Done."}}]})),
    ])
    .await;
    let (session, release, _dir) = session_with(&order, base_url);
    let mut events = session.subscribe();
    session.send("change a file".into()).unwrap();

    // The model is asked while the checkpoint is still being taken.
    until_asked(&requests).await;
    // Its tool call waits: no file may change before the checkpoint has them as they were.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        order.lock().unwrap().is_empty(),
        "the tool ran before the checkpoint was taken: {:?}",
        order.lock().unwrap()
    );

    release.send(()).unwrap();
    until_completed(&mut events).await;
    // The checkpoint comes before the tool, and the turn's end after both.
    wait_for_order(&order, 3).await;
    assert_eq!(
        *order.lock().unwrap(),
        ["checkpoint taken", "tool ran", "turn ended"]
    );
}

#[tokio::test]
async fn a_turn_without_tools_ends_after_its_checkpoint_and_is_still_compared() {
    let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let (base_url, requests) = serve(vec![sse(
        json!({"choices":[{"delta":{"content":"Hello."}}]}),
    )])
    .await;
    let (session, release, _dir) = session_with(&order, base_url);
    let mut events = session.subscribe();
    session.send("hello".into()).unwrap();
    until_asked(&requests).await;

    // The reply streams, but the turn does not end while its files are being recorded: what the
    // user changes after it ends is not the turn's.
    let mut completed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(700);
    while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
        completed |= matches!(event.event, SessionEvent::TurnCompleted { .. });
    }
    assert!(!completed, "the turn ended before its checkpoint was taken");

    release.send(()).unwrap();
    until_completed(&mut events).await;
    // Changes made meanwhile by background commands or subagents are compared all the same.
    wait_for_order(&order, 2).await;
    assert_eq!(*order.lock().unwrap(), ["checkpoint taken", "turn ended"]);
}
