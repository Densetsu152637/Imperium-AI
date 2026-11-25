//! Reactive tool loading, stage 2a (docs/plans/reactive-tools.md): with `harness.tool_loading`
//! on, an agent is offered a fixed core list with `tool_search` and `call_tool` and reaches every
//! other tool of its universe through them, with the checks of a direct call; with it off (the
//! default) nothing changes. The model is a script, the MCP server and the plugin are fakes.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_api::{AcquireFuture, Availability, LocalLease, LocalModels, LocalRequest};
use imperium_api::{
    PermissionMode, ToolCheck, ToolContext, ToolFuture, ToolProvider, ToolSpec, Usage,
};
use imperium_config::{HarnessMode, HarnessSettings};
use imperium_engine::{
    ChatClient, ChatClients, LaunchFuture, ResolvedModel, Session, SessionConfig, SessionLauncher,
};
use imperium_extensions::{ConfiguredHook, Sources};
use imperium_mcp::{Launch, McpTools, ServerSpec};
use imperium_models::{ChatFuture, ChatRequest, Completion, Endpoint};
use imperium_policy::permissions::{Permissions, SharedRules};
use imperium_policy::{AgentProfile, Allowlist, ProfileRules};
use imperium_protocol::session::{ROOT_AGENT, SessionEvent, ToolLoadVia};
use imperium_tools::Workspace;
use serde_json::{Value, json};

const PLUGIN_LOOKUP: &str = "mcp__words_dict__lookup";
const ALPHA: &str = "mcp__alpha__lookup";
const BETA: &str = "mcp__beta__lookup";

/// The root's tool list while loading is on, in the session `start` builds.
const ROOT_CORE: [&str; 10] = [
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "glob",
    "grep",
    "run_command",
    "activate_plugin",
    "tool_search",
    "call_tool",
];

type Step = Option<(&'static str, Value)>;

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// An MCP server whose one tool, `lookup`, answers `<label> ok`.
async fn mcp_server(label: &'static str) -> String {
    let app = Router::new().route(
        "/mcp",
        post(move |axum::Json(request): axum::Json<Value>| async move {
            let Some(id) = request.get("id").cloned() else {
                return axum::Json(json!({}));
            };
            let result = match request["method"].as_str() {
                Some("initialize") => {
                    json!({"protocolVersion": "2025-06-18", "capabilities": {}})
                }
                Some("tools/list") => json!({"tools": [
                    {"name": "lookup", "description": "Look up a word", "inputSchema": {"type": "object"}}]}),
                _ => json!({"content": [{"type": "text", "text": format!("{label} ok")}]}),
            };
            axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        }),
    );
    format!("{}/mcp", serve(app).await)
}

fn server(name: &str, url: String) -> ServerSpec {
    ServerSpec {
        name: name.into(),
        launch: Launch::Http {
            url,
            headers: Vec::new(),
        },
        allow: vec!["*".into()],
        timeout: Duration::from_secs(5),
        sampling: false,
    }
}

/// A plugin `words` whose server `dict` is at `mcp_url`.
fn write_plugin(config: &Path, mcp_url: &str) {
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

/// A built-in integration (standing in for the web tools): fetches without asking.
#[derive(Debug)]
struct Fetcher;

impl ToolProvider for Fetcher {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a page.".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }]
    }

    fn handles(&self, name: &str) -> bool {
        name == "web_fetch"
    }

    fn check(&self, _name: &str, _arguments: &str) -> Result<ToolCheck, String> {
        Ok(ToolCheck {
            summary: "fetch a page".into(),
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
        Box::pin(async { Ok("fetched".to_string()) })
    }
}

/// A built-in integration whose one tool, `flaky`, always fails.
#[derive(Debug)]
struct Flaky;

impl ToolProvider for Flaky {
    fn specs(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "flaky".into(),
            description: "A tool that fails.".into(),
            parameters: json!({"type": "object", "properties": {"mode": {"type": "string"}}}),
        }]
    }

    fn handles(&self, name: &str) -> bool {
        name == "flaky"
    }

    fn check(&self, _name: &str, _arguments: &str) -> Result<ToolCheck, String> {
        Ok(ToolCheck {
            summary: "call flaky".into(),
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
        Box::pin(async { Err("boom".to_string()) })
    }
}

/// What one model request carried.
#[derive(Debug, Clone)]
struct Seen {
    subagent: bool,
    names: Vec<String>,
    /// The `tools` array as sent.
    tools_json: String,
    /// The latest tool result in the conversation, as the model reads it.
    last_result: Option<String>,
}

impl Seen {
    fn offers(&self, tool: &str) -> bool {
        self.names.iter().any(|name| name == tool)
    }
}

/// A model that answers the root agent's requests with the next of `root` and a subagent's with
/// the next of `sub` (a tool call, or `None` for a final answer), recording what each carried.
async fn model(root: Vec<Step>, sub: Vec<Step>, seen: Arc<Mutex<Vec<Seen>>>) -> String {
    let root_step = Arc::new(AtomicUsize::new(0));
    let sub_step = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let (root_step, sub_step) = (Arc::clone(&root_step), Arc::clone(&sub_step));
            let seen = Arc::clone(&seen);
            let (root, sub) = (root.clone(), sub.clone());
            async move {
                let subagent = body["messages"][0]["content"]
                    .as_str()
                    .is_some_and(|system| system.contains("Imperium subagent"));
                seen.lock().unwrap().push(Seen {
                    subagent,
                    names: body["tools"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                        .collect(),
                    tools_json: body["tools"].to_string(),
                    last_result: body["messages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .rev()
                        .find(|message| message["role"] == "tool")
                        .and_then(|message| message["content"].as_str().map(str::to_string)),
                });
                let (script, counter) = if subagent {
                    (&sub, &sub_step)
                } else {
                    (&root, &root_step)
                };
                let index = counter.fetch_add(1, Ordering::SeqCst);
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

/// How a session is set up.
#[derive(Default)]
struct Options {
    root: Vec<Step>,
    sub: Vec<Step>,
    loading: HarnessMode,
    profile: ProfileRules,
    hooks: Vec<ConfiguredHook>,
    launcher: Option<Arc<dyn SessionLauncher>>,
}

fn options(loading: HarnessMode, root: Vec<Step>) -> Options {
    Options {
        root,
        loading,
        ..Options::default()
    }
}

struct Setup {
    project: tempfile::TempDir,
    _config: tempfile::TempDir,
    session: Session,
    profile: AgentProfile,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// A session in a new project folder with servers `alpha` and `beta`, the plugin `words`, the
/// built-in `web_fetch` and `flaky`, and every call approved (full mode).
async fn start(options: Options) -> Setup {
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write_plugin(config.path(), &mcp_server("words").await);
    let extensions = imperium_extensions::load(&Sources {
        config_dir: config.path().to_path_buf(),
        project_dir: project.path().to_path_buf(),
        plugin_dirs: Vec::new(),
        user_plugin_dirs: Vec::new(),
        disabled_plugins: Vec::new(),
        hooks: options.hooks,
        server_timeout: Duration::from_secs(5),
        lazy: true,
        eager: Vec::new(),
        downloaded: Vec::new(),
        allowed: None,
    });
    assert!(extensions.notices.is_empty(), "{:?}", extensions.notices);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = model(options.root, options.sub, Arc::clone(&seen)).await;
    let profile = AgentProfile::new(options.profile);
    let permissions = Permissions::new(SharedRules::default(), SharedRules::default());
    let workspace = Workspace::new(project.path())
        .unwrap()
        .with_permissions(permissions)
        .with_profile(profile.clone());
    workspace.permissions().set_mode(PermissionMode::Full);
    let mcp = McpTools::new(vec![
        server("alpha", mcp_server("alpha").await),
        server("beta", mcp_server("beta").await),
    ]);
    let session = Session::spawn(SessionConfig {
        id: "reactive".into(),
        workspace,
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: url,
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
        tools: vec![Arc::new(mcp), Arc::new(Fetcher), Arc::new(Flaky)],
        notices: Vec::new(),
        extensions,
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: options.launcher,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 20,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness: harness(options.loading),
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
        journal_path: project.path().join(".journal.jsonl"),
    })
    .unwrap();
    Setup {
        project,
        _config: config,
        session,
        profile,
        seen,
    }
}

/// The harness settings with every aid off, so the tool list is only what loading decides.
fn harness(loading: HarnessMode) -> HarnessSettings {
    HarnessSettings {
        dereference: HarnessMode::Off,
        state: HarnessMode::Off,
        hints: HarnessMode::Off,
        clarify: HarnessMode::Off,
        tool_loading: loading,
        ..HarnessSettings::default()
    }
}

/// What the root agent did in one turn.
struct Turn {
    /// Its tool results, as (ok, output).
    results: Vec<(bool, String)>,
    events: Vec<SessionEvent>,
}

/// Sends `text` and collects the root's events of the turn.
async fn turn(session: &Session, text: &str) -> Turn {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut done = Turn {
        results: Vec::new(),
        events: Vec::new(),
    };
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        if event.agent_id != ROOT_AGENT {
            continue;
        }
        match event.event {
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            SessionEvent::PermissionRequested { .. } => panic!("nothing should ask"),
            other => {
                if let SessionEvent::ToolFinished { ok, output, .. } = &other {
                    done.results.push((*ok, output.clone()));
                }
                done.events.push(other);
            }
        }
    }
    done
}

fn refused_by_profile(result: &(bool, String)) -> bool {
    !result.0 && result.1.contains("permission profile does not allow this")
}

fn locked() -> ProfileRules {
    ProfileRules {
        mcp: Allowlist::All,
        plugins: Allowlist::All,
        local_files_only: true,
    }
}

/// A `tool.before` hook on `matcher` that answers with `answer`, and how often it ran.
async fn hook(matcher: &str, answer: Value) -> (ConfiguredHook, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    let app = Router::new().route(
        "/hook",
        post(move |_: axum::Json<Value>| {
            let counted = Arc::clone(&counted);
            let answer = answer.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                axum::Json(answer)
            }
        }),
    );
    let url = format!("{}/hook", serve(app).await);
    (
        ConfiguredHook {
            event: "tool.before".into(),
            matcher: matcher.into(),
            command: String::new(),
            url: Some(url),
            headers: Default::default(),
            timeout_s: Some(5),
        },
        hits,
    )
}

fn wire(spec: &ToolSpec) -> Value {
    spec.to_wire()
}

#[tokio::test]
async fn with_loading_off_the_whole_list_is_offered_unchanged_and_nothing_is_checked_against_it() {
    // `auto` is off for a hosted model: the same list, byte for byte, as `off`.
    let mut lists = Vec::new();
    for mode in [HarnessMode::Off, HarnessMode::Auto] {
        let root = vec![
            // Not offered (no knowledge agent exists), but dispatched as it always was.
            Some(("list_knowledge_agents", json!({}))),
            Some(("tool_search", json!({"query": "plan"}))),
            None,
        ];
        let setup = start(options(mode, root)).await;
        let done = turn(&setup.session, "go").await;
        assert!(
            done.results[0].0 && !done.results[0].1.contains("not one of your tools"),
            "{:?}",
            done.results
        );
        // The loader tools do not exist with loading off.
        assert!(!done.results[1].0, "{:?}", done.results);
        let seen = setup.seen.lock().unwrap();
        lists.push(seen[0].clone());
    }
    assert_eq!(lists[0].tools_json, lists[1].tools_json);
    let mut names = lists[0].names.clone();
    names.retain(|name| name != "keep_as_knowledge");
    let expected = [
        "read_file",
        "write_file",
        "edit_file",
        "list_dir",
        "glob",
        "grep",
        "run_command",
        "command_output",
        "command_stop",
        ALPHA,
        BETA,
        "update_plan",
        "activate_plugin",
        "web_fetch",
        "flaky",
        "spawn_agent",
        "await_tasks",
        "steer_agent",
        "queue_agent",
        "list_agents",
        "resume_agent",
        "delegate_chunks",
        "ask_user",
    ];
    assert_eq!(names, expected);
    // The tools the engine itself defines are the exported definitions, as they always were.
    let sent: Vec<Value> = serde_json::from_str(&lists[0].tools_json).unwrap();
    let mut wires: Vec<Value> = imperium_tools::specs().iter().map(wire).collect();
    wires.push(wire(&ToolSpec {
        name: "update_plan".into(),
        description: imperium_protocol::tools::update_plan().description.into(),
        parameters: imperium_protocol::tools::update_plan().parameters,
    }));
    for expected in wires {
        assert!(sent.contains(&expected), "{expected} is not as exported");
    }
}

#[tokio::test]
async fn with_loading_on_the_list_is_the_core_and_the_loaders_and_never_changes() {
    let plan = json!({"goal": "g", "steps": [{"title": "s", "status": "pending"}]});
    let root = vec![
        Some(("tool_search", json!({"names": ["update_plan"]}))),
        Some(("activate_plugin", json!({"name": "words"}))),
        // Every plugin is active now: the tool stays offered and says so.
        Some(("activate_plugin", json!({"name": "words"}))),
        Some(("tool_search", json!({"query": "lookup"}))),
        Some(("call_tool", json!({"name": PLUGIN_LOOKUP, "arguments": {}}))),
        Some((
            "call_tool",
            json!({"name": "update_plan", "arguments": plan}),
        )),
        None,
    ];
    let setup = start(options(HarnessMode::On, root)).await;
    let done = turn(&setup.session, "go").await;

    let seen = setup.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 7);
    assert_eq!(seen[0].names, ROOT_CORE);
    for request in &seen {
        assert_eq!(
            request.tools_json, seen[0].tools_json,
            "the list changed within the session"
        );
    }
    // The search result shows the schema.
    let found = seen[1].last_result.as_deref().unwrap();
    assert!(found.contains(r#""name":"update_plan""#), "{found}");
    assert!(found.contains(r#""required":["steps"]"#), "{found}");
    // The model reads that the plugin's tools are reached through the loaders.
    let activated = &done.results[1].1;
    assert!(
        activated.contains("tool_search and call_tool"),
        "{activated}"
    );
    assert!(
        done.results[2].1.contains("already active"),
        "{:?}",
        done.results
    );
    // A search finds the activated plugin's tool, and the proxy runs it.
    assert!(
        done.results[3].1.contains(PLUGIN_LOOKUP),
        "{:?}",
        done.results
    );
    assert_eq!(done.results[4], (true, "words ok".to_string()));
    assert!(done.results[5].0 && done.results[5].1.contains("plan updated"));
    // The events name the tool that ran, not the proxy.
    let started: Vec<&str> = done
        .events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolStarted { tool, .. } => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert!(started.contains(&"update_plan"), "{started:?}");
    assert!(started.contains(&PLUGIN_LOOKUP), "{started:?}");
    assert!(!started.contains(&"call_tool"), "{started:?}");
    assert!(
        done.events
            .iter()
            .any(|event| matches!(event, SessionEvent::PlanUpdated { plan } if plan.goal == "g"))
    );
    // What the model looked up and ran is reported once per tool.
    let loaded: Vec<(ToolLoadVia, Vec<String>)> = done
        .events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolsLoaded { names, via } => Some((*via, names.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        loaded[0],
        (ToolLoadVia::Search, vec!["update_plan".to_string()])
    );
    let all: Vec<&String> = loaded.iter().flat_map(|(_, names)| names).collect();
    assert!(
        all.iter().any(|name| name.as_str() == PLUGIN_LOOKUP),
        "{loaded:?}"
    );
    assert_eq!(
        all.iter()
            .filter(|name| name.as_str() == "update_plan")
            .count(),
        1,
        "{loaded:?}"
    );
}

#[tokio::test]
async fn tool_search_finds_by_words_and_by_name_and_suggests_close_names() {
    let root = vec![
        Some(("tool_search", json!({"query": "fetch"}))),
        Some((
            "tool_search",
            json!({"names": ["spawn_agnt", "mcp__alpha__*"]}),
        )),
        Some(("tool_search", json!({}))),
        None,
    ];
    let setup = start(options(HarnessMode::On, root)).await;
    let done = turn(&setup.session, "search").await;
    assert!(
        done.results.iter().all(|result| result.0),
        "{:?}",
        done.results
    );
    let by_words: Value = serde_json::from_str(&done.results[0].1).unwrap();
    assert_eq!(by_words["tools"][0]["name"], "web_fetch");
    let by_name: Value = serde_json::from_str(&done.results[1].1).unwrap();
    assert_eq!(by_name["tools"][0]["name"], ALPHA);
    assert_eq!(by_name["unknown"][0]["name"], "spawn_agnt");
    assert_eq!(by_name["unknown"][0]["close"][0], "spawn_agent");
    // Neither: the catalog as one-line entries.
    let catalog: Value = serde_json::from_str(&done.results[2].1).unwrap();
    assert!(catalog["tools"].as_array().unwrap().is_empty());
    let lines: Vec<&str> = catalog["more"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("update_plan: Replace your plan")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("read_file:")),
        "{lines:?}"
    );
    // The description names the deferred built-in tools, once, from the start.
    let seen = setup.seen.lock().unwrap().clone();
    let search = seen[0].tools_json.clone();
    let tools: Vec<Value> = serde_json::from_str(&search).unwrap();
    let description = tools
        .iter()
        .find(|tool| tool["function"]["name"] == "tool_search")
        .unwrap()["function"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    for name in ["update_plan", "spawn_agent", "web_fetch", "command_output"] {
        assert!(description.contains(name), "{description}");
    }
    assert!(!description.contains(ALPHA), "{description}");
    assert!(description.contains("MCP servers and activated plugins"));
}

#[tokio::test]
async fn call_tool_is_refused_where_a_direct_call_is() {
    let setup_root = vec![
        // The shell and the integrations are locked off by the profile.
        Some((
            "call_tool",
            json!({"name": "run_command", "arguments": {"command": "echo hi"}}),
        )),
        Some(("call_tool", json!({"name": "web_fetch", "arguments": {}}))),
        // The loaders are not run through the proxy.
        Some(("call_tool", json!({"name": "call_tool", "arguments": {}}))),
        Some(("call_tool", json!({"name": "tool_search", "arguments": {}}))),
        // A name that is no tool of the agent.
        Some((
            "call_tool",
            json!({"name": "no_such_tool", "arguments": {}}),
        )),
        // Files inside the folders still work through the proxy.
        Some((
            "call_tool",
            json!({"name": "read_file", "arguments": {"path": "a.txt"}}),
        )),
        // A call from a stale list is refused by the profile as before.
        Some(("run_command", json!({"command": "echo hi"}))),
        // On-mode only: tools whose arms run when they were not offered.
        Some((
            "call_tool",
            json!({"name": "list_knowledge_agents", "arguments": {}}),
        )),
        None,
    ];
    let mut setup = options(HarnessMode::On, setup_root);
    setup.profile = locked();
    let setup = start(setup).await;
    std::fs::write(setup.project.path().join("a.txt"), "AAA").unwrap();
    let done = turn(&setup.session, "try").await;
    let results = &done.results;
    assert!(refused_by_profile(&results[0]), "{results:?}");
    assert!(refused_by_profile(&results[1]), "{results:?}");
    assert!(
        !results[2].0 && results[2].1.contains("directly"),
        "{results:?}"
    );
    assert!(
        !results[3].0 && results[3].1.contains("directly"),
        "{results:?}"
    );
    assert!(
        !results[4].0
            && results[4]
                .1
                .contains("no_such_tool is not one of your tools"),
        "{results:?}"
    );
    assert!(results[5].0 && results[5].1.contains("AAA"), "{results:?}");
    assert!(refused_by_profile(&results[6]), "{results:?}");
    assert!(
        !results[7].0
            && results[7]
                .1
                .contains("list_knowledge_agents is not one of your tools"),
        "{results:?}"
    );
    let seen = setup.seen.lock().unwrap();
    assert!(!seen[0].offers("run_command") && !seen[0].offers("web_fetch"));
    assert!(seen[0].offers("tool_search") && seen[0].offers("call_tool"));
}

#[tokio::test]
async fn a_shell_lock_mid_session_removes_run_command_and_the_proxy_refuses_it() {
    // The first turn answers at once; the second tries the shell through the proxy.
    let root = vec![
        None,
        Some((
            "call_tool",
            json!({"name": "run_command", "arguments": {"command": "echo hi"}}),
        )),
        None,
    ];
    let setup = start(options(HarnessMode::On, root)).await;
    turn(&setup.session, "first").await;
    assert!(setup.seen.lock().unwrap()[0].offers("run_command"));

    setup.profile.set(locked());
    let done = turn(&setup.session, "second").await;
    assert!(refused_by_profile(&done.results[0]), "{:?}", done.results);
    let seen = setup.seen.lock().unwrap();
    assert!(!seen[1].offers("run_command"), "{:?}", seen[1].names);
    assert!(seen[1].offers("tool_search") && seen[1].offers("call_tool"));
    assert!(seen[1].offers("read_file"));
    // Nothing else about the list moved: the shell tool is the only one the lock removed.
    let mut before = seen[0].names.clone();
    before.retain(|name| name != "run_command" && name != "activate_plugin");
    let mut after = seen[1].names.clone();
    after.retain(|name| name != "activate_plugin");
    assert_eq!(before, after);
}

#[tokio::test]
async fn a_hook_sees_rewrites_and_blocks_the_inner_tool() {
    let rewrite = json!({"updated_input": {"url": "https://rewritten.example"}});
    let (rewriter, rewrites) = hook("web_fetch", rewrite).await;
    let (blocker, blocks) =
        hook("write_file", json!({"decision": "block", "reason": "nope"})).await;
    let root = vec![
        Some((
            "call_tool",
            json!({"name": "web_fetch", "arguments": {"url": "https://a.example"}}),
        )),
        Some((
            "call_tool",
            json!({"name": "write_file", "arguments": {
            "path": "blocked.txt", "content": "c"}}),
        )),
        None,
    ];
    let mut setup = options(HarnessMode::On, root);
    setup.hooks = vec![rewriter, blocker];
    let setup = start(setup).await;
    let done = turn(&setup.session, "go").await;
    // The hook saw the inner tool: it ran on the call as made and once more on the rewritten
    // call, which it cannot rewrite again, and the rewritten call is the same inner tool.
    assert_eq!(rewrites.load(Ordering::SeqCst), 2);
    assert!(
        done.results[0].0
            && done.results[0]
                .1
                .contains("A hook rewrote this call's arguments"),
        "{:?}",
        done.results
    );
    assert!(done.results[0].1.contains("fetched"), "{:?}", done.results);
    assert!(done.events.iter().any(|event| matches!(
        event,
        SessionEvent::Notice { message } if message.contains("web_fetch")
    )));
    // A hook that blocks the inner tool blocks the call.
    assert_eq!(blocks.load(Ordering::SeqCst), 1);
    assert!(
        !done.results[1].0 && done.results[1].1.contains("A hook blocked this call: nope"),
        "{:?}",
        done.results
    );
    assert!(!setup.project.path().join("blocked.txt").exists());
}

#[tokio::test]
async fn a_failed_call_to_a_deferred_tool_carries_its_parameters() {
    let root = vec![
        // The tool's own parser rejects it, as it would a direct call.
        Some((
            "call_tool",
            json!({"name": "update_plan", "arguments": {"goal": "x"}}),
        )),
        // An integration's failure is reported with the tool's parameters too.
        Some(("call_tool", json!({"name": "flaky", "arguments": {}}))),
        // A direct call to a deferred name runs like call_tool would.
        Some(("flaky", json!({}))),
        // A call that works carries nothing extra.
        Some((
            "update_plan",
            json!({"steps": [{"title": "a", "status": "pending"}]}),
        )),
        Some(("call_tool", json!({"name": ALPHA, "arguments": {}}))),
        // A string of JSON arguments is accepted too.
        Some(("call_tool", json!({"name": BETA, "arguments": "{}"}))),
        None,
    ];
    let setup = start(options(HarnessMode::On, root)).await;
    let done = turn(&setup.session, "go").await;
    let seen = setup.seen.lock().unwrap().clone();
    let advice = "The parameters of";
    let first = seen[1].last_result.as_deref().unwrap();
    assert!(first.contains("error: invalid arguments"), "{first}");
    assert!(first.contains("The parameters of update_plan"), "{first}");
    assert!(first.contains(r#""required":["steps"]"#), "{first}");
    let second = seen[2].last_result.as_deref().unwrap();
    assert!(second.contains("error: boom"), "{second}");
    assert!(second.contains("The parameters of flaky"), "{second}");
    let third = seen[3].last_result.as_deref().unwrap();
    assert!(third.contains("error: boom") && third.contains("The parameters of flaky"));
    for index in [4, 5, 6] {
        let result = seen[index].last_result.as_deref().unwrap();
        assert!(!result.contains(advice), "{result}");
    }
    assert_eq!(done.results[4], (true, "alpha ok".to_string()));
    assert_eq!(done.results[5], (true, "beta ok".to_string()));
    // The events keep the tool's own output, without the advice.
    assert!(!done.results[1].1.contains(advice), "{:?}", done.results);
}

#[tokio::test]
async fn subagents_get_the_loaders_of_their_own_universe() {
    let root = vec![
        Some((
            "call_tool",
            json!({"name": "spawn_agent", "arguments": {"brief": "look"}}),
        )),
        Some(("call_tool", json!({"name": "await_tasks", "arguments": {}}))),
        None,
    ];
    let sub = vec![
        // A subagent does not delegate: the root's tools are not in its universe.
        Some((
            "call_tool",
            json!({"name": "spawn_agent", "arguments": {"brief": "again"}}),
        )),
        None,
    ];
    let mut setup = options(HarnessMode::On, root);
    setup.sub = sub;
    let setup = start(setup).await;
    let done = turn(&setup.session, "delegate").await;
    assert!(done.results[0].0, "{:?}", done.results);
    let seen = setup.seen.lock().unwrap().clone();
    let sub_requests: Vec<&Seen> = seen.iter().filter(|request| request.subagent).collect();
    assert_eq!(sub_requests.len(), 2, "{seen:?}");
    assert!(sub_requests[0].offers("tool_search") && sub_requests[0].offers("call_tool"));
    assert!(!sub_requests[0].offers("update_plan") && !sub_requests[0].offers("spawn_agent"));
    assert_eq!(sub_requests[0].tools_json, sub_requests[1].tools_json);
    let refused = sub_requests[1].last_result.as_deref().unwrap();
    assert!(
        refused.contains("spawn_agent is not one of your tools"),
        "{refused}"
    );
    // The root's list is the same on every call.
    let roots: Vec<&Seen> = seen.iter().filter(|request| !request.subagent).collect();
    assert!(
        roots
            .iter()
            .all(|request| request.tools_json == roots[0].tools_json)
    );
}

#[tokio::test]
async fn a_catalog_of_fewer_than_three_deferred_tools_gets_no_loader() {
    // Under a local-files-only profile a subagent has the file tools and `update_plan`: one
    // deferred tool, so it is offered everything. The root still has plenty to defer.
    let root = vec![
        Some((
            "call_tool",
            json!({"name": "spawn_agent", "arguments": {"brief": "look"}}),
        )),
        Some(("call_tool", json!({"name": "await_tasks", "arguments": {}}))),
        None,
    ];
    let mut setup = options(HarnessMode::On, root);
    setup.profile = locked();
    let setup = start(setup).await;
    turn(&setup.session, "delegate").await;
    let seen = setup.seen.lock().unwrap().clone();
    let root_first = seen.iter().find(|request| !request.subagent).unwrap();
    assert!(root_first.offers("tool_search") && !root_first.offers("update_plan"));
    let sub = seen.iter().find(|request| request.subagent).unwrap();
    assert!(sub.offers("update_plan"), "{:?}", sub.names);
    assert!(
        !sub.offers("tool_search") && !sub.offers("call_tool"),
        "{:?}",
        sub.names
    );
}

#[tokio::test]
async fn a_plugin_another_agent_activated_is_reachable_at_once() {
    let root = vec![
        Some((
            "call_tool",
            json!({"name": "spawn_agent", "arguments": {"brief": "activate"}}),
        )),
        Some(("call_tool", json!({"name": "await_tasks", "arguments": {}}))),
        // The root never activated the plugin: the subagent did, in the shared session.
        Some(("call_tool", json!({"name": PLUGIN_LOOKUP, "arguments": {}}))),
        None,
    ];
    let mut setup = options(HarnessMode::On, root);
    setup.sub = vec![Some(("activate_plugin", json!({"name": "words"}))), None];
    let setup = start(setup).await;
    let done = turn(&setup.session, "delegate").await;
    assert_eq!(
        done.results[2],
        (true, "words ok".to_string()),
        "{:?}",
        done.results
    );
    let seen = setup.seen.lock().unwrap().clone();
    let roots: Vec<&Seen> = seen.iter().filter(|request| !request.subagent).collect();
    assert!(
        roots
            .iter()
            .all(|request| request.tools_json == roots[0].tools_json)
    );
}

/// A launcher whose owner allows only `start_session`, as during an initiative review.
#[derive(Debug)]
struct Review;

impl SessionLauncher for Review {
    fn launch<'a>(&'a self, _brief: &'a str, _model: Option<&'a str>) -> LaunchFuture<'a> {
        Box::pin(async { Ok("started-1".to_string()) })
    }

    fn restricted(&self, tool: &str) -> Option<String> {
        (tool != "start_session").then(|| "this turn only reads and suggests".to_string())
    }
}

#[tokio::test]
async fn the_loaders_are_exempt_from_a_launcher_limit_but_their_inner_tools_are_not() {
    let root = vec![
        Some(("tool_search", json!({"names": ["start_session"]}))),
        Some((
            "call_tool",
            json!({"name": "start_session", "arguments": {"brief": "do it"}}),
        )),
        Some((
            "call_tool",
            json!({"name": "write_file", "arguments": {"path": "x.txt", "content": "c"}}),
        )),
        None,
    ];
    let mut setup = options(HarnessMode::On, root);
    setup.launcher = Some(Arc::new(Review));
    let setup = start(setup).await;
    let done = turn(&setup.session, "review").await;
    let results = &done.results;
    assert!(
        results[0].0 && results[0].1.contains("start_session"),
        "{results:?}"
    );
    assert!(
        results[1].0 && results[1].1.contains("started-1"),
        "{results:?}"
    );
    assert!(
        !results[2].0 && results[2].1.contains("this turn only reads and suggests"),
        "{results:?}"
    );
    assert!(!setup.project.path().join("x.txt").exists());
}

/// A local runtime with one model, `m`, whose leases cost nothing.
#[derive(Debug, Default)]
struct Runtime;

impl LocalModels for Runtime {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Fits
    }

    fn has_model(&self, model: &str) -> bool {
        model == "m"
    }

    fn planned_context(&self, model: &str) -> Option<u64> {
        (model == "m").then_some(32_768)
    }

    fn acquire<'a>(&'a self, model: &'a str, holder: &'a str) -> AcquireFuture<'a> {
        self.acquire_with(model, holder, LocalRequest::default())
    }

    fn acquire_with<'a>(
        &'a self,
        _model: &'a str,
        _holder: &'a str,
        _request: LocalRequest,
    ) -> AcquireFuture<'a> {
        Box::pin(async move { Ok(LocalLease::new("http://fake/v1".into(), Some(0), None, ())) })
    }
}

/// Answers every call with "done", recording the tools it was offered.
#[derive(Debug, Default)]
struct Recorder {
    tools: Mutex<Vec<String>>,
}

impl ChatClient for Recorder {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(async move {
            let wires: Vec<Value> = request.tools.iter().map(ToolSpec::to_wire).collect();
            self.tools
                .lock()
                .unwrap()
                .push(Value::Array(wires).to_string());
            on_text("done");
            Ok(Completion {
                text: "done".into(),
                usage: Some(Usage {
                    prompt_tokens: 100,
                    completion_tokens: 1,
                    cached_tokens: 0,
                }),
                finish_reason: Some("stop".into()),
                ..Completion::default()
            })
        })
    }
}

fn scripted(spec: &str, local: bool) -> ResolvedModel {
    ResolvedModel {
        spec: spec.into(),
        endpoint: Endpoint {
            base_url: if local {
                String::new()
            } else {
                "http://hosted/v1".into()
            },
            api_key: None,
        },
        model: spec.split_once(':').unwrap().1.into(),
        pricing: None,
        local,
        context_window: None,
        kind: "scripted".into(),
        reasoning: None,
    }
}

fn names(request: &str) -> Vec<String> {
    let tools: Vec<Value> = serde_json::from_str(request).unwrap();
    tools
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn auto_follows_the_model_between_turns_and_the_list_returns_byte_for_byte() {
    let (runtime, recorder) = (Arc::new(Runtime), Arc::new(Recorder::default()));
    let dir = tempfile::tempdir().unwrap();
    let mut clients = ChatClients::default();
    let shared = Arc::clone(&recorder);
    clients.register("scripted", move |_http, _endpoint| {
        Arc::clone(&shared) as Arc<dyn ChatClient>
    });
    let local: Arc<dyn LocalModels> = runtime;
    let session = Session::spawn(SessionConfig {
        id: "auto".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(scripted("cloud:hosted", false)),
        catalog: Vec::new(),
        clients,
        routing: imperium_config::RoutingSettings::default(),
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
        personality: None,
        max_steps: 5,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        attended: true,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness: harness(HarnessMode::Auto),
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();

    turn(&session, "hosted").await;
    session
        .set_model(scripted("llamacpp:m", true))
        .await
        .unwrap();
    turn(&session, "local").await;
    session
        .set_model(scripted("cloud:hosted", false))
        .await
        .unwrap();
    turn(&session, "hosted again").await;
    session
        .set_model(scripted("llamacpp:m", true))
        .await
        .unwrap();
    turn(&session, "local again").await;

    let sent = recorder.tools.lock().unwrap().clone();
    assert_eq!(sent.len(), 4);
    // A hosted model is offered everything; a local one the fixed list.
    assert!(names(&sent[0]).contains(&"spawn_agent".to_string()));
    assert!(!names(&sent[0]).contains(&"tool_search".to_string()));
    let local_names = names(&sent[1]);
    assert!(local_names.ends_with(&["tool_search".to_string(), "call_tool".to_string()]));
    assert!(!local_names.contains(&"spawn_agent".to_string()));
    assert_eq!(
        sent[2], sent[0],
        "back on a hosted model the list is whole again"
    );
    assert_eq!(
        sent[3], sent[1],
        "back on a local model the fixed list returns unchanged"
    );
}
