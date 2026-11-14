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
use imperium_tools::permissions::{Permissions, SharedRules};
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

/// What a test changes about its session.
#[derive(Default)]
struct Setup {
    workspace: Option<Workspace>,
    pre_approved: Vec<String>,
    context: Option<Arc<imperium_workflow::ContextIndex>>,
    required_context: bool,
    resume: bool,
}

/// A session whose model makes `calls` in turn, then says it is done.
async fn session(
    dir: &Path,
    tools: Vec<Arc<dyn ToolProvider>>,
    calls: Vec<(&str, Value)>,
) -> Session {
    session_with(dir, tools, calls, Setup::default()).await
}

async fn session_with(
    dir: &Path,
    tools: Vec<Arc<dyn ToolProvider>>,
    calls: Vec<(&str, Value)>,
    setup: Setup,
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
        workspace: setup
            .workspace
            .unwrap_or_else(|| Workspace::new(dir).unwrap()),
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
        context: setup.context,
        enforce: if setup.required_context {
            imperium_config::Enforce::RequiredBeforeAction
        } else {
            imperium_config::Enforce::Warn
        },
        pre_approved: setup.pre_approved,
        launcher: None,
        resume: setup.resume,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        tools,
        max_steps: 8,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        harness: Default::default(),
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
    /// The tool each approval named, and the pattern of the rule "always allow" would add.
    requests: Vec<(String, Option<String>)>,
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
                tool,
                suggested_rule,
            } => {
                turn.asked.push(summary);
                turn.requests
                    .push((tool, suggested_rule.map(|rule| rule.pattern)));
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

/// A folder for a stand-in dev server: the port it serves and the command that starts it.
fn stand_in(dir: &Path) -> (u16, String) {
    let port = free_port();
    std::fs::write(dir.join("imperium-preview-port"), port.to_string()).unwrap();
    let command = format!(
        "\"{}\" --exact stand_in_dev_server --nocapture",
        std::env::current_exe().unwrap().display()
    );
    (port, command)
}

fn preview_tools(workspace: &Workspace) -> Vec<Arc<dyn ToolProvider>> {
    vec![Arc::new(PreviewTools::new(workspace.clone()))]
}

fn permissions(
    mode: imperium_api::PermissionMode,
    rules: Vec<imperium_api::PermissionRule>,
) -> Permissions {
    Permissions::new(SharedRules::new(rules), SharedRules::default()).with_mode(mode)
}

#[tokio::test]
async fn a_preview_command_is_refused_until_the_required_context_is_loaded() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "Project rules.\n").unwrap();
    let (port, command) = stand_in(dir.path());
    let workspace = Workspace::new(dir.path()).unwrap();
    let session = session_with(
        dir.path(),
        preview_tools(&workspace),
        vec![("preview_open", json!({"command": command, "port": port}))],
        Setup {
            workspace: Some(workspace),
            context: Some(Arc::new(imperium_workflow::ContextIndex::new(
                Vec::new(),
                dir.path(),
            ))),
            required_context: true,
            ..Setup::default()
        },
    )
    .await;
    let turn = run(&session, PermissionDecision::Allow).await;
    assert!(
        turn.results[0]
            .1
            .contains("load the required context first"),
        "{turn:?}"
    );
    assert!(
        turn.asked.is_empty() && turn.previews.is_empty(),
        "{turn:?}"
    );
    assert!(!listening(port).await);
}

#[tokio::test]
async fn preview_commands_follow_run_command_rules_and_suggestions() {
    use imperium_api::{PermissionRule, RuleAction};
    let dir = tempfile::tempdir().unwrap();
    let (port, command) = stand_in(dir.path());
    // Full mode runs commands without asking, but an ask rule on run_command still asks.
    let exe = std::env::current_exe().unwrap().display().to_string();
    let rule = PermissionRule {
        tool: "run_command".into(),
        pattern: format!("\"{exe}\""),
        action: RuleAction::Ask,
    };
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions(imperium_api::PermissionMode::Full, vec![rule]));
    let session = session_with(
        dir.path(),
        preview_tools(&workspace),
        vec![("preview_open", json!({"command": command, "port": port}))],
        Setup {
            workspace: Some(workspace),
            ..Setup::default()
        },
    )
    .await;
    let turn = run(&session, PermissionDecision::Deny).await;
    // The approval names run_command, so "always allow" cannot add a blanket preview_open rule.
    assert_eq!(turn.requests.len(), 1, "{turn:?}");
    assert_eq!(turn.requests[0].0, "run_command");
    assert!(turn.requests[0].1.is_some(), "{turn:?}");
    assert!(!listening(port).await);

    // A deny rule refuses it outright.
    let deny = PermissionRule {
        tool: "run_command".into(),
        pattern: String::new(),
        action: RuleAction::Deny,
    };
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions(imperium_api::PermissionMode::Full, vec![deny]));
    let session = session_with(
        dir.path(),
        preview_tools(&workspace),
        vec![("preview_open", json!({"command": "echo hi", "port": port}))],
        Setup {
            workspace: Some(workspace),
            ..Setup::default()
        },
    )
    .await;
    let turn = run(&session, PermissionDecision::Allow).await;
    assert!(turn.requests.is_empty() && !turn.results[0].0, "{turn:?}");
}

#[tokio::test]
async fn preview_commands_meet_the_safety_floor_and_permission_modes() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    // An unattended session was started with run_command pre-approved: an ordinary command
    // runs without asking (it ends before listening here), a destructive one still asks.
    let workspace = Workspace::new(dir.path()).unwrap();
    let session = session_with(
        dir.path(),
        preview_tools(&workspace),
        vec![
            ("preview_open", json!({"command": "echo hi", "port": port})),
            (
                "preview_open",
                json!({"command": "rm -rf ./nothing-here", "port": port}),
            ),
        ],
        Setup {
            workspace: Some(workspace),
            pre_approved: vec!["run_command".into()],
            ..Setup::default()
        },
    )
    .await;
    let turn = run(&session, PermissionDecision::Deny).await;
    assert_eq!(turn.results.len(), 2, "{turn:?}");
    assert!(turn.results[0].1.contains("before listening"), "{turn:?}");
    assert_eq!(turn.requests.len(), 1, "{turn:?}");
    assert!(turn.asked[0].contains("rm -rf"), "{turn:?}");
    assert!(turn.results[1].1.contains("denied"), "{turn:?}");

    // Plan mode only reads: a preview command is refused without asking.
    let (port, command) = stand_in(dir.path());
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions(imperium_api::PermissionMode::Plan, Vec::new()));
    let session = session_with(
        dir.path(),
        preview_tools(&workspace),
        vec![("preview_open", json!({"command": command, "port": port}))],
        Setup {
            workspace: Some(workspace),
            ..Setup::default()
        },
    )
    .await;
    let turn = run(&session, PermissionDecision::Allow).await;
    assert!(turn.requests.is_empty() && !turn.results[0].0, "{turn:?}");
    assert!(
        turn.results[0].1.to_lowercase().contains("plan"),
        "{turn:?}"
    );
    assert!(!listening(port).await);
}

#[tokio::test]
async fn a_resumed_session_shows_the_agent_driving_and_no_preview() {
    let dir = tempfile::tempdir().unwrap();
    let first = session(dir.path(), Vec::new(), vec![]).await;
    first.notify(SessionEvent::BrowserControl { user: true });
    first.notify(SessionEvent::PreviewChanged {
        url: Some("http://localhost:5173/".into()),
        command: Some("npm run dev".into()),
    });
    drop(first);
    let resumed = session_with(
        dir.path(),
        Vec::new(),
        vec![],
        Setup {
            resume: true,
            ..Setup::default()
        },
    )
    .await;
    let (history, _) = resumed.subscribe_with_history();
    let mut driver = false;
    let mut preview = None;
    for event in history {
        match event.event {
            SessionEvent::BrowserControl { user } => driver = user,
            SessionEvent::PreviewChanged { url, .. } => preview = url,
            _ => {}
        }
    }
    assert!(!driver, "the user's takeover does not survive a restart");
    assert_eq!(preview, None, "the preview's server did not survive it");
}
