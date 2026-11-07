//! Skills, slash commands, and hooks in a running session.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{ResolvedModel, Session, SessionConfig};
use imperium_extensions::{ConfiguredHook, Sources};
use imperium_models::Endpoint;
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;
use serde_json::{Value, json};

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Loads the skill, tries a command (blocked by a hook), then answers. Records the system
/// prompt it was given.
async fn model(system: Arc<Mutex<String>>) -> String {
    let step = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let step = Arc::clone(&step);
            let system = Arc::clone(&system);
            async move {
                *system.lock().unwrap() = body["messages"][0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let call = |name: &str, arguments: Value| {
                    sse(
                        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
                        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
                    )
                };
                let body = match step.fetch_add(1, Ordering::SeqCst) {
                    0 => call("load_skill", json!({"name": "greeting"})),
                    1 => call("run_command", json!({"command": "echo hi"})),
                    _ => sse(json!({"choices":[{"delta":{"content":"done"}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
async fn skills_commands_and_hooks_shape_a_turn() {
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let skill = config.path().join("skills/greeting");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: greeting\ndescription: Greet people politely\n---\nAlways say good morning.\n",
    )
    .unwrap();
    let commands = project.path().join(".claude/commands");
    std::fs::create_dir_all(&commands).unwrap();
    std::fs::write(commands.join("greet.md"), "Greet $ARGUMENTS warmly.").unwrap();
    let block = if cfg!(windows) {
        "echo no shell today 1>&2 & exit /b 2"
    } else {
        "echo no shell today >&2; exit 2"
    };
    let extensions = imperium_extensions::load(&Sources {
        config_dir: config.path().to_path_buf(),
        project_dir: project.path().to_path_buf(),
        plugin_dirs: Vec::new(),
        hooks: vec![ConfiguredHook {
            event: "PreToolUse".into(),
            matcher: "Bash".into(),
            command: block.into(),
            timeout_s: Some(10),
        }],
        server_timeout: Duration::from_secs(5),
    });
    assert!(extensions.notices.is_empty(), "{:?}", extensions.notices);

    let system = Arc::new(Mutex::new(String::new()));
    let session = Session::spawn(SessionConfig {
        id: "ext".into(),
        workspace: Workspace::new(project.path()).unwrap(),
        model: ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: model(Arc::clone(&system)).await,
                api_key: None,
            },
            model: "model".into(),
            pricing: None,
            local: false,
        },
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        local: None,
        mcp: Vec::new(),
        notices: Vec::new(),
        extensions,
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        instructions: None,
        web: None,
        max_steps: 5,
        journal_path: project.path().join(".journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("/greet Ada".into()).unwrap();
    let mut started = None;
    let mut finished = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::TurnStarted { text } => started = Some(text),
            SessionEvent::PermissionRequested { .. } => panic!("the hook should block first"),
            SessionEvent::ToolFinished { ok, output, .. } => finished.push((ok, output)),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(started.as_deref(), Some("Greet Ada warmly."));
    assert!(
        system
            .lock()
            .unwrap()
            .contains("- greeting: Greet people politely")
    );
    assert!(finished[0].0 && finished[0].1.contains("Always say good morning."));
    assert!(!finished[1].0);
    assert!(
        finished[1].1.contains("no shell today"),
        "{}",
        finished[1].1
    );
}
