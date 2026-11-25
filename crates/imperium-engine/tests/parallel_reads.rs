//! Read-only calls of one model step run at the same time (roadmap R2.9, `harness.parallel_reads`)
//! and their results go back in call order. The overlap itself is proved in the unit tests of
//! `agent/parallel.rs` (a barrier only three overlapping jobs pass); these tests check what the
//! model and the clients see: the same results and events as with the setting off, writes and
//! asks keeping their place in the order.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_api::{PermissionRule, RuleAction};
use imperium_config::HarnessSettings;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_policy::permissions::{Permissions, SharedRules};
use imperium_protocol::session::{AgentEvent, PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// A fake model: its first reply makes `calls` (id, tool, arguments) in one step, its second
/// is text. Every request body is kept.
struct Fake {
    calls: Vec<(String, String, String)>,
    requests: Mutex<Vec<Value>>,
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let fake = Arc::clone(&fake);
            async move {
                let first = {
                    let mut requests = fake.requests.lock().unwrap();
                    requests.push(body);
                    requests.len() == 1
                };
                let reply = if first {
                    let calls: Vec<Value> = fake
                        .calls
                        .iter()
                        .enumerate()
                        .map(|(index, (id, name, arguments))| {
                            json!({"index": index, "id": id,
                                "function": {"name": name, "arguments": arguments}})
                        })
                        .collect();
                    sse(json!({"choices": [{"delta": {"tool_calls": calls}}]}))
                } else {
                    sse(json!({"choices": [{"delta": {"content": "done"}}]}))
                };
                ([("content-type", "text/event-stream")], reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn start(url: &str, workspace: Workspace, parallel_reads: u32) -> Session {
    let dir = workspace.root().to_path_buf();
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace,
        model: Some(ResolvedModel {
            spec: "a:model".into(),
            endpoint: Endpoint {
                base_url: url.into(),
                api_key: None,
            },
            model: "model".into(),
            pricing: None,
            local: false,
            context_window: Some(100_000),
            kind: "openai".into(),
            reasoning: None,
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
        max_steps: 20,
        auto_compact: false,
        auto_compact_at: None,
        attended: false,
        attachments: Default::default(),
        auto_permissions: Default::default(),
        harness: HarnessSettings {
            parallel_reads,
            ..HarnessSettings::default()
        },
        background: None,
        checkpoints: None,
        budget_gate: None,
        keep_agent_state: true,
        effort: Default::default(),
        knowledge: Default::default(),
        journal_path: dir.join(".sessions/s.jsonl"),
    })
    .unwrap()
}

async fn next(events: &mut broadcast::Receiver<AgentEvent>) -> SessionEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap()
            .event;
        assert!(
            !matches!(event, SessionEvent::TurnFailed { .. }),
            "turn failed: {event:?}"
        );
        if !matches!(
            event,
            SessionEvent::StatusChanged { .. } | SessionEvent::ContextBreakdown { .. }
        ) {
            return event;
        }
    }
}

/// Events up to and including the end of the turn.
async fn to_the_end(events: &mut broadcast::Receiver<AgentEvent>) -> Vec<SessionEvent> {
    let mut seen = Vec::new();
    loop {
        let event = next(events).await;
        let done = matches!(event, SessionEvent::TurnCompleted { .. });
        seen.push(event);
        if done {
            return seen;
        }
    }
}

fn call(id: &str, tool: &str, arguments: Value) -> (String, String, String) {
    (id.into(), tool.into(), arguments.to_string())
}

fn files(dir: &std::path::Path) {
    std::fs::write(dir.join("a.txt"), "alpha").unwrap();
    std::fs::write(dir.join("b.txt"), "bravo").unwrap();
    std::fs::write(dir.join("c.txt"), "charlie").unwrap();
    std::fs::write(dir.join("secret.txt"), "sesame").unwrap();
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

/// The tool messages the second request carries, in order, as (call id, content).
fn tool_messages(request: &Value) -> Vec<(String, String)> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| {
            (
                message["tool_call_id"].as_str().unwrap().to_string(),
                message["content"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Each call id is announced once and finished once, the start before the finish; the finishes
/// come in the order of `ids`.
fn assert_well_formed(events: &[SessionEvent], ids: &[&str]) {
    let position = |wanted: &str, finished: bool| {
        let found: Vec<usize> = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                SessionEvent::ToolStarted { call_id, .. } if !finished && call_id == wanted => {
                    Some(index)
                }
                SessionEvent::ToolFinished { call_id, .. } if finished && call_id == wanted => {
                    Some(index)
                }
                _ => None,
            })
            .collect();
        assert_eq!(found.len(), 1, "{wanted} finished={finished}: {events:?}");
        found[0]
    };
    let mut last_finish = 0;
    for id in ids {
        let (started, finished) = (position(id, false), position(id, true));
        assert!(started < finished, "{id} finished before it started");
        assert!(finished > last_finish, "{id} finished out of call order");
        last_finish = finished;
    }
}

/// `body` with the directory's unique name replaced, so two runs in different directories can
/// be compared (the rest of the path is the same however the system spells it).
fn without_dir(body: &Value, dir: &std::path::Path) -> Value {
    let name = dir.file_name().unwrap().to_string_lossy().to_string();
    serde_json::from_str(&body.to_string().replace(&name, "<dir>")).unwrap()
}

/// Runs one step of `calls` and returns the first and second requests (the directory's path
/// replaced) and the turn's events.
async fn run(
    calls: Vec<(String, String, String)>,
    parallel_reads: u32,
    permissions: Option<Permissions>,
) -> (Vec<Value>, Vec<SessionEvent>) {
    let fake = Arc::new(Fake {
        calls,
        requests: Mutex::new(Vec::new()),
    });
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    files(dir.path());
    let mut workspace = Workspace::new(dir.path()).unwrap();
    if let Some(permissions) = permissions {
        workspace = workspace.with_permissions(permissions);
    }
    let session = start(&url, workspace, parallel_reads);
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    let seen = to_the_end(&mut events).await;
    let requests = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|body| without_dir(body, dir.path()))
        .collect();
    (requests, seen)
}

#[tokio::test]
async fn reads_of_a_step_come_back_in_call_order_exactly_as_one_at_a_time() {
    let calls = || {
        vec![
            call("c1", "read_file", json!({"path": "c.txt"})),
            call("c2", "read_file", json!({"path": "a.txt"})),
            call("c3", "read_file", json!({"path": "missing.txt"})),
            call("c4", "read_file", json!({"path": "b.txt"})),
        ]
    };
    let (one_at_a_time, _) = run(calls(), 0, None).await;
    let (together, events) = run(calls(), 4, None).await;

    let results = tool_messages(&together[1]);
    let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2", "c3", "c4"]);
    assert!(results[0].1.contains("charlie"), "{results:?}");
    assert!(results[1].1.contains("alpha"), "{results:?}");
    assert!(results[2].1.starts_with("error:"), "{results:?}");
    assert!(results[3].1.contains("bravo"), "{results:?}");
    assert_well_formed(&events, &["c1", "c2", "c3", "c4"]);
    // What the model is sent, the system prompt and the tools included, is the same with the
    // setting off.
    assert_eq!(together, one_at_a_time);
}

#[tokio::test]
async fn a_read_after_a_write_sees_the_write() {
    let calls = vec![
        call("r1", "read_file", json!({"path": "a.txt"})),
        call(
            "w1",
            "write_file",
            json!({"path": "a.txt", "content": "rewritten"}),
        ),
        call("r2", "read_file", json!({"path": "a.txt"})),
        call("r3", "read_file", json!({"path": "b.txt"})),
    ];
    let permissions = Permissions::new(rules(&[]), rules(&[("write_file", "", RuleAction::Allow)]));
    let (requests, events) = run(calls, 4, Some(permissions)).await;

    let results = tool_messages(&requests[1]);
    let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["r1", "w1", "r2", "r3"]);
    assert!(results[0].1.contains("alpha"), "{results:?}");
    assert!(results[2].1.contains("rewritten"), "{results:?}");
    assert!(!results[2].1.contains("alpha"), "{results:?}");
    assert_well_formed(&events, &["r1", "w1", "r2", "r3"]);
}

#[tokio::test]
async fn a_read_that_asks_does_not_run_before_its_approval() {
    let calls = vec![
        call("r1", "read_file", json!({"path": "a.txt"})),
        call("r2", "read_file", json!({"path": "secret.txt"})),
        call("r3", "read_file", json!({"path": "b.txt"})),
        call("r4", "read_file", json!({"path": "c.txt"})),
    ];
    let fake = Arc::new(Fake {
        calls,
        requests: Mutex::new(Vec::new()),
    });
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    files(dir.path());
    let permissions = Permissions::new(
        rules(&[]),
        rules(&[("read_file", "secret.txt", RuleAction::Ask)]),
    );
    let workspace = Workspace::new(dir.path())
        .unwrap()
        .with_permissions(permissions);
    let session = start(&url, workspace, 4);
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();

    // Everything up to the question: the first read is done, and nothing after the asking
    // read has been announced or finished.
    let mut before = Vec::new();
    let request_id = loop {
        match next(&mut events).await {
            SessionEvent::PermissionRequested {
                request_id, tool, ..
            } => {
                assert_eq!(tool, "read_file");
                break request_id;
            }
            event => before.push(event),
        }
    };
    for event in &before {
        if let SessionEvent::ToolStarted { call_id, .. }
        | SessionEvent::ToolFinished { call_id, .. } = event
        {
            assert_eq!(call_id, "r1", "{call_id} ran before the approval");
        }
    }
    assert_eq!(fake.requests.lock().unwrap().len(), 1);

    session
        .respond(request_id, PermissionDecision::Allow)
        .unwrap();
    let mut after = to_the_end(&mut events).await;
    before.append(&mut after);
    assert_well_formed(&before, &["r1", "r2", "r3", "r4"]);
    let results = tool_messages(&fake.requests.lock().unwrap()[1]);
    let ids: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["r1", "r2", "r3", "r4"]);
    assert!(results[1].1.contains("sesame"), "{results:?}");
    assert!(results[3].1.contains("charlie"), "{results:?}");
}

#[tokio::test]
async fn a_denied_read_leaves_the_others_of_its_step_alone() {
    let calls = vec![
        call("r1", "read_file", json!({"path": "a.txt"})),
        call("r2", "read_file", json!({"path": "secret.txt"})),
        call("r3", "read_file", json!({"path": "b.txt"})),
    ];
    let permissions = Permissions::new(
        rules(&[("read_file", "secret.txt", RuleAction::Deny)]),
        rules(&[]),
    );
    let (requests, events) = run(calls, 4, Some(permissions)).await;

    let results = tool_messages(&requests[1]);
    assert!(results[0].1.contains("alpha"), "{results:?}");
    assert!(
        results[1].1.contains("permission rule denies"),
        "{results:?}"
    );
    assert!(!results[1].1.contains("sesame"), "{results:?}");
    assert!(results[2].1.contains("bravo"), "{results:?}");
    // The refused call is only finished (it never started); the others keep their order.
    let finished: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolFinished { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(finished, ["r1", "r2", "r3"]);
}
