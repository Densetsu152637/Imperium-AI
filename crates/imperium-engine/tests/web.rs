//! Phase 8 exit criterion (first half): an agent researches with web_search and web_fetch and
//! cites its sources; fetching an unlisted domain asks first.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use imperium_web::{BrowserSetup, Downloads, Searxng, Web, WebTools};
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

#[tokio::test]
async fn agents_research_and_cite_sources() {
    let site = serve(
        Router::new()
            .route(
                "/tokio",
                get(|| async {
                    (
                        [("content-type", "text/html")],
                        "<html><head><title>Tokio tasks</title></head><body><p>A task is a lightweight green thread.</p></body></html>",
                    )
                }),
            )
            .route(
                "/search",
                get(|| async { axum::Json(json!({"results": [] })) }),
            ),
    )
    .await;
    let page = format!("{site}/tokio");
    let search_page = page.clone();
    let searxng = serve(Router::new().route(
        "/search",
        get(move || {
            let page = search_page.clone();
            async move {
                axum::Json(json!({"results": [
                    {"title": "Tokio tasks", "url": page, "content": "What a task is"}
                ]}))
            }
        }),
    ))
    .await;

    let system = Arc::new(Mutex::new(String::new()));
    let last_tool = Arc::new(Mutex::new(String::new()));
    let step = Arc::new(AtomicUsize::new(0));
    let model = {
        let (system, last_tool, step, page) = (
            Arc::clone(&system),
            Arc::clone(&last_tool),
            Arc::clone(&step),
            page.clone(),
        );
        serve(Router::new().route(
            "/v1/chat/completions",
            post(move |axum::Json(body): axum::Json<Value>| {
                let (system, last_tool, step, page) =
                    (Arc::clone(&system), Arc::clone(&last_tool), Arc::clone(&step), page.clone());
                async move {
                    let messages = body["messages"].as_array().cloned().unwrap_or_default();
                    *system.lock().unwrap() = messages[0]["content"].as_str().unwrap_or_default().into();
                    if let Some(tool) = messages.iter().rev().find(|m| m["role"] == "tool") {
                        *last_tool.lock().unwrap() = tool["content"].as_str().unwrap_or_default().into();
                    }
                    let body = match step.fetch_add(1, Ordering::SeqCst) {
                        0 => call("web_search", json!({"query": "what is a tokio task"})),
                        1 => call("web_fetch", json!({"url": page})),
                        2 => call("web_fetch", json!({"url": "https://unlisted.example/page"})),
                        _ => sse(json!({"choices":[{"delta":{"content":
                            format!("A Tokio task is a lightweight green thread [1].\n\n[1] {page}")}}]})),
                    };
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        ))
        .await
    };

    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "web".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
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
        tools: vec![Arc::new(WebTools {
            web: Web::new(Some(Arc::new(Searxng { url: searxng }))),
            allow_domains: vec!["127.0.0.1".into()],
            browser: None,
            downloads: None,
        })],
        max_steps: 8,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        harness: Default::default(),
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session
        .send("What is a Tokio task? Cite sources.".into())
        .unwrap();
    let mut asked = Vec::new();
    let mut finished = Vec::new();
    let mut reply = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::PermissionRequested {
                request_id,
                summary,
                ..
            } => {
                asked.push(summary);
                session
                    .respond(request_id, PermissionDecision::Deny)
                    .unwrap();
            }
            SessionEvent::ToolFinished { ok, output, .. } => finished.push((ok, output)),
            SessionEvent::TextDelta { text } => reply.push_str(&text),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert!(system.lock().unwrap().contains("cite it"));
    // Search and the allowed domain ran without asking; the unlisted domain asked and was denied.
    assert_eq!(asked, ["fetch https://unlisted.example/page"]);
    assert!(
        finished[0].0 && finished[0].1.starts_with("[1] Tokio tasks"),
        "{:?}",
        finished[0]
    );
    assert!(
        finished[1].0 && finished[1].1.contains("lightweight green thread"),
        "{:?}",
        finished[1]
    );
    assert!(!finished[2].0);
    assert!(reply.contains(&format!("[1] {page}")), "{reply}");
}

/// Phase 8 exit criterion (second half): browser actions are mirrored to clients. Runs where
/// Chrome, Chromium, or Edge is installed.
#[tokio::test]
async fn browser_actions_are_mirrored_as_screenshots() {
    let Some(program) = imperium_web::browser::find_browser() else {
        eprintln!("no browser found; skipping");
        return;
    };
    let site = serve(Router::new().route(
        "/",
        get(|| async {
            axum::response::Html(
                "<html><head><title>Docs</title></head><body><a id='next' href='/two'>Next</a></body></html>",
            )
        })
        .post(|| async { "" }),
    )
    .route(
        "/two",
        get(|| async { axum::response::Html("<html><head><title>Page two</title></head><body>Second page</body></html>") }),
    ))
    .await;
    let step = Arc::new(AtomicUsize::new(0));
    let start = format!("{site}/");
    let model = {
        let (step, start) = (Arc::clone(&step), start.clone());
        serve(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let (step, start) = (Arc::clone(&step), start.clone());
                async move {
                    let body = match step.fetch_add(1, Ordering::SeqCst) {
                        0 => call("browser_open", json!({"url": start})),
                        1 => call("browser_click", json!({"selector": "#next"})),
                        _ => sse(json!({"choices":[{"delta":{"content":"Reached page two."}}]})),
                    };
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        ))
        .await
    };
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "browser".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
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
        tools: vec![Arc::new(WebTools {
            web: Web::new(None),
            allow_domains: vec!["127.0.0.1".into()],
            browser: Some(BrowserSetup {
                program,
                profile_dir: dir.path().join("profile"),
                headless: true,
                instance: Arc::default(),
                takeover: Arc::default(),
            }),
            downloads: None,
        })],
        max_steps: 8,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        harness: Default::default(),
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("Go to the next page".into()).unwrap();
    let mut views = Vec::new();
    let mut asked = Vec::new();
    let mut failed = Vec::new();
    loop {
        // Generous: the first browser call includes a cold browser start.
        let event = tokio::time::timeout(Duration::from_secs(90), events.recv())
            .await
            .expect("an event within 90 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::PermissionRequested {
                request_id,
                summary,
                ..
            } => {
                asked.push(summary);
                session
                    .respond(request_id, PermissionDecision::Allow)
                    .unwrap();
            }
            SessionEvent::BrowserView {
                title, screenshot, ..
            } => views.push((title, screenshot.len())),
            SessionEvent::ToolFinished {
                ok: false, output, ..
            } => failed.push(output),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    // A browser that failed to start shows here rather than as a bare count mismatch.
    assert!(failed.is_empty(), "browser tools failed: {failed:?}");
    // Opening an allowed domain did not ask; clicking did.
    assert_eq!(asked, ["click #next in the browser"]);
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].0, "Docs");
    assert_eq!(views[1].0, "Page two");
    assert!(views.iter().all(|(_, size)| *size > 100));
}

/// Roadmap 32, stage 2: an agent works with several tabs and saves a file a page offered, only
/// after the user approves, into the session's downloads folder. Runs where Chrome, Chromium, or
/// Edge is installed.
#[tokio::test]
async fn agents_use_tabs_and_save_downloads_after_approval() {
    let Some(program) = imperium_web::browser::find_browser() else {
        eprintln!("no browser found; skipping");
        return;
    };
    let site = serve(
        Router::new()
            .route(
                "/",
                get(|| async {
                    axum::response::Html(
                        "<html><head><title>Docs</title></head><body>\
                         <a id='dl' href='/report.csv' download>Get report</a></body></html>",
                    )
                }),
            )
            .route(
                "/two",
                get(|| async {
                    axum::response::Html(
                        "<html><head><title>Page two</title></head><body>Second page</body></html>",
                    )
                }),
            )
            .route(
                "/report.csv",
                get(|| async {
                    (
                        [
                            ("content-type", "text/csv"),
                            ("content-disposition", "attachment; filename=\"report.csv\""),
                        ],
                        "a,b\n1,2\n",
                    )
                }),
            ),
    )
    .await;
    let step = Arc::new(AtomicUsize::new(0));
    let start = format!("{site}/");
    let model = {
        let (step, start) = (Arc::clone(&step), start.clone());
        serve(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let (step, start) = (Arc::clone(&step), start.clone());
                async move {
                    let body = match step.fetch_add(1, Ordering::SeqCst) {
                        0 => call("browser_open", json!({"url": start})),
                        1 => call(
                            "browser_open",
                            json!({"url": format!("{start}two"), "new_tab": true}),
                        ),
                        2 => call("browser_tabs", json!({})),
                        3 => call("browser_switch", json!({"tab": "1"})),
                        4 => call("browser_click", json!({"selector": "#dl"})),
                        5 => call("browser_download", json!({})),
                        _ => sse(json!({"choices":[{"delta":{"content":"Saved the report."}}]})),
                    };
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        ))
        .await
    };
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "tabs".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
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
        tools: vec![Arc::new(WebTools {
            web: Web::new(None),
            allow_domains: vec!["127.0.0.1".into()],
            browser: Some(BrowserSetup {
                program,
                profile_dir: dir.path().join("profile"),
                headless: true,
                instance: Arc::default(),
                takeover: Arc::default(),
            }),
            downloads: Some(Downloads {
                dir: dir.path().join("downloads"),
                max_bytes: 1_000_000,
            }),
        })],
        max_steps: 12,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        harness: Default::default(),
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session
        .send("Open the second page in a tab, then save the report".into())
        .unwrap();
    let mut views = Vec::new();
    let mut outputs = Vec::new();
    let mut asked = Vec::new();
    let mut failed = Vec::new();
    loop {
        // Generous: the first browser call includes a cold browser start.
        let event = tokio::time::timeout(Duration::from_secs(90), events.recv())
            .await
            .expect("an event within 90 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::PermissionRequested {
                request_id,
                summary,
                ..
            } => {
                asked.push(summary);
                session
                    .respond(request_id, PermissionDecision::Allow)
                    .unwrap();
            }
            SessionEvent::BrowserView {
                title,
                screenshot,
                tab,
                tabs,
                ..
            } => views.push((title, screenshot.len(), tab, tabs.len())),
            SessionEvent::ToolFinished {
                ok: true,
                output,
                tool,
                ..
            } => outputs.push((tool.unwrap_or_default(), output)),
            SessionEvent::ToolFinished {
                ok: false, output, ..
            } => failed.push(output),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    // A browser that failed to start shows here rather than as a bare count mismatch.
    assert!(failed.is_empty(), "browser tools failed: {failed:?}");
    // Opening allowed pages, listing, switching and reading did not ask; clicking did, and so
    // did saving a file.
    assert_eq!(
        asked,
        [
            "click #dl in the browser",
            "save the file the page tried to download"
        ]
    );
    let output = |tool: &str| {
        outputs
            .iter()
            .filter(|(name, _)| name == tool)
            .map(|(_, output)| output.clone())
            .collect::<Vec<_>>()
    };
    let tabs = output("browser_tabs");
    assert!(
        tabs[0].contains("tab 1") && tabs[0].contains("Docs"),
        "{tabs:?}"
    );
    assert!(
        tabs[0].contains("* tab 2") && tabs[0].contains("Page two"),
        "{tabs:?}"
    );
    // Each action was mirrored with its tab: the second page opened as tab 2, then tab 1 was
    // shown again.
    assert!(
        views
            .iter()
            .any(|(title, _, tab, count)| title.as_str() == "Page two"
                && tab.as_deref() == Some("2")
                && *count == 2)
    );
    assert!(
        views
            .iter()
            .any(|(title, _, tab, _)| title.as_str() == "Docs" && tab.as_deref() == Some("1"))
    );
    assert!(views.iter().all(|(_, size, _, _)| *size > 100));
    // The click was told the browser refused a download; saving it wrote the file where the
    // session keeps downloads, under a safe name.
    let clicked = output("browser_click");
    assert!(clicked[0].contains("refused it"), "{clicked:?}");
    let saved = output("browser_download");
    assert!(
        saved[0].contains("Saved") && saved[0].contains("report.csv"),
        "{saved:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("downloads").join("report.csv")).unwrap(),
        "a,b\n1,2\n"
    );
}
