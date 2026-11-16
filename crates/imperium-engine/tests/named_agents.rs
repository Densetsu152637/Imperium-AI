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
use imperium_engine::{ResolvedModel, Session, SessionConfig};
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
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let url = model(root, sub, Arc::clone(&seen)).await;
    let permissions = Permissions::new(SharedRules::new(rules), SharedRules::default());
    let workspace = Workspace::new(project.path())
        .unwrap()
        .with_permissions(permissions)
        .with_profile(AgentProfile::new(initial));
    workspace.permissions().set_mode(PermissionMode::Full);
    let session = Session::spawn(SessionConfig {
        id: "named".into(),
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
            SessionEvent::TurnCompleted { .. } => return outcome,
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
    assert!(told[0].contains("not available") && told[0].contains("fake:model"));
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
    assert_eq!(requests.len(), 3, "{requests:?}");
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
