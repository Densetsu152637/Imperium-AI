//! Roadmap item 9: an agent opens a live preview (a command that serves a port, or a loopback
//! URL), non-loopback URLs are refused, the server stops with the session, and the agent's
//! browser tools are refused while the user has taken the browser over.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig, ToolProvider};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use imperium_web::{BrowserSetup, PreviewTools, USER_DRIVING, Web, WebTools};
use serde_json::{Value, json};

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn call(name: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
    )
}

/// A session whose model makes `calls` in turn, then says it is done.
async fn session(
    dir: &Path,
    tools: Vec<Arc<dyn ToolProvider>>,
    calls: Vec<(&str, Value)>,
) -> Session {
    // An empty name makes the model answer in text, which ends the turn.
    let calls: Vec<String> = calls
        .into_iter()
        .map(|(name, args)| {
            if name.is_empty() {
                sse(json!({"choices":[{"delta":{"content":"Done."}}]}))
            } else {
                call(name, args)
            }
        })
        .collect();
    let step = Arc::new(AtomicUsize::new(0));
    let model = serve(Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let (step, calls) = (Arc::clone(&step), calls.clone());
            async move {
                let body = calls
                    .get(step.fetch_add(1, Ordering::SeqCst))
                    .cloned()
                    .unwrap_or_else(|| sse(json!({"choices":[{"delta":{"content":"Done."}}]})));
                ([("content-type", "text/event-stream")], body)
            }
        }),
    ))
    .await;
    Session::spawn(SessionConfig {
        id: "preview".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: format!("{model}/v1"),
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
        tools,
        max_steps: 8,
        background: None,
        checkpoints: None,
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join("journal.jsonl"),
    })
    .unwrap()
}

/// What a turn produced: approvals asked (answered with `decision`), tool results, and the
/// preview and browser-control events.
#[derive(Default, Debug)]
struct Turn {
    asked: Vec<String>,
    results: Vec<(bool, String)>,
    previews: Vec<Option<String>>,
}

async fn run(session: &Session, decision: PermissionDecision) -> Turn {
    let mut events = session.subscribe();
    session.send("Show me the app".into()).unwrap();
    let mut turn = Turn::default();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .expect("an event within 60 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::PermissionRequested {
                request_id,
                summary,
                ..
            } => {
                turn.asked.push(summary);
                session.respond(request_id, decision).unwrap();
            }
            SessionEvent::ToolFinished { ok, output, .. } => turn.results.push((ok, output)),
            SessionEvent::PreviewChanged { url, .. } => turn.previews.push(url),
            SessionEvent::TurnCompleted { .. } => return turn,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn listening(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

/// Not a test of its own: the test below runs this binary again as a stand-in dev server, in a
/// folder that names the port. Elsewhere it does nothing.
#[tokio::test]
async fn stand_in_dev_server() {
    let Ok(port) = std::fs::read_to_string("imperium-preview-port") else {
        return;
    };
    let listener =
        tokio::net::TcpListener::bind(("127.0.0.1", port.trim().parse::<u16>().unwrap()))
            .await
            .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let _ = listener.accept().await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_preview_command_starts_a_server_that_stops_with_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    std::fs::write(dir.path().join("imperium-preview-port"), port.to_string()).unwrap();
    let command = format!(
        "\"{}\" --exact stand_in_dev_server --nocapture",
        std::env::current_exe().unwrap().display()
    );
    let workspace = Workspace::new(dir.path()).unwrap();
    let preview: Arc<dyn ToolProvider> = Arc::new(PreviewTools::new(workspace));
    let session = session(
        dir.path(),
        vec![preview],
        vec![
            ("preview_open", json!({"url": "https://example.com/"})),
            ("preview_open", json!({"command": command, "port": port})),
        ],
    )
    .await;
    let turn = run(&session, PermissionDecision::Allow).await;
    // A remote page is refused without asking; the command asks, then serves the port.
    assert_eq!(turn.results.len(), 2, "{turn:?}");
    assert!(
        !turn.results[0].0 && turn.results[0].1.contains("loopback"),
        "{turn:?}"
    );
    assert!(turn.results[1].0, "{turn:?}");
    assert_eq!(turn.asked.len(), 1, "{turn:?}");
    assert!(turn.asked[0].contains("and preview it"), "{turn:?}");
    assert_eq!(turn.previews, [Some(format!("http://localhost:{port}/"))]);
    assert!(listening(port).await);

    // Ending the session drops its tools, which stops the server.
    drop(session);
    let mut stopped = false;
    for _ in 0..80 {
        if !listening(port).await {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(stopped, "the preview server outlived its session");
}

#[tokio::test]
async fn a_denied_preview_command_is_not_started() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    std::fs::write(dir.path().join("imperium-preview-port"), port.to_string()).unwrap();
    let workspace = Workspace::new(dir.path()).unwrap();
    let preview: Arc<dyn ToolProvider> = Arc::new(PreviewTools::new(workspace));
    let session = session(
        dir.path(),
        vec![preview],
        vec![(
            "preview_open",
            json!({"command": "echo never", "port": port}),
        )],
    )
    .await;
    let turn = run(&session, PermissionDecision::Deny).await;
    assert_eq!(turn.asked.len(), 1);
    assert!(!turn.results[0].0);
    assert!(turn.previews.is_empty());
    assert!(!listening(port).await);
}

#[tokio::test]
async fn browser_tools_are_refused_while_the_user_drives_and_return_when_given_back() {
    let dir = tempfile::tempdir().unwrap();
    let setup = BrowserSetup {
        program: "imperium-no-such-browser".into(),
        profile_dir: dir.path().join("profile"),
        headless: true,
        instance: Arc::default(),
        takeover: Arc::default(),
    };
    let tools: Arc<dyn ToolProvider> = Arc::new(WebTools {
        web: Web::new(None),
        allow_domains: vec!["127.0.0.1".into()],
        browser: Some(setup.clone()),
    });
    let session = session(
        dir.path(),
        vec![tools],
        vec![
            ("browser_open", json!({"url": "http://127.0.0.1:1/"})),
            ("browser_read", json!({})),
            ("", json!(null)),
            ("browser_open", json!({"url": "http://127.0.0.1:1/"})),
        ],
    )
    .await;

    // The user drives: the agent's calls fail with the reason, and nothing is asked.
    setup.takeover.set_user(true);
    let turn = run(&session, PermissionDecision::Allow).await;
    assert!(turn.asked.is_empty(), "{turn:?}");
    assert!(
        turn.results
            .iter()
            .all(|(ok, output)| !ok && output.contains(USER_DRIVING)),
        "{turn:?}"
    );

    // Given back, the agent's call goes through to starting the browser (which cannot start
    // here), no longer refused with the takeover message.
    assert!(setup.give_back());
    let turn = run(&session, PermissionDecision::Allow).await;
    let last = turn.results.last().expect("a result");
    assert!(!last.0 && !last.1.contains(USER_DRIVING), "{turn:?}");
}
