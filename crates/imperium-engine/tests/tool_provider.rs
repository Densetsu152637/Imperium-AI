//! A tool provider from outside the agent loop is offered to the model and called through the
//! same hooks and approvals as the built-in tools.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use imperium_api::{
    StartFuture, ToolCheck, ToolContext, ToolFuture, ToolProvider, ToolSpec, parse_arguments,
};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

/// Keeps notes in memory; every note asks first.
#[derive(Debug, Default)]
struct Notes {
    saved: Mutex<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct NoteArgs {
    text: String,
}

impl ToolProvider for Notes {
    fn start(&self) -> StartFuture<'_> {
        Box::pin(async { vec!["notes are ready".to_string()] })
    }

    fn instructions(&self) -> Option<&str> {
        Some("Keep notes with note_add.")
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "note_add".into(),
            description: "Save a note.".into(),
            parameters: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        }]
    }

    fn handles(&self, name: &str) -> bool {
        name == "note_add"
    }

    fn check(&self, _name: &str, arguments: &str) -> Result<ToolCheck, String> {
        let args: NoteArgs = parse_arguments(arguments)?;
        Ok(ToolCheck {
            summary: format!("add a note: {}", args.text),
            ask: true,
            needs_context: false,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a ToolContext<'a>,
        _name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: NoteArgs = parse_arguments(arguments)?;
            self.saved.lock().unwrap().push(args.text);
            Ok("noted".to_string())
        })
    }
}

fn sse(chunk: Value) -> String {
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20}}"#
    )
}

fn note_call(id: &str, text: &str) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":id,
        "function":{"name":"note_add","arguments":json!({"text": text}).to_string()}}]}}]}),
    )
}

#[tokio::test]
async fn a_custom_provider_is_offered_and_its_calls_need_approval() {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let bodies = [
        note_call("c1", "first"),
        note_call("c2", "second"),
        sse(json!({"choices":[{"delta":{"content":"Saved."}}]})),
    ];
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let requests = Arc::clone(&requests);
            move |Json(body): Json<Value>| {
                requests.lock().unwrap().push(body);
                let index = calls.fetch_add(1, Ordering::SeqCst).min(bodies.len() - 1);
                let body = bodies[index].clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });

    let notes = Arc::new(Notes::default());
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "provider".into(),
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
        }),
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        clients: Default::default(),
        local: None,
        tools: vec![Arc::clone(&notes) as Arc<dyn ToolProvider>],
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
        max_steps: 5,
        background: None,
        auto_compact: false,
        keep_agent_state: true,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("note two things".into()).unwrap();
    let mut asked = Vec::new();
    let mut finished = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::PermissionRequested {
                request_id,
                tool,
                summary,
            } => {
                assert_eq!(tool, "note_add");
                // The first note is refused, the second allowed.
                let decision = if asked.is_empty() {
                    PermissionDecision::Deny
                } else {
                    PermissionDecision::Allow
                };
                asked.push(summary);
                session.respond(request_id, decision).unwrap();
            }
            SessionEvent::ToolFinished { ok, output, .. } => finished.push((ok, output)),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("turn failed: {message}"),
            _ => {}
        }
    }
    assert_eq!(asked, ["add a note: first", "add a note: second"]);
    assert_eq!(
        finished,
        [
            (false, "The user denied this action.".to_string()),
            (true, "noted".to_string())
        ]
    );
    assert_eq!(*notes.saved.lock().unwrap(), ["second"]);

    // The provider's tool comes after the agent's own tools and before delegation, and its
    // instructions are in the system prompt.
    let first = requests.lock().unwrap()[0].clone();
    let names: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names[names.len() - 10..],
        [
            "update_plan",
            "note_add",
            "spawn_agent",
            "await_tasks",
            "steer_agent",
            "queue_agent",
            "list_agents",
            "resume_agent",
            "delegate_chunks",
            "ask_user"
        ]
    );
    let system = first["messages"][0]["content"].as_str().unwrap();
    assert!(system.ends_with("Keep notes with note_add."), "{system}");
    // Its start-up notice reached the session before the turn.
    let (history, _) = session.subscribe_with_history();
    assert!(matches!(
        &history[0].event,
        SessionEvent::Notice { message } if message == "notes are ready"
    ));
}
