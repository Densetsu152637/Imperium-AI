//! Named agents (agent files): `spawn_agent` can run a task as an agent a file defines. The file
//! gives it instructions, may narrow its tools to those the delegator also has, and may choose
//! its model; it never widens a tool list and never grants a permission.

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
use imperium_engine::{CatalogModel, ResolvedModel, Session, SessionConfig};
use imperium_extensions::Sources;
use imperium_models::Endpoint;
use imperium_policy::permissions::{Permissions, SharedRules};
use imperium_policy::{AgentProfile, ProfileRules};
use imperium_protocol::session::{PermissionDecision, ROOT_AGENT, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

type Step = Option<(&'static str, Value)>;

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
    system: String,
    tools: Vec<String>,
}

impl Seen {
    fn offers(&self, tool: &str) -> bool {
        self.tools.iter().any(|name| name == tool)
    }
}

/// A model that answers the root's requests with the next of `root` and a subagent's with the
/// next of `sub` (a tool call, or `None` for a final answer), recording what each carried.
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
                let system = body["messages"][0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let subagent = system.contains("Imperium subagent");
                seen.lock().unwrap().push(Seen {
                    subagent,
                    system,
                    tools: body["tools"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                        .collect(),
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

struct Setup {
    project: tempfile::TempDir,
    session: Session,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// A session in a new project folder whose user config holds `agents` (file name, text), with
/// a built-in `web_fetch`, in full mode except for what `rules` deny and `initial` hides.
async fn start(
    root: Vec<Step>,
    sub: Vec<Step>,
    agents: &[(&str, &str)],
    rules: Vec<PermissionRule>,
    initial: ProfileRules,
) -> Setup {
    start_with_catalog(root, sub, agents, rules, initial, &[]).await
}

/// A hosted model-table row: id (its spec is `<id>:model`), tier and tags.
type Row<'a> = (&'a str, imperium_config::Tier, &'a str);

/// [`start`] with hosted model-table rows beside the session's model.
async fn start_with_catalog(
    root: Vec<Step>,
    sub: Vec<Step>,
    agents: &[(&str, &str)],
    rules: Vec<PermissionRule>,
    initial: ProfileRules,
    rows: &[Row<'_>],
) -> Setup {
    start_routed(
        root,
        sub,
        agents,
        rules,
        initial,
        rows,
        imperium_config::RoutingSettings::default(),
    )
    .await
}

/// [`start_with_catalog`] with these routing settings (user rules).
async fn start_routed(
    root: Vec<Step>,
    sub: Vec<Step>,
    agents: &[(&str, &str)],
    rules: Vec<PermissionRule>,
    initial: ProfileRules,
    rows: &[Row<'_>],
    routing: imperium_config::RoutingSettings,
) -> Setup {
    let config = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    for (name, text) in agents {
        write(&config.path().join("agents").join(name), text);
    }
    write(&project.path().join("note.txt"), "a note");
    std::fs::create_dir(project.path().join("keep")).unwrap();
    write(&project.path().join("keep/file.txt"), "x");
    let extensions = imperium_extensions::load(&Sources {
        config_dir: config.path().to_path_buf(),
        project_dir: project.path().to_path_buf(),
        plugin_dirs: Vec::new(),
        user_plugin_dirs: Vec::new(),
        disabled_plugins: Vec::new(),
        hooks: Vec::new(),
        server_timeout: Duration::from_secs(5),
        lazy: true,
        eager: Vec::new(),
        downloaded: Vec::new(),
        allowed: None,
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = model(root, sub, Arc::clone(&seen)).await;
    let permissions = Permissions::new(SharedRules::new(rules), SharedRules::default());
    let workspace = Workspace::new(project.path())
        .unwrap()
        .with_permissions(permissions)
        .with_profile(AgentProfile::new(initial));
    workspace.permissions().set_mode(PermissionMode::Full);
    let resolved = |spec: &str| ResolvedModel {
        spec: spec.into(),
        endpoint: Endpoint {
            base_url: url.clone(),
            api_key: None,
        },
        model: "model".into(),
        pricing: None,
        local: false,
        context_window: None,
        kind: "openai".into(),
        reasoning: None,
    };
    let catalog = rows
        .iter()
        .map(|(id, tier, tags)| CatalogModel {
            id: (*id).into(),
            model: resolved(&format!("{id}:model")),
            tier: *tier,
            location: imperium_config::Location::Cloud,
            tags: tags
                .split(',')
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect(),
            named_only: false,
            cpu_lane: false,
        })
        .collect();
    let session = Session::spawn(SessionConfig {
        id: "named".into(),
        workspace,
        model: Some(resolved("fake:model")),
        catalog,
        routing,
        clients: Default::default(),
        local: None,
        tools: vec![Arc::new(Fetcher)],
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
        attended: true,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness: Default::default(),
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
        journal_path: project.path().join(".journal.jsonl"),
    })
    .unwrap();
    Setup {
        project,
        session,
        seen,
    }
}

/// What a turn did.
#[derive(Debug, Default)]
struct Outcome {
    /// The root's tool results, as (ok, output).
    root: Vec<(bool, String)>,
    /// The subagents' tool results, as (ok, output).
    sub: Vec<(bool, String)>,
    /// The tools that asked for permission, and what they asked to run.
    asked: Vec<String>,
    notices: Vec<String>,
    /// The models the subagents were started on.
    started_on: Vec<String>,
}

/// Sends `text` and runs the turn to its end, answering every permission request with `answer`.
async fn turn(session: &Session, text: &str, answer: PermissionDecision) -> Outcome {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut outcome = Outcome::default();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap();
        match event.event {
            SessionEvent::TurnCompleted { .. } if event.agent_id == ROOT_AGENT => return outcome,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            SessionEvent::PermissionRequested {
                request_id,
                tool,
                summary,
                ..
            } => {
                outcome.asked.push(format!("{tool}: {summary}"));
                session.respond(request_id, answer).unwrap();
            }
            SessionEvent::Notice { message } => outcome.notices.push(message),
            SessionEvent::TaskSpawned { model, .. } => outcome.started_on.push(model),
            SessionEvent::ToolFinished { ok, output, .. } => {
                if event.agent_id == ROOT_AGENT {
                    outcome.root.push((ok, output));
                } else {
                    outcome.sub.push((ok, output));
                }
            }
            _ => {}
        }
    }
}

fn agent_file(name: &str, description: &str, extra: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n{extra}---\n{body}\n")
}

fn spawn(agent: &str) -> Step {
    Some((
        "spawn_agent",
        json!({"brief": "look at note.txt", "agent": agent}),
    ))
}

fn subagent_requests(setup: &Setup) -> Vec<Seen> {
    setup
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.subagent)
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_root_is_told_the_names_and_a_subagent_runs_as_the_named_agent() {
    let file = agent_file(
        "scout",
        "Reads files and reports. Use for lookups.",
        "tools: Read, grep\n",
        "SCOUT RULES: only read.",
    );
    let setup = start(
        vec![spawn("scout"), Some(("await_tasks", json!({}))), None],
        vec![Some(("read_file", json!({"path": "note.txt"}))), None],
        &[("scout.md", &file)],
        Vec::new(),
        ProfileRules::default(),
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert!(
        outcome.root[0].0 && outcome.root[0].1.contains("runs as the agent scout"),
        "{outcome:?}"
    );
    assert!(
        outcome.sub[0].0 && outcome.sub[0].1.contains("a note"),
        "{outcome:?}"
    );

    let seen = setup.seen.lock().unwrap();
    // The root has the name and the first line of the description, and not the file's body.
    let root = seen.iter().find(|request| !request.subagent).unwrap();
    assert!(
        root.system
            .contains("- scout: Reads files and reports. Use for lookups."),
        "{}",
        root.system
    );
    assert!(!root.system.contains("SCOUT RULES"));
    assert!(root.offers("spawn_agent"));
    drop(seen);

    // The subagent has the file's instructions after its own, and the listed tools only
    // (Claude Code's `Read` stands for `read_file`).
    for request in subagent_requests(&setup) {
        assert!(request.system.contains("Imperium subagent"));
        assert!(request.system.contains("SCOUT RULES: only read."));
        let mut tools = request.tools.clone();
        tools.sort();
        assert_eq!(tools, ["grep", "read_file"], "{tools:?}");
    }
}

#[tokio::test]
async fn a_file_cannot_give_a_tool_the_delegator_does_not_have() {
    // `local_files_only` takes web_fetch away from every agent of the session, root included.
    let file = agent_file(
        "fetcher",
        "Fetches pages.",
        "tools: web_fetch, read_file, no_such_tool\n",
        "Fetch.",
    );
    let setup = start(
        vec![spawn("fetcher"), Some(("await_tasks", json!({}))), None],
        vec![Some(("web_fetch", json!({}))), None],
        &[("fetcher.md", &file)],
        Vec::new(),
        ProfileRules {
            local_files_only: true,
            ..ProfileRules::default()
        },
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    let root_tools = setup
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|request| !request.subagent)
        .unwrap()
        .tools
        .clone();
    assert!(!root_tools.iter().any(|tool| tool == "web_fetch"));
    for request in subagent_requests(&setup) {
        assert_eq!(request.tools, ["read_file"], "{:?}", request.tools);
    }
    // The call from memory of a tool it was never offered is refused.
    assert!(
        !outcome.sub[0].0
            && outcome.sub[0]
                .1
                .contains("not one of the tools of the agent"),
        "{outcome:?}"
    );
    // The user is told what was left out.
    for tool in ["web_fetch", "no_such_tool"] {
        assert!(
            outcome
                .notices
                .iter()
                .any(|notice| notice.contains(tool) && notice.contains("left out")),
            "{tool}: {:?}",
            outcome.notices
        );
    }
}

#[tokio::test]
async fn a_tool_outside_the_files_list_is_refused_when_called() {
    let file = agent_file("reader", "Reads.", "tools: read_file\n", "Read.");
    let setup = start(
        vec![spawn("reader"), Some(("await_tasks", json!({}))), None],
        vec![
            Some(("run_command", json!({"command": "echo hi"}))),
            Some(("write_file", json!({"path": "out.txt", "content": "x"}))),
            None,
        ],
        &[("reader.md", &file)],
        Vec::new(),
        ProfileRules::default(),
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Allow).await;
    assert_eq!(outcome.sub.len(), 2, "{outcome:?}");
    for result in &outcome.sub {
        assert!(
            !result.0
                && result
                    .1
                    .contains("not one of the tools of the agent reader"),
            "{result:?}"
        );
    }
    assert!(outcome.asked.is_empty(), "{:?}", outcome.asked);
    assert!(!setup.project.path().join("out.txt").exists());
}

#[tokio::test]
async fn a_named_agent_with_a_tool_still_meets_the_rules_and_the_safety_floor() {
    let file = agent_file("shell", "Runs commands.", "tools: Bash\n", "Run things.");
    let deny = PermissionRule {
        tool: "run_command".into(),
        pattern: "echo".into(),
        action: RuleAction::Deny,
    };
    let setup = start(
        vec![spawn("shell"), Some(("await_tasks", json!({}))), None],
        vec![
            Some(("run_command", json!({"command": "echo hi"}))),
            Some(("run_command", json!({"command": "rm -rf keep"}))),
            None,
        ],
        &[("shell.md", &file)],
        vec![deny],
        ProfileRules::default(),
    )
    .await;
    // Nobody approves what asks.
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    // A deny rule refuses the first call, though the file lists the tool.
    assert!(
        !outcome.sub[0].0 && outcome.sub[0].1.contains("permission rule denies"),
        "{outcome:?}"
    );
    // The floor still asks about the destructive one, even in full mode, and a no is a no.
    assert!(
        outcome.asked.iter().any(|ask| ask.contains("rm -rf keep")),
        "{:?}",
        outcome.asked
    );
    assert!(setup.project.path().join("keep/file.txt").exists());
}

#[tokio::test]
async fn a_name_that_is_not_defined_starts_nothing_and_lists_the_names() {
    let file = agent_file("scout", "Looks.", "", "Look.");
    let setup = start(
        vec![spawn("nobody"), None],
        Vec::new(),
        &[("scout.md", &file)],
        Vec::new(),
        ProfileRules::default(),
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert!(
        !outcome.root[0].0
            && outcome.root[0].1.contains("no named agent \"nobody\"")
            && outcome.root[0].1.contains("scout"),
        "{outcome:?}"
    );
    assert!(subagent_requests(&setup).is_empty());

    let none = start(
        vec![spawn("anyone"), None],
        Vec::new(),
        &[],
        Vec::new(),
        ProfileRules::default(),
    )
    .await;
    let outcome = turn(&none.session, "delegate", PermissionDecision::Deny).await;
    assert!(
        !outcome.root[0].0 && outcome.root[0].1.contains("no agent files are defined"),
        "{outcome:?}"
    );
    // With no agents the root's prompt has no list.
    let seen = none.seen.lock().unwrap();
    assert!(!seen[0].system.contains("Named agents"));
}

#[tokio::test]
async fn a_model_that_is_not_available_falls_back_to_the_delegators_with_a_notice() {
    let missing = agent_file(
        "far",
        "Wants a model.",
        "model: nowhere:no-such-model\n",
        "Far.",
    );
    let inherit = agent_file("near", "Inherits.", "model: inherit\n", "Near.");
    let setup = start(
        vec![
            spawn("far"),
            spawn("near"),
            Some(("await_tasks", json!({}))),
            None,
        ],
        Vec::new(),
        &[("far.md", &missing), ("near.md", &inherit)],
        Vec::new(),
        ProfileRules::default(),
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert!(outcome.root[0].0 && outcome.root[1].0, "{outcome:?}");
    let told: Vec<&String> = outcome
        .notices
        .iter()
        .filter(|notice| notice.contains("nowhere:no-such-model"))
        .collect();
    assert_eq!(told.len(), 1, "{:?}", outcome.notices);
    assert!(told[0].contains("not available") && told[0].contains("routed normally"));
    // `inherit` needs no notice.
    assert!(!outcome.notices.iter().any(|notice| notice.contains("near")));
}

#[tokio::test]
async fn a_resumed_named_agent_is_the_same_agent_with_the_same_tools() {
    let file = agent_file("reader", "Reads.", "tools: read_file\n", "READER RULES.");
    let setup = start(
        vec![
            spawn("reader"),
            Some(("await_tasks", json!({}))),
            Some((
                "resume_agent",
                json!({"agent_id": "t1", "text": "once more"}),
            )),
            Some(("await_tasks", json!({}))),
            None,
        ],
        vec![
            // First turn: a plain answer. Resumed: it tries a tool its file does not list.
            None,
            Some(("run_command", json!({"command": "echo hi"}))),
            None,
        ],
        &[("reader.md", &file)],
        Vec::new(),
        ProfileRules::default(),
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Allow).await;
    let requests = subagent_requests(&setup);
    assert_eq!(requests.len(), 3, "{outcome:?}");
    for request in &requests {
        assert!(request.system.contains("READER RULES."));
        assert_eq!(request.tools, ["read_file"], "{:?}", request.tools);
    }
    assert!(
        outcome
            .sub
            .iter()
            .any(|result| !result.0 && result.1.contains("not one of the tools of the agent")),
        "{outcome:?}"
    );
}

fn spawn_with(agent: Option<&str>, location: Option<&str>) -> Step {
    let mut arguments = json!({"brief": "look at note.txt"});
    if let Some(agent) = agent {
        arguments["agent"] = json!(agent);
    }
    if let Some(location) = location {
        arguments["location"] = json!(location);
    }
    Some(("spawn_agent", arguments))
}

#[tokio::test]
async fn a_files_model_never_overrides_the_location_the_task_is_confined_to() {
    let hosted = agent_file("far", "Wants a hosted model.", "model: rowx\n", "Far.");
    let inherit = agent_file("near", "Inherits.", "model: inherit\n", "Near.");
    let setup = start_with_catalog(
        vec![
            spawn_with(Some("far"), Some("local")),
            spawn_with(Some("near"), Some("local")),
            spawn_with(Some("far"), None),
            Some(("await_tasks", json!({}))),
            None,
        ],
        Vec::new(),
        &[("far.md", &hosted), ("near.md", &inherit)],
        Vec::new(),
        ProfileRules::default(),
        &[("rowx", Default::default(), "")],
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert_eq!(outcome.started_on.len(), 3, "{outcome:?}");
    // Confined to local: the hosted row is not taken, and a hosted session model is not
    // inherited; each says why.
    assert_ne!(outcome.started_on[0], "rowx:model", "{outcome:?}");
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("rowx") && n.contains("location") && n.contains("routed normally")),
        "{:?}",
        outcome.notices
    );
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("inherits") && n.contains("location")),
        "{:?}",
        outcome.notices
    );
    // With no confinement the file's model is used.
    assert_eq!(outcome.started_on[2], "rowx:model", "{outcome:?}");
}

#[tokio::test]
async fn an_unavailable_files_model_keeps_the_routed_choice() {
    let file = agent_file("far", "Wants a model.", "model: nowhere:gone\n", "Far.");
    let setup = start_with_catalog(
        vec![
            spawn_with(Some("far"), None),
            spawn_with(None, None),
            Some(("await_tasks", json!({}))),
            None,
        ],
        Vec::new(),
        &[("far.md", &file)],
        Vec::new(),
        ProfileRules::default(),
        &[("rowx", Default::default(), "")],
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert_eq!(outcome.started_on.len(), 2, "{outcome:?}");
    // Routed as the same call without the agent is: not forced onto the session's model.
    assert_eq!(outcome.started_on[0], outcome.started_on[1], "{outcome:?}");
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("nowhere:gone") && n.contains("routed normally")),
        "{:?}",
        outcome.notices
    );
}

fn spawn_requiring(agent: &str, extra: Value) -> Step {
    let mut arguments = json!({"brief": "look at note.txt", "agent": agent});
    for (key, value) in extra.as_object().unwrap() {
        arguments[key] = value.clone();
    }
    Some(("spawn_agent", arguments))
}

#[tokio::test]
async fn a_files_model_never_overrides_the_tier_the_task_requires() {
    let file = agent_file("cheap", "Wants a basic model.", "model: rowx\n", "Cheap.");
    let setup = start_with_catalog(
        vec![
            spawn_requiring("cheap", json!({"tier": "strong"})),
            spawn_requiring("cheap", json!({})),
            Some(("await_tasks", json!({}))),
            None,
        ],
        Vec::new(),
        &[("cheap.md", &file)],
        Vec::new(),
        ProfileRules::default(),
        &[("rowx", imperium_config::Tier::Basic, "")],
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert_eq!(outcome.started_on.len(), 2, "{outcome:?}");
    // Under a strong tier the basic row is passed over, with a notice; without one it is used.
    assert_ne!(outcome.started_on[0], "rowx:model", "{outcome:?}");
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("rowx") && n.contains("tier") && n.contains("routed normally")),
        "{:?}",
        outcome.notices
    );
    assert_eq!(outcome.started_on[1], "rowx:model", "{outcome:?}");
}

#[tokio::test]
async fn inherit_respects_the_capabilities_the_task_requires() {
    let file = agent_file("same", "Inherits.", "model: inherit\n", "Same.");
    let setup = start_with_catalog(
        vec![
            spawn_requiring("same", json!({"tags": ["vision"]})),
            Some(("await_tasks", json!({}))),
            None,
        ],
        Vec::new(),
        &[("same.md", &file)],
        Vec::new(),
        ProfileRules::default(),
        // The session's own model is in the table and reads no images (no `vision` tag).
        &[("fake", imperium_config::Tier::Standard, "")],
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("inherits") && n.contains("capabilities")),
        "{:?}",
        outcome.notices
    );
}

/// Rules written for the default (standard) tier, which a call that sets no tier still meets.
fn standard_rule(
    location: Option<imperium_config::Location>,
    require: Vec<imperium_config::Capability>,
) -> imperium_config::RoutingSettings {
    imperium_config::RoutingSettings {
        rules: vec![imperium_config::RoutingRule {
            when_tier: Some(imperium_config::Tier::Standard),
            location,
            require,
            ..imperium_config::RoutingRule::default()
        }],
        ..imperium_config::RoutingSettings::default()
    }
}

#[tokio::test]
async fn a_rule_for_the_default_tier_confines_a_file_model_when_the_call_sets_no_tier() {
    let hosted = agent_file("far", "Wants a hosted model.", "model: rowx\n", "Far.");
    let inherit = agent_file("near", "Inherits.", "model: inherit\n", "Near.");
    let setup = start_routed(
        vec![
            spawn_requiring("far", json!({})),
            spawn_requiring("near", json!({})),
            Some(("await_tasks", json!({}))),
            None,
        ],
        Vec::new(),
        &[("far.md", &hosted), ("near.md", &inherit)],
        Vec::new(),
        ProfileRules::default(),
        // The session's own hosted model is in the table too.
        &[
            ("rowx", Default::default(), ""),
            ("fake", Default::default(), ""),
        ],
        standard_rule(Some(imperium_config::Location::Local), Vec::new()),
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert_eq!(outcome.started_on.len(), 2, "{outcome:?}");
    assert_ne!(outcome.started_on[0], "rowx:model", "{outcome:?}");
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("rowx") && n.contains("location") && n.contains("routed normally")),
        "{:?}",
        outcome.notices
    );
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("inherits") && n.contains("location")),
        "{:?}",
        outcome.notices
    );
}

#[tokio::test]
async fn a_rules_required_capabilities_apply_to_a_file_model() {
    let file = agent_file("far", "Wants a model.", "model: rowx\n", "Far.");
    let setup = start_routed(
        vec![
            spawn_requiring("far", json!({})),
            Some(("await_tasks", json!({}))),
            None,
        ],
        Vec::new(),
        &[("far.md", &file)],
        Vec::new(),
        ProfileRules::default(),
        // No `vision` tag on the row.
        &[("rowx", Default::default(), "")],
        standard_rule(None, vec![imperium_config::Capability::Vision]),
    )
    .await;
    let outcome = turn(&setup.session, "delegate", PermissionDecision::Deny).await;
    assert_eq!(outcome.started_on.len(), 1, "{outcome:?}");
    assert_ne!(outcome.started_on[0], "rowx:model", "{outcome:?}");
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.contains("rowx") && n.contains("routed normally")),
        "{:?}",
        outcome.notices
    );
}
