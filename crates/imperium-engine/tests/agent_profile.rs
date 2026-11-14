//! A persistent agent's permission profile narrows what its session offers and allows: MCP
//! server and plugin allowlists hide tools from the list and refuse calls to them (also calls
//! from a stale list), "local files only" removes everything but file tools and keeps those
//! inside the folders, a profile never grants what the session denies, and subagents are held
//! to it too.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_api::{
    PermissionMode, PermissionRule, RuleAction, ToolCheck, ToolContext, ToolFuture, ToolProvider,
    ToolSpec,
};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_extensions::Sources;
use imperium_mcp::{Launch, McpTools, ServerSpec};
use imperium_models::Endpoint;
use imperium_protocol::session::{ROOT_AGENT, SessionEvent};
use imperium_tools::permissions::{Permissions, SharedRules};
use imperium_tools::{AgentProfile, Allowlist, ProfileRules, Workspace};
use serde_json::{Value, json};

const PLUGIN_LOOKUP: &str = "mcp__words_dict__lookup";
const ALPHA: &str = "mcp__alpha__lookup";
const BETA: &str = "mcp__beta__lookup";

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

/// What one model request carried.
#[derive(Debug, Clone)]
struct Seen {
    subagent: bool,
    tools: Vec<String>,
    /// The latest tool result in the conversation.
    last_result: Option<String>,
}

impl Seen {
    fn offers(&self, tool: &str) -> bool {
        self.tools.iter().any(|name| name == tool)
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
                    tools: body["tools"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                        .collect(),
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

struct Setup {
    project: tempfile::TempDir,
    _config: tempfile::TempDir,
    session: Session,
    profile: AgentProfile,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// A session in a new project folder with servers `alpha` and `beta`, the plugin `words`, a
/// built-in `web_fetch`, and every call approved (full mode) except what `rules` deny.
async fn start(
    root: Vec<Step>,
    sub: Vec<Step>,
    rules: Vec<PermissionRule>,
    initial: ProfileRules,
) -> Setup {
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    write_plugin(config.path(), &mcp_server("words").await);
    let extensions = imperium_extensions::load(&Sources {
        config_dir: config.path().to_path_buf(),
        project_dir: project.path().to_path_buf(),
        plugin_dirs: Vec::new(),
        disabled_plugins: Vec::new(),
        hooks: Vec::new(),
        server_timeout: Duration::from_secs(5),
        lazy: true,
        eager: Vec::new(),
    });
    assert!(extensions.notices.is_empty(), "{:?}", extensions.notices);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = model(root, sub, Arc::clone(&seen)).await;
    let profile = AgentProfile::new(initial);
    let permissions = Permissions::new(SharedRules::new(rules), SharedRules::default());
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
        id: "profile".into(),
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
        tools: vec![Arc::new(mcp), Arc::new(Fetcher)],
        notices: Vec::new(),
        extensions,
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
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
        harness: Default::default(),
        keep_agent_state: true,
        effort: Default::default(),
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

/// Sends `text` and returns the root's tool results of the turn, as (ok, output).
async fn turn(session: &Session, text: &str) -> Vec<(bool, String)> {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut results = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        match event.event {
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            SessionEvent::PermissionRequested { .. } => panic!("nothing should ask"),
            SessionEvent::ToolFinished { ok, output, .. } if event.agent_id == ROOT_AGENT => {
                results.push((ok, output));
            }
            _ => {}
        }
    }
    results
}

fn refused_by_profile(result: &(bool, String)) -> bool {
    !result.0 && result.1.contains("permission profile does not allow this")
}

fn rules(mcp: Allowlist, plugins: Allowlist, local_files_only: bool) -> ProfileRules {
    ProfileRules {
        mcp,
        plugins,
        local_files_only,
    }
}

#[tokio::test]
async fn allowlists_filter_the_tool_list_and_refuse_calls_from_a_stale_one() {
    let root = vec![
        // Turn one, unrestricted: activate the plugin and use it.
        Some(("activate_plugin", json!({"name": "words"}))),
        Some((PLUGIN_LOOKUP, json!({}))),
        None,
        // Turn two, after the profile narrowed: calls from the earlier list.
        Some((BETA, json!({}))),
        Some((ALPHA, json!({}))),
        Some((PLUGIN_LOOKUP, json!({}))),
        Some(("activate_plugin", json!({"name": "words"}))),
        None,
    ];
    let setup = start(root, Vec::new(), Vec::new(), ProfileRules::default()).await;

    let first = turn(&setup.session, "use everything").await;
    assert!(
        first[0].0 && first[1] == (true, "words ok".to_string()),
        "{first:?}"
    );
    {
        let seen = setup.seen.lock().unwrap();
        let before = &seen[0];
        for tool in [ALPHA, BETA, "web_fetch", "run_command", "activate_plugin"] {
            assert!(
                before.offers(tool),
                "{tool} missing from {:?}",
                before.tools
            );
        }
        assert!(
            seen[2].offers(PLUGIN_LOOKUP),
            "the active plugin's tool is offered"
        );
    }

    // Only the server alpha, and no plugins.
    setup.profile.set(rules(
        Allowlist::Named(vec!["Alpha".into()]),
        Allowlist::none(),
        false,
    ));
    let second = turn(&setup.session, "use it again").await;
    {
        let seen = setup.seen.lock().unwrap();
        let after = &seen[3];
        assert!(after.offers(ALPHA), "{:?}", after.tools);
        for tool in [BETA, PLUGIN_LOOKUP, "activate_plugin"] {
            assert!(
                !after.offers(tool),
                "{tool} still offered: {:?}",
                after.tools
            );
        }
        // Built-in tools and commands are not covered by the allowlists.
        for tool in ["web_fetch", "run_command", "read_file"] {
            assert!(after.offers(tool), "{tool} missing: {:?}", after.tools);
        }
    }
    assert!(refused_by_profile(&second[0]), "{second:?}");
    assert!(second[0].1.contains("beta"), "{second:?}");
    assert_eq!(second[1], (true, "alpha ok".to_string()));
    assert!(
        refused_by_profile(&second[2]),
        "a plugin tool from a stale list: {second:?}"
    );
    assert!(refused_by_profile(&second[3]), "{second:?}");
}

#[tokio::test]
async fn local_files_only_removes_everything_but_files_inside_the_folders() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    let secret = outside
        .path()
        .join("secret.txt")
        .to_string_lossy()
        .to_string();
    let absolute_outside = outside.path().join("new.txt").to_string_lossy().to_string();
    let root = vec![
        Some(("run_command", json!({"command": "echo hi"}))),
        Some((
            "run_command",
            json!({"command": "echo hi", "background": true}),
        )),
        Some(("web_fetch", json!({}))),
        Some((ALPHA, json!({}))),
        Some(("activate_plugin", json!({"name": "words"}))),
        Some((
            "write_file",
            json!({"path": "note.txt", "content": "inside"}),
        )),
        Some((
            "write_file",
            json!({"path": "../escape.txt", "content": "out"}),
        )),
        Some((
            "write_file",
            json!({"path": "sub/../../escape2.txt", "content": "out"}),
        )),
        Some((
            "write_file",
            json!({"path": absolute_outside, "content": "out"}),
        )),
        Some(("read_file", json!({"path": secret}))),
        None,
    ];
    let setup = start(
        root,
        Vec::new(),
        Vec::new(),
        rules(Allowlist::All, Allowlist::All, true),
    )
    .await;
    let results = turn(&setup.session, "stay local").await;

    {
        let seen = setup.seen.lock().unwrap();
        let tools = &seen[0];
        for tool in [
            "run_command",
            "command_output",
            "command_stop",
            "web_fetch",
            ALPHA,
            BETA,
            "activate_plugin",
        ] {
            assert!(!tools.offers(tool), "{tool} offered: {:?}", tools.tools);
        }
        for tool in [
            "read_file",
            "write_file",
            "edit_file",
            "list_dir",
            "glob",
            "grep",
        ] {
            assert!(tools.offers(tool), "{tool} missing: {:?}", tools.tools);
        }
    }
    for refused in &results[..5] {
        assert!(refused_by_profile(refused), "{results:?}");
        assert!(refused.1.contains("local files only"), "{results:?}");
    }
    assert!(results[5].0, "a file inside the folder: {results:?}");
    assert_eq!(
        std::fs::read_to_string(setup.project.path().join("note.txt")).unwrap(),
        "inside"
    );
    for escaped in &results[6..] {
        assert!(!escaped.0, "{results:?}");
        assert!(escaped.1.contains("outside the workspace"), "{results:?}");
    }
    let parent = setup.project.path().parent().unwrap();
    assert!(!parent.join("escape.txt").exists());
    assert!(!parent.join("escape2.txt").exists());
    assert!(!outside.path().join("new.txt").exists());
}

#[tokio::test]
async fn a_profile_never_grants_what_the_session_denies() {
    let root = vec![
        Some(("run_command", json!({"command": "echo hi"}))),
        None,
        Some((ALPHA, json!({}))),
        None,
    ];
    let deny = PermissionRule {
        tool: "run_command".into(),
        pattern: String::new(),
        action: RuleAction::Deny,
    };
    // The widest profile there is: everything allowed.
    let setup = start(
        root,
        Vec::new(),
        vec![deny],
        rules(Allowlist::All, Allowlist::All, false),
    )
    .await;
    let first = turn(&setup.session, "run it").await;
    assert!(
        !first[0].0 && first[0].1.contains("permission rule denies"),
        "{first:?}"
    );

    // Naming a server that does not exist offers nothing: the allowlist filters, it adds no
    // server and does not bring back the others.
    setup.profile.set(rules(
        Allowlist::Named(vec!["gamma".into()]),
        Allowlist::All,
        false,
    ));
    let second = turn(&setup.session, "look it up").await;
    let seen = setup.seen.lock().unwrap();
    let tools = &seen[2];
    assert!(
        !tools.tools.iter().any(|name| name.starts_with("mcp__")),
        "{:?}",
        tools.tools
    );
    drop(seen);
    assert!(refused_by_profile(&second[0]), "{second:?}");
}

#[tokio::test]
async fn subagents_are_held_to_the_profile_of_the_agent_that_started_them() {
    let root = vec![
        Some(("spawn_agent", json!({"brief": "look around"}))),
        Some(("await_tasks", json!({}))),
        None,
    ];
    let sub = vec![
        Some(("run_command", json!({"command": "echo hi"}))),
        Some((ALPHA, json!({}))),
        None,
    ];
    let setup = start(
        root,
        sub,
        Vec::new(),
        rules(Allowlist::All, Allowlist::All, true),
    )
    .await;
    turn(&setup.session, "delegate").await;

    let seen = setup.seen.lock().unwrap();
    let sub_requests: Vec<&Seen> = seen.iter().filter(|request| request.subagent).collect();
    assert_eq!(sub_requests.len(), 3, "{seen:?}");
    for request in &sub_requests {
        for tool in ["run_command", "web_fetch", ALPHA, BETA, "activate_plugin"] {
            assert!(
                !request.offers(tool),
                "{tool} offered to a subagent: {:?}",
                request.tools
            );
        }
        assert!(request.offers("read_file"));
    }
    // Its calls from memory of a tool it was not offered are refused too.
    for refused in [&sub_requests[1], &sub_requests[2]] {
        let result = refused.last_result.as_deref().unwrap_or_default();
        assert!(
            result.contains("permission profile does not allow this"),
            "{result}"
        );
    }
}
