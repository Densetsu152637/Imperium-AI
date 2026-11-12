//! Lazy skills and plugins: agents start with an index of names and descriptions; a skill's
//! instructions enter the conversation on `load_skill` (once), a plugin's MCP server starts on
//! `activate_plugin` (not before), a resumed session restores both, and eager settings load
//! them up front.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_extensions::Sources;
use imperium_models::Endpoint;
use imperium_protocol::session::{ExtensionKind, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

const LOOKUP: &str = "mcp__words_dict__lookup";

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// An MCP server offering `lookup`; counts how often it is initialised (started).
async fn mcp_server(starts: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/mcp",
        post(move |axum::Json(request): axum::Json<Value>| {
            let starts = Arc::clone(&starts);
            async move {
                let Some(id) = request.get("id").cloned() else {
                    return axum::Json(json!({}));
                };
                let result = match request["method"].as_str() {
                    Some("initialize") => {
                        starts.fetch_add(1, Ordering::SeqCst);
                        json!({"protocolVersion": "2025-06-18", "capabilities": {}})
                    }
                    Some("tools/list") => json!({"tools": [
                        {"name": "lookup", "description": "Look up a word", "inputSchema": {"type": "object"}}]}),
                    _ => json!({"content": [{"type": "text", "text": "lookup ok"}]}),
                };
                axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            }
        }),
    );
    format!("{}/mcp", serve(app).await)
}

/// What one model request carried.
#[derive(Debug, Clone)]
struct Seen {
    system: String,
    tools: Vec<String>,
    /// MCP server starts so far when the request arrived.
    starts: usize,
}

/// A model that answers each request with the next of `script` (a tool call `(name, args)`,
/// or `None` for a final answer), recording what each request carried.
async fn model(
    script: Vec<Option<(&'static str, Value)>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    starts: Arc<AtomicUsize>,
) -> String {
    let step = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let step = Arc::clone(&step);
            let seen = Arc::clone(&seen);
            let starts = Arc::clone(&starts);
            let script = script.clone();
            async move {
                seen.lock().unwrap().push(Seen {
                    system: body["messages"][0]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    tools: body["tools"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                        .collect(),
                    starts: starts.load(Ordering::SeqCst),
                });
                let index = step.fetch_add(1, Ordering::SeqCst);
                let chunk = match script.get(index).cloned().flatten() {
                    Some((name, arguments)) => json!({"choices":[{"delta":{"tool_calls":[{
                        "index": 0, "id": format!("call{index}"),
                        "function": {"name": name, "arguments": arguments.to_string()}}]}}]}),
                    None => json!({"choices":[{"delta":{"content":"done"}}]}),
                };
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                )
            }
        }),
    );
    format!("{}/v1", serve(app).await)
}

/// A user skill `greeting` and a plugin `words` whose server `dict` is at `mcp_url`.
fn write_extensions(config: &Path, mcp_url: &str) {
    let skill = config.join("skills/greeting");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: greeting\ndescription: Greet people politely\n---\nAlways say good morning.\n",
    )
    .unwrap();
    let plugin = config.join("plugins/words");
    std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name": "words", "description": "Dictionary tools"}"#,
    )
    .unwrap();
    std::fs::write(
        plugin.join(".mcp.json"),
        json!({"mcpServers": {"dict": {"type": "http", "url": mcp_url}}}).to_string(),
    )
    .unwrap();
}

fn start(
    config: &Path,
    project: &Path,
    model_url: String,
    resume: bool,
    lazy: bool,
    eager: &[&str],
) -> Session {
    let extensions = imperium_extensions::load(&Sources {
        config_dir: config.to_path_buf(),
        project_dir: project.to_path_buf(),
        plugin_dirs: Vec::new(),
        disabled_plugins: Vec::new(),
        hooks: Vec::new(),
        server_timeout: Duration::from_secs(5),
        lazy,
        eager: eager.iter().map(|name| name.to_string()).collect(),
    });
    assert!(extensions.notices.is_empty(), "{:?}", extensions.notices);
    Session::spawn(SessionConfig {
        id: "lazy".into(),
        workspace: Workspace::new(project).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: model_url,
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
        tools: Vec::new(),
        notices: Vec::new(),
        extensions,
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: vec![LOOKUP.into()],
        launcher: None,
        resume,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 10,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: project.join(".journal.jsonl"),
    })
    .unwrap()
}

/// Sends `text` and collects the turn's events.
async fn turn(session: &Session, text: &str) -> Vec<SessionEvent> {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match &event {
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            SessionEvent::PermissionRequested { .. } => panic!("nothing should ask"),
            _ => seen.push(event),
        }
    }
    seen
}

fn outputs(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolFinished { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect()
}

fn loaded(events: &[SessionEvent]) -> Vec<(ExtensionKind, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ExtensionLoaded { kind, name, .. } => Some((*kind, name.clone())),
            _ => None,
        })
        .collect()
}

fn category_tokens(events: &[SessionEvent], key: &str) -> Vec<i64> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ContextBreakdown { categories, .. } => Some(
                categories
                    .iter()
                    .find(|category| category.key == key)
                    .map_or(0, |category| category.tokens),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn skills_and_plugins_load_on_first_use_and_after_a_restart() {
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    write_extensions(config.path(), &mcp_server(Arc::clone(&starts)).await);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let script = vec![
        Some(("load_skill", json!({"name": "greeting"}))),
        Some(("load_skill", json!({"name": "greeting"}))),
        Some(("activate_plugin", json!({"name": "words"}))),
        Some((LOOKUP, json!({"word": "x"}))),
        None,
    ];
    let url = model(script, Arc::clone(&seen), Arc::clone(&starts)).await;
    let session = start(config.path(), project.path(), url, false, true, &[]);
    let events = turn(&session, "greet and look up").await;

    let requests = seen.lock().unwrap().clone();
    // Up front: the index, without the skill's instructions, and no plugin server or tools.
    let first = &requests[0];
    assert!(
        first.system.contains("- greeting: Greet people politely"),
        "{}",
        first.system
    );
    assert!(
        first
            .system
            .contains("- words: Dictionary tools (servers: words_dict)"),
        "{}",
        first.system
    );
    assert!(!first.system.contains("Always say good morning."));
    assert!(first.tools.iter().any(|tool| tool == "load_skill"));
    assert!(first.tools.iter().any(|tool| tool == "activate_plugin"));
    assert!(!first.tools.iter().any(|tool| tool == LOOKUP));
    // The plugin's server did not start until activate_plugin.
    assert_eq!((requests[1].starts, requests[2].starts), (0, 0));
    assert_eq!(requests[3].starts, 1);
    assert!(requests[3].tools.iter().any(|tool| tool == LOOKUP));
    assert!(
        !requests[3]
            .tools
            .iter()
            .any(|tool| tool == "activate_plugin")
    );

    let said = outputs(&events);
    assert!(said[0].contains("Always say good morning."), "{said:?}");
    assert!(said[1].contains("already loaded"), "{said:?}");
    assert!(said[2].contains("Plugin words is active"), "{said:?}");
    assert!(said[2].contains(LOOKUP), "{said:?}");
    assert_eq!(said[3], "lookup ok");
    assert_eq!(
        loaded(&events),
        [
            (ExtensionKind::Skill, "greeting".to_string()),
            (ExtensionKind::Plugin, "words".to_string())
        ]
    );
    // The breakdown shows the index from the first call, the loaded skill once loaded, and the
    // plugin's tools once activated.
    let index = category_tokens(&events, "system_skills");
    assert!(index.iter().all(|tokens| *tokens > 0), "{index:?}");
    let skills = category_tokens(&events, "skills_loaded");
    assert_eq!(skills[0], 0, "{skills:?}");
    assert!(skills[1] > 0, "{skills:?}");
    let plugin_tools = category_tokens(&events, "tools_plugins");
    assert_eq!(plugin_tools[2], 0, "{plugin_tools:?}");
    assert!(plugin_tools[3] > 0, "{plugin_tools:?}");
    drop(session);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // After a restart the activated plugin starts with the session, and the skill the root
    // already has is not loaded again.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let script = vec![Some(("load_skill", json!({"name": "greeting"}))), None];
    let url = model(script, Arc::clone(&seen), Arc::clone(&starts)).await;
    let resumed = start(config.path(), project.path(), url, true, true, &[]);
    let events = turn(&resumed, "again").await;
    let requests = seen.lock().unwrap().clone();
    assert_eq!(
        requests[0].starts, 2,
        "the plugin restarts with the session"
    );
    assert!(requests[0].tools.iter().any(|tool| tool == LOOKUP));
    let said = outputs(&events);
    assert!(said[0].contains("already loaded"), "{said:?}");
    assert!(loaded(&events).is_empty());
}

#[tokio::test]
async fn eager_extensions_load_up_front() {
    for (lazy, eager) in [(false, &[][..]), (true, &["greeting", "words"][..])] {
        let config = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        write_extensions(config.path(), &mcp_server(Arc::clone(&starts)).await);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let url = model(vec![None], Arc::clone(&seen), Arc::clone(&starts)).await;
        let session = start(config.path(), project.path(), url, false, lazy, eager);
        let events = turn(&session, "hello").await;
        let first = seen.lock().unwrap()[0].clone();
        assert!(
            first.system.contains("Always say good morning."),
            "{}",
            first.system
        );
        assert!(!first.system.contains("- greeting:"), "{}", first.system);
        assert_eq!(first.starts, 1, "the eager plugin started with the session");
        assert!(first.tools.iter().any(|tool| tool == LOOKUP));
        assert!(!first.tools.iter().any(|tool| tool == "activate_plugin"));
        assert!(category_tokens(&events, "skills_loaded")[0] > 0);
        assert!(category_tokens(&events, "tools_plugins")[0] > 0);
    }
}
