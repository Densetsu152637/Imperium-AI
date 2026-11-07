//! Phase 8 exit criterion (first half): an agent researches with web_search and web_fetch and
//! cites its sources; fetching an unlisted domain asks first.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use imperium_core::{ResolvedModel, Session, SessionConfig, WebTools};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use imperium_web::{SearchProvider, Web};
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
        model: ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: format!("{model}/v1"),
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
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        instructions: None,
        media: None,
        web: Some(WebTools {
            web: Web::new(Some(SearchProvider::Searxng { url: searxng })),
            allow_domains: vec!["127.0.0.1".into()],
            browser: None,
        }),
        max_steps: 8,
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
        model: ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: format!("{model}/v1"),
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
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        instructions: None,
        media: None,
        web: Some(WebTools {
            web: Web::new(None),
            allow_domains: vec!["127.0.0.1".into()],
            browser: Some(imperium_core::BrowserSetup {
                program,
                profile_dir: dir.path().join("profile"),
                headless: true,
                instance: Arc::default(),
            }),
        }),
        max_steps: 8,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("Go to the next page".into()).unwrap();
    let mut views = Vec::new();
    let mut asked = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.recv())
            .await
            .expect("an event within 30 s")
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
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    // Opening an allowed domain did not ask; clicking did.
    assert_eq!(asked, ["click #next in the browser"]);
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].0, "Docs");
    assert_eq!(views[1].0, "Page two");
    assert!(views.iter().all(|(_, size)| *size > 100));
}
