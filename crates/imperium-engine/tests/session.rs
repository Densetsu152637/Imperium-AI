use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::{Endpoint, Pricing};
use imperium_protocol::session::{AgentEvent, PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use imperium_tools::permissions::{
    PermissionMode, PermissionRule, Permissions, RuleAction, SharedRules,
};
use tokio::sync::broadcast;

/// A fake endpoint that replays one scripted SSE body per request.
async fn scripted_endpoint(bodies: Vec<String>) -> Endpoint {
    let calls = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(bodies);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let index = calls.fetch_add(1, Ordering::SeqCst);
            let body = bodies[index.min(bodies.len() - 1)].clone();
            async move { ([("content-type", "text/event-stream")], body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    Endpoint {
        base_url,
        api_key: None,
    }
}

fn tool_call_body(name: &str, arguments: &str) -> String {
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":"call-1","function":{"name":name,"arguments":arguments}}]}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20}}"#
    )
}

fn text_body(text: &str) -> String {
    let chunk = serde_json::json!({"choices":[{"delta":{"content":text}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":10}}"#
    )
}

/// The next event, skipping status changes (tested on their own in tests/usage_limits.rs) and
/// context breakdowns (tests/context.rs).
async fn next(events: &mut broadcast::Receiver<AgentEvent>) -> SessionEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if !matches!(
            event,
            SessionEvent::StatusChanged { .. } | SessionEvent::ContextBreakdown { .. }
        ) {
            return event;
        }
    }
}

fn start(endpoint: Endpoint, dir: &std::path::Path) -> Session {
    start_in(endpoint, dir, Workspace::new(dir).unwrap())
}

fn start_in(endpoint: Endpoint, dir: &std::path::Path, workspace: Workspace) -> Session {
    start_with(endpoint, dir, workspace, Vec::new())
}

fn start_with(
    endpoint: Endpoint,
    dir: &std::path::Path,
    workspace: Workspace,
    pre_approved: Vec<String>,
) -> Session {
    let mut config = config(endpoint, dir, workspace);
    config.pre_approved = pre_approved;
    Session::spawn(config).unwrap()
}

fn config(endpoint: Endpoint, dir: &std::path::Path, workspace: Workspace) -> SessionConfig {
    SessionConfig {
        id: "test".into(),
        workspace,
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint,
            model: "model".into(),
            local: false,
            context_window: None,
            kind: "openai".into(),
            reasoning: None,
            pricing: Some(Pricing {
                input_usd_per_mtok: 1.0,
                output_usd_per_mtok: 2.0,
                cached_input_usd_per_mtok: None,
            }),
        }),
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        clients: Default::default(),
        local: None,
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
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".journal/test.jsonl"),
    }
}

#[tokio::test]
async fn an_approved_write_changes_the_file_and_the_turn_reports_cost() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("write_file", r#"{"path":"hello.txt","content":"hi"}"#),
        text_body("Created hello.txt."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();
    session.send("create hello.txt".into()).unwrap();

    assert!(matches!(
        next(&mut events).await,
        SessionEvent::TurnStarted { .. }
    ));
    let SessionEvent::ModelCallCompleted {
        usage,
        cost_usd,
        tokens_per_sec,
        ..
    } = next(&mut events).await
    else {
        panic!("expected per-call telemetry");
    };
    assert_eq!(usage.prompt_tokens, 100);
    // 100 × $1 + 20 × $2, per million tokens.
    assert!((cost_usd.unwrap() - 0.00014).abs() < 1e-12);
    assert!(tokens_per_sec.is_some());
    let SessionEvent::PermissionRequested {
        request_id, tool, ..
    } = next(&mut events).await
    else {
        panic!("expected a permission request");
    };
    assert_eq!(tool, "write_file");
    session
        .respond(request_id, PermissionDecision::Allow)
        .unwrap();

    // The transcript shows each call with its arguments, result, and duration.
    let SessionEvent::ToolStarted { arguments, .. } = next(&mut events).await else {
        panic!("expected the tool to start");
    };
    assert!(arguments.unwrap().contains("hello.txt"));
    let SessionEvent::ToolFinished {
        ok,
        tool,
        duration_ms,
        ..
    } = next(&mut events).await
    else {
        panic!("expected the tool to finish");
    };
    assert!(ok);
    assert_eq!(tool.as_deref(), Some("write_file"));
    assert!(duration_ms.is_some());
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::TextDelta { .. }
    ));
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::ModelCallCompleted { .. }
    ));
    let SessionEvent::TurnCompleted {
        usage,
        cost_usd,
        steps,
    } = next(&mut events).await
    else {
        panic!("expected the turn to complete");
    };
    assert_eq!(steps, 2);
    assert_eq!(usage.prompt_tokens, 250);
    assert_eq!(usage.completion_tokens, 30);
    // 250 × $1 + 30 × $2, per million tokens.
    assert!((cost_usd.unwrap() - 0.00031).abs() < 1e-12);

    assert_eq!(
        std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
        "hi"
    );
    let journal = std::fs::read_to_string(dir.path().join(".journal/test.jsonl")).unwrap();
    let (history, _) = session.subscribe_with_history();
    // Eight turn events, a context breakdown after each of the two model calls, and the status
    // changes they caused (working, needs attention while asking, working, idle).
    let count = |matches: fn(&SessionEvent) -> bool| {
        history.iter().filter(|event| matches(&event.event)).count()
    };
    let statuses = count(|event| matches!(event, SessionEvent::StatusChanged { .. }));
    let breakdowns = count(|event| matches!(event, SessionEvent::ContextBreakdown { .. }));
    assert_eq!(history.len() - statuses - breakdowns, 8);
    assert_eq!(statuses, 4);
    assert_eq!(breakdowns, 2);
    assert_eq!(journal.lines().count(), history.len());
    assert!(matches!(history[0].event, SessionEvent::TurnStarted { .. }));
    assert!(history.iter().all(|event| event.agent_id == "root"));
    assert!(journal.contains(r#""type":"turn_completed""#));
}

#[tokio::test]
async fn a_denied_command_does_not_run() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body(
            "run_command",
            r#"{"command":"echo should-not-run > ran.txt"}"#,
        ),
        text_body("Understood."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();
    session.send("run it".into()).unwrap();

    next(&mut events).await;
    next(&mut events).await;
    let SessionEvent::PermissionRequested { request_id, .. } = next(&mut events).await else {
        panic!("expected a permission request");
    };
    session
        .respond(request_id, PermissionDecision::Deny)
        .unwrap();
    let SessionEvent::ToolFinished { ok, output, .. } = next(&mut events).await else {
        panic!("expected the denied tool to finish");
    };
    assert!(!ok);
    assert!(output.contains("denied"));
    assert!(!dir.path().join("ran.txt").exists());
}

fn rules(list: &[(&str, &str, RuleAction)]) -> SharedRules {
    SharedRules::new(
        list.iter()
            .map(|(tool, pattern, action)| PermissionRule {
                tool: (*tool).into(),
                pattern: (*pattern).into(),
                action: *action,
            })
            .collect(),
    )
}

#[tokio::test]
async fn an_allow_rule_runs_a_command_without_asking() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("run_command", r#"{"command":"echo ran > ran.txt"}"#),
        text_body("Done."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    // `echo` is allowed globally; the redirection into a file is not covered by a pattern,
    // so the environment allows the whole tool instead.
    let permissions = Permissions::new(
        rules(&[("run_command", "echo", RuleAction::Allow)]),
        rules(&[("run_command", "", RuleAction::Allow)]),
    );
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions);
    let session = start_in(endpoint, dir.path(), workspace);
    let mut events = session.subscribe();
    session.send("run it".into()).unwrap();

    next(&mut events).await;
    next(&mut events).await;
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::ToolStarted { .. }
    ));
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::ToolFinished { ok: true, .. }
    ));
    assert!(dir.path().join("ran.txt").exists());
}

#[tokio::test]
async fn a_deny_rule_refuses_without_asking_and_an_ask_rule_asks_for_reads() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("write_file", r#"{"path":"secrets/.env","content":"x"}"#),
        tool_call_body("read_file", r#"{"path":"notes.txt"}"#),
        text_body("Done."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "private").unwrap();
    let permissions = Permissions::new(
        rules(&[("*", ".env", RuleAction::Deny)]),
        rules(&[("read_file", "notes.txt", RuleAction::Ask)]),
    );
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions);
    let session = start_in(endpoint, dir.path(), workspace);
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();

    next(&mut events).await;
    next(&mut events).await;
    let SessionEvent::ToolFinished { ok, output, .. } = next(&mut events).await else {
        panic!("expected the denied write to finish at once");
    };
    assert!(!ok);
    assert!(output.contains("permission rule denies"), "{output}");
    assert!(!dir.path().join("secrets").exists());

    // The read asks, because of the environment's ask rule, and offers a rule to always allow.
    next(&mut events).await;
    let SessionEvent::PermissionRequested {
        request_id,
        tool,
        suggested_rule,
        ..
    } = next(&mut events).await
    else {
        panic!("expected the read to ask");
    };
    assert_eq!(tool, "read_file");
    let suggested = suggested_rule.expect("a suggested rule");
    assert_eq!(suggested.tool, "read_file");
    assert!(suggested.pattern.ends_with("/**"), "{}", suggested.pattern);
    session
        .respond(request_id, PermissionDecision::Allow)
        .unwrap();
    assert!(matches!(
        next(&mut events).await,
        SessionEvent::ToolStarted { .. }
    ));
}

/// A fake endpoint that tells the root agent and subagents apart by their system prompt.
async fn delegating_endpoint(
    root_calls: Arc<AtomicUsize>,
    sub_calls: Arc<AtomicUsize>,
) -> Endpoint {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let root_calls = Arc::clone(&root_calls);
            let sub_calls = Arc::clone(&sub_calls);
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                let body = if system.contains("subagent") {
                    let n = sub_calls.fetch_add(1, Ordering::SeqCst);
                    text_body(&format!("report {n}"))
                } else {
                    match root_calls.fetch_add(1, Ordering::SeqCst) {
                        0 => {
                            let calls = serde_json::json!({"choices":[{"delta":{"tool_calls":[
                                {"index":0,"id":"a","function":{"name":"spawn_agent","arguments":"{\"brief\":\"check docs\"}"}},
                                {"index":1,"id":"b","function":{"name":"spawn_agent","arguments":"{\"brief\":\"check tests\",\"tier\":\"basic\"}"}}
                            ]}}]});
                            format!("data: {calls}

data: [DONE]

")
                        }
                        1 => tool_call_body("await_tasks", "{}"),
                        _ => text_body("Both checks are done."),
                    }
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    Endpoint {
        base_url,
        api_key: None,
    }
}

#[tokio::test]
async fn the_root_delegates_waits_without_model_calls_and_collects_reports() {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let sub_calls = Arc::new(AtomicUsize::new(0));
    let endpoint = delegating_endpoint(Arc::clone(&root_calls), Arc::clone(&sub_calls)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();
    session.send("check docs and tests".into()).unwrap();

    let mut spawned = Vec::new();
    let mut finished = Vec::new();
    let mut breakdowns = std::collections::BTreeMap::<String, usize>::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::TaskSpawned { task_id, .. } => spawned.push(task_id),
            SessionEvent::TaskFinished { task_id, ok, .. } => {
                assert!(ok);
                assert_eq!(event.agent_id, task_id);
                finished.push(task_id);
            }
            SessionEvent::TurnCompleted { steps, .. } if event.agent_id == "root" => {
                assert_eq!(steps, 3);
                break;
            }
            SessionEvent::ContextBreakdown { .. } => {
                *breakdowns.entry(event.agent_id).or_default() += 1;
            }
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            _ => {}
        }
    }
    // Every agent reports what each of its own model calls read.
    assert_eq!(
        breakdowns.into_iter().collect::<Vec<_>>(),
        [("root".into(), 3), ("t1".into(), 1), ("t2".into(), 1)]
    );
    assert_eq!(spawned, ["t1", "t2"]);
    finished.sort();
    assert_eq!(finished, ["t1", "t2"]);
    // Spawn, await, and summary: waiting for the subagents cost the root no model calls.
    assert_eq!(root_calls.load(Ordering::SeqCst), 3);
    assert_eq!(sub_calls.load(Ordering::SeqCst), 2);
}

/// A single-call result: whether the task succeeded, its report, and the tool counts offered
/// on each subagent model call.
struct SingleCall {
    ok: bool,
    report: String,
    tools_offered: Vec<usize>,
}

/// Runs a root that spawns one single-call subagent, which answers with `sub_reply`.
async fn run_single_call(sub_reply: String) -> SingleCall {
    let tools_offered = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let sub_calls = Arc::new(AtomicUsize::new(0));
    let root_calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post({
            let tools_offered = Arc::clone(&tools_offered);
            let sub_calls = Arc::clone(&sub_calls);
            let root_calls = Arc::clone(&root_calls);
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let sub_reply = sub_reply.clone();
                let tools_offered = Arc::clone(&tools_offered);
                let sub_calls = Arc::clone(&sub_calls);
                let root_calls = Arc::clone(&root_calls);
                async move {
                    let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                    let reply = if system.contains("subagent") {
                        sub_calls.fetch_add(1, Ordering::SeqCst);
                        let tools = body["tools"].as_array().map_or(0, Vec::len);
                        tools_offered.lock().unwrap().push(tools);
                        sub_reply.clone()
                    } else {
                        match root_calls.fetch_add(1, Ordering::SeqCst) {
                            0 => tool_call_body(
                                "spawn_agent",
                                r#"{"brief":"solve it","single_call":true}"#,
                            ),
                            1 => tool_call_body("await_tasks", "{}"),
                            _ => text_body("done"),
                        }
                    };
                    ([("content-type", "text/event-stream")], reply)
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let dir = tempfile::tempdir().unwrap();
    let session = start(
        Endpoint {
            base_url,
            api_key: None,
        },
        dir.path(),
    );
    let mut events = session.subscribe();
    session.send("solve it".into()).unwrap();

    let mut result = None;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::TaskFinished { ok, report, .. } => result = Some((ok, report)),
            SessionEvent::TurnCompleted { .. } if event.agent_id == "root" => break,
            SessionEvent::TurnFailed { message } if event.agent_id == "root" => {
                panic!("turn failed: {message}")
            }
            _ => {}
        }
    }
    // One model call, whatever it returned.
    assert_eq!(sub_calls.load(Ordering::SeqCst), 1);
    let (ok, report) = result.expect("the task finished");
    let tools_offered = tools_offered.lock().unwrap().clone();
    SingleCall {
        ok,
        report,
        tools_offered,
    }
}

#[tokio::test]
async fn a_single_call_subagent_makes_one_model_call_with_no_tools_and_its_reply_is_the_report() {
    let result = run_single_call(text_body("the proof is: x = 2")).await;
    assert!(result.ok);
    assert_eq!(result.report, "the proof is: x = 2");
    assert_eq!(result.tools_offered, [0]);
}

#[tokio::test]
async fn a_single_call_subagent_with_a_blank_reply_fails() {
    for reply in ["", " \n\t "] {
        let result = run_single_call(text_body(reply)).await;
        assert!(!result.ok, "{reply:?} counted as a report");
        assert!(result.report.contains("no reply"), "{}", result.report);
    }
}

#[tokio::test]
async fn a_single_call_subagent_ignores_stray_tool_calls() {
    let text = serde_json::json!({"choices":[{"delta":{"content":"x = 2"}}]});
    let call = serde_json::json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":"call-1","function":{"name":"read_file","arguments":"{}"}}]}}]});
    let body = format!(
        "data: {text}\n\ndata: {call}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":10}}"#
    );
    let result = run_single_call(body).await;
    assert!(result.ok);
    assert_eq!(result.report, "x = 2");
}

/// Every event until the turn ends, answering permission requests with `answer` (and recording
/// which tools asked).
async fn run_turn(
    session: &Session,
    events: &mut broadcast::Receiver<AgentEvent>,
    answer: PermissionDecision,
) -> (Vec<SessionEvent>, Vec<String>) {
    let mut seen = Vec::new();
    let mut asked = Vec::new();
    loop {
        let event = next(events).await;
        if let SessionEvent::PermissionRequested {
            request_id, tool, ..
        } = &event
        {
            asked.push(tool.clone());
            session.respond(*request_id, answer).unwrap();
        }
        let done = matches!(
            event,
            SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
        );
        seen.push(event);
        if done {
            return (seen, asked);
        }
    }
}

fn finished(events: &[SessionEvent]) -> Vec<(bool, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolFinished { ok, output, .. } => Some((*ok, output.clone())),
            _ => None,
        })
        .collect()
}

/// Counts the sessions an agent asks to start.
#[derive(Debug, Default)]
struct CountingLauncher(AtomicUsize);

impl imperium_engine::SessionLauncher for CountingLauncher {
    fn launch<'a>(
        &'a self,
        _brief: &'a str,
        _model: Option<&'a str>,
    ) -> imperium_engine::LaunchFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok("launched".to_string()) })
    }
}

#[tokio::test]
async fn plan_mode_refuses_to_start_a_session() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("start_session", r#"{"brief":"write the release notes"}"#),
        text_body("Here is the plan."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let launcher = Arc::new(CountingLauncher::default());
    let mut config = config(endpoint, dir.path(), Workspace::new(dir.path()).unwrap());
    config.launcher = Some(Arc::clone(&launcher) as Arc<dyn imperium_engine::SessionLauncher>);
    let session = Session::spawn(config).unwrap();
    session.set_permission_mode(PermissionMode::Plan);
    let mut events = session.subscribe();
    session.send("plan the release".into()).unwrap();
    let (seen, asked) = run_turn(&session, &mut events, PermissionDecision::Allow).await;
    assert!(asked.is_empty(), "{asked:?}");
    let results = finished(&seen);
    assert!(
        !results[0].0 && results[0].1.contains("plan mode"),
        "{results:?}"
    );
    assert_eq!(
        launcher.0.load(Ordering::SeqCst),
        0,
        "no session was started"
    );
}

#[tokio::test]
async fn plan_mode_refuses_writes_and_commands_but_reads() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("write_file", r#"{"path":"plan.txt","content":"x"}"#),
        tool_call_body("run_command", r#"{"command":"echo ran > ran.txt"}"#),
        tool_call_body("read_file", r#"{"path":"notes.txt"}"#),
        text_body("Here is the plan."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "context").unwrap();
    // Even an allow rule for everything does not lift plan mode.
    let permissions = Permissions::new(
        rules(&[("*", "", RuleAction::Allow)]),
        SharedRules::default(),
    )
    .with_mode(PermissionMode::Plan);
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions);
    let session = start_in(endpoint, dir.path(), workspace);
    let mut events = session.subscribe();
    session.send("plan it".into()).unwrap();
    let (seen, asked) = run_turn(&session, &mut events, PermissionDecision::Allow).await;
    assert!(asked.is_empty(), "nothing asks in plan mode: {asked:?}");
    let results = finished(&seen);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(
        !results[0].0 && results[0].1.contains("plan mode"),
        "{results:?}"
    );
    assert!(
        !results[1].0 && results[1].1.contains("plan mode"),
        "{results:?}"
    );
    assert!(
        results[2].0 && results[2].1.contains("context"),
        "{results:?}"
    );
    assert!(!dir.path().join("plan.txt").exists());
    assert!(!dir.path().join("ran.txt").exists());
}

#[tokio::test]
async fn full_access_runs_without_asking_but_deny_rules_and_the_floor_hold() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("write_file", r#"{"path":"made.txt","content":"x"}"#),
        tool_call_body("write_file", r#"{"path":"blocked.txt","content":"x"}"#),
        tool_call_body("run_command", r#"{"command":"rm -rf made.txt"}"#),
        text_body("Done."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let permissions = Permissions::new(
        rules(&[("write_file", "blocked.txt", RuleAction::Deny)]),
        SharedRules::default(),
    );
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions);
    let session = start_in(endpoint, dir.path(), workspace);
    session.set_permission_mode(PermissionMode::Full);
    assert_eq!(session.permission_mode(), PermissionMode::Full);
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    let (seen, asked) = run_turn(&session, &mut events, PermissionDecision::Deny).await;
    // Only the destructive command asked (and was denied).
    assert_eq!(asked, ["run_command"]);
    let results = finished(&seen);
    assert!(results[0].0, "{results:?}");
    assert!(
        results[1].1.contains("permission rule denies"),
        "{results:?}"
    );
    assert!(dir.path().join("made.txt").exists());
    assert!(!dir.path().join("blocked.txt").exists());
}

#[tokio::test]
async fn the_safety_floor_asks_even_for_a_pre_approved_tool_in_an_unattended_session() {
    let endpoint = scripted_endpoint(vec![
        tool_call_body("run_command", r#"{"command":"echo ok > ran.txt"}"#),
        tool_call_body("run_command", r#"{"command":"rm -rf keep"}"#),
        text_body("Done."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("keep")).unwrap();
    std::fs::write(dir.path().join("keep").join("file.txt"), "x").unwrap();
    // As auto-fix starts its sessions: run_command pre-approved, nobody watching.
    let session = start_with(
        endpoint,
        dir.path(),
        Workspace::new(dir.path()).unwrap(),
        vec!["run_command".into()],
    );
    let mut events = session.subscribe();
    session.send("fix it".into()).unwrap();
    let (seen, asked) = run_turn(&session, &mut events, PermissionDecision::Deny).await;
    // The ordinary command ran without asking; the destructive one waited for approval.
    assert_eq!(asked, ["run_command"]);
    assert!(dir.path().join("ran.txt").exists());
    assert!(seen.iter().any(|event| matches!(event,
            SessionEvent::PermissionRequested { summary, .. } if summary.contains("rm -rf keep"))));
    assert!(dir.path().join("keep").join("file.txt").exists());
}

#[tokio::test]
async fn background_commands_are_killed_when_the_session_stops() {
    let slow = if cfg!(windows) {
        "ping -n 60 127.0.0.1"
    } else {
        "sleep 60"
    };
    let endpoint = scripted_endpoint(vec![
        tool_call_body(
            "run_command",
            &serde_json::json!({"command": slow, "background": true}).to_string(),
        ),
        text_body("Started."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    session.set_permission_mode(PermissionMode::Full);
    let mut events = session.subscribe();
    session.send("serve".into()).unwrap();
    let (seen, asked) = run_turn(&session, &mut events, PermissionDecision::Deny).await;
    assert!(asked.is_empty(), "{asked:?}");
    let results = finished(&seen);
    assert!(results[0].1.contains("started p1"), "{results:?}");
    let processes = session.workspace().processes().clone();
    assert_eq!(processes.running(), 1);
    session.stop();
    for _ in 0..100 {
        if processes.running() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(processes.running(), 0, "stopping the session kills it");
}

/// The personality joins the root agent's system prompt after its reply rules, and never a
/// subagent's.
#[tokio::test]
async fn only_the_root_prompt_carries_the_personality_after_the_reply_rules() {
    const PERSONALITY: &str = "Personality: speak as Marcus Aurelius. MARKER-7f3a";
    let root_calls = Arc::new(AtomicUsize::new(0));
    let sub_calls = Arc::new(AtomicUsize::new(0));
    let systems = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&systems);
    let (roots, subs) = (Arc::clone(&root_calls), Arc::clone(&sub_calls));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let (roots, subs, seen) = (Arc::clone(&roots), Arc::clone(&subs), Arc::clone(&seen));
            async move {
                let system = body["messages"][0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let reply = if system.starts_with("You are an Imperium subagent") {
                    subs.fetch_add(1, Ordering::SeqCst);
                    text_body("report")
                } else if system.starts_with("You are Imperium, a coding agent") {
                    match roots.fetch_add(1, Ordering::SeqCst) {
                        0 => tool_call_body("spawn_agent", r#"{"brief":"check docs"}"#),
                        1 => tool_call_body("await_tasks", "{}"),
                        _ => text_body("Done."),
                    }
                } else {
                    text_body("Title")
                };
                seen.lock().unwrap().push(system);
                ([("content-type", "text/event-stream")], reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let endpoint = Endpoint {
        base_url,
        api_key: None,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(endpoint, dir.path(), Workspace::new(dir.path()).unwrap());
    config.personality = Some(PERSONALITY.into());
    let session = Session::spawn(config).unwrap();
    let mut events = session.subscribe();
    session.send("check the docs".into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::TurnCompleted { .. } if event.agent_id == "root" => break,
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            _ => {}
        }
    }
    assert_eq!(sub_calls.load(Ordering::SeqCst), 1);
    let systems = systems.lock().unwrap().clone();
    let (root, others): (Vec<_>, Vec<_>) = systems
        .iter()
        .partition(|system| system.starts_with("You are Imperium, a coding agent"));
    assert_eq!(root.len(), 3);
    for system in root {
        let style = system
            .find("lead with the result")
            .expect("the reply rules");
        let personality = system.find(PERSONALITY).expect("the personality");
        assert!(
            style < personality,
            "the personality follows the reply rules"
        );
    }
    assert!(!others.is_empty());
    for system in others {
        assert!(!system.contains("MARKER-7f3a"), "{system}");
    }
}
