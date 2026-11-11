//! The engine's hooks into the local runtime's slot residency: a session the user stops
//! releases its agents' KV slots (`LocalModels::suspend`) while it is stopped and takes them back
//! on resume, and an agent whose context was saved meanwhile (`LocalModels::suspended`) has it
//! restored ahead of its next model call (`ensure_resident`), after a resume or a steer. The
//! runtime is a fake that records what it was asked.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{
    AcquireFuture, Availability, LocalLease, LocalModels, LocalRequest, ResolvedModel, Session,
    SessionConfig,
};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SendMode, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// What the fake runtime was asked, in order.
#[derive(Debug, Default)]
struct Calls {
    /// `acquire <holder> <prompt tokens>`, `suspend <holder>`.
    log: Mutex<Vec<String>>,
    /// Holders whose context the runtime holds saved.
    saved: Mutex<HashSet<String>>,
}

#[derive(Debug)]
struct Runtime {
    base_url: String,
    calls: Arc<Calls>,
}

impl LocalModels for Runtime {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Loaded
    }

    fn suspended(&self, holder: &str) -> bool {
        self.calls.saved.lock().unwrap().contains(holder)
    }

    fn suspend<'a>(
        &'a self,
        _model: &'a str,
        holder: &'a str,
    ) -> imperium_engine::SuspendFuture<'a> {
        Box::pin(async move {
            self.calls
                .log
                .lock()
                .unwrap()
                .push(format!("suspend {holder}"));
            self.calls.saved.lock().unwrap().insert(holder.to_string());
        })
    }

    fn acquire<'a>(&'a self, model: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        self.acquire_with(model, holder, LocalRequest::default())
    }

    fn acquire_with<'a>(
        &'a self,
        _model: &'a str,
        holder: &'a str,
        request: LocalRequest,
    ) -> AcquireFuture<'a> {
        Box::pin(async move {
            let prompt = request
                .prompt_tokens
                .map_or_else(|| "-".to_string(), |tokens| tokens.to_string());
            self.calls
                .log
                .lock()
                .unwrap()
                .push(format!("acquire {holder} {prompt}"));
            // Taking a slot restores the saved context into it.
            self.calls.saved.lock().unwrap().remove(holder);
            Ok(LocalLease::new(self.base_url.clone(), Some(0), None, ()))
        })
    }
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// The first call reads a file slowly; the next answers.
async fn model_server() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| async move {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let tool_results = messages.iter().filter(|m| m["role"] == "tool").count();
            let reply = if tool_results == 0 {
                tokio::time::sleep(Duration::from_millis(600)).await;
                sse(
                    json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"r",
                    "function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}}]}}]}),
                )
            } else {
                sse(json!({"choices":[{"delta":{"content":"done"}}]}))
            };
            ([("content-type", "text/event-stream")], reply)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn start(local: Arc<dyn LocalModels>, dir: &std::path::Path) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
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
            reasoning: None,
        }),
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
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        max_steps: 6,
        background: None,
        auto_compact: false,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".journal.jsonl"),
    })
    .unwrap()
}

async fn turn_ends(events: &mut broadcast::Receiver<AgentEvent>) -> bool {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.agent_id != "root" {
                continue;
            }
            match event.event {
                SessionEvent::TurnCompleted { .. } => return true,
                SessionEvent::TurnFailed { .. } => return false,
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends")
}

fn setup() -> (Arc<Calls>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
    (Arc::new(Calls::default()), dir)
}

#[tokio::test]
async fn a_stopped_session_releases_its_slot_and_resuming_restores_it_first() {
    let (calls, dir) = setup();
    let local = Arc::new(Runtime {
        base_url: model_server().await,
        calls: Arc::clone(&calls),
    });
    let session = start(local, dir.path());
    let mut events = session.subscribe();
    session.send("read it".into()).unwrap();
    // Stopped while the first call is answering: it is cancelled, and the root holds at its
    // safe point with its slot released.
    tokio::time::sleep(Duration::from_millis(200)).await;
    session.stop();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !calls
            .log
            .lock()
            .unwrap()
            .contains(&"suspend s/root".to_string())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the stopped root releases its slot");
    session.resume_work();
    assert!(turn_ends(&mut events).await);
    let log = calls.log.lock().unwrap().clone();
    let suspended = log.iter().position(|c| c == "suspend s/root").unwrap();
    // Resumed: the context is restored by a near-empty acquire before the next real call.
    let restore = log
        .iter()
        .position(|c| c == "acquire s/root 0")
        .unwrap_or_else(|| panic!("{log:?}"));
    assert!(suspended < restore, "{log:?}");
    assert!(
        log[restore + 1..]
            .iter()
            .any(|c| c.starts_with("acquire s/root") && c != "acquire s/root 0"),
        "{log:?}"
    );
}

#[tokio::test]
async fn a_steered_root_has_its_saved_context_restored_before_its_next_call() {
    let (calls, dir) = setup();
    let local = Arc::new(Runtime {
        base_url: model_server().await,
        calls: Arc::clone(&calls),
    });
    let session = start(local, dir.path());
    let mut events = session.subscribe();
    session.send("read it".into()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The runtime saved the root's context meanwhile (its subagents took the pool); the user
    // steers it, and it picks the message up at its next safe point.
    calls.saved.lock().unwrap().insert("s/root".into());
    session
        .send_message("also check b".into(), SendMode::Steer, None)
        .unwrap();
    assert!(turn_ends(&mut events).await);
    let log = calls.log.lock().unwrap().clone();
    let restore = log
        .iter()
        .position(|c| c == "acquire s/root 0")
        .unwrap_or_else(|| panic!("{log:?}"));
    assert!(
        log[restore + 1..]
            .iter()
            .any(|c| c.starts_with("acquire s/root")),
        "the next call follows the restore: {log:?}"
    );
}
