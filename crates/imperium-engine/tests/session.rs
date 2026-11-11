use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::{Endpoint, Pricing};
use imperium_protocol::session::{AgentEvent, PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use imperium_tools::permissions::{PermissionRule, Permissions, RuleAction, SharedRules};
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
    Session::spawn(SessionConfig {
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
        max_steps: 5,
        background: None,
        checkpoints: None,
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".journal/test.jsonl"),
    })
    .unwrap()
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
