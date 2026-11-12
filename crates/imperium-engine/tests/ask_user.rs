use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SessionEvent};
use imperium_tools::Workspace;
use tokio::sync::broadcast;

fn text_body(text: &str) -> String {
    let chunk = serde_json::json!({"choices":[{"delta":{"content":text}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":10}}"#
    )
}

fn calls_body(calls: &[(&str, &str, String)]) -> String {
    let calls: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(index, (id, name, arguments))| {
            serde_json::json!({"index":index,"id":id,"function":{"name":name,"arguments":arguments}})
        })
        .collect();
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":calls}}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn questions() -> String {
    serde_json::json!({"questions":[{
        "question": "Which database?",
        "options": ["sqlite", "postgres"],
        "recommended": "sqlite, because it needs no server"
    }]})
    .to_string()
}

/// A fake endpoint: the root spawns a slow subagent and asks the user in the same step, then
/// waits for the subagent and finishes. Subagents answer after `delay`.
async fn endpoint(
    root_calls: Arc<AtomicUsize>,
    sub_calls: Arc<AtomicUsize>,
    delay: Duration,
) -> Endpoint {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let root_calls = Arc::clone(&root_calls);
            let sub_calls = Arc::clone(&sub_calls);
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                let reply = if system.contains("subagent") {
                    tokio::time::sleep(delay).await;
                    sub_calls.fetch_add(1, Ordering::SeqCst);
                    text_body("slow work done")
                } else {
                    match root_calls.fetch_add(1, Ordering::SeqCst) {
                        0 => calls_body(&[
                            ("a", "spawn_agent", r#"{"brief":"slow work"}"#.into()),
                            ("b", "ask_user", questions()),
                        ]),
                        1 => calls_body(&[("c", "await_tasks", "{}".into())]),
                        _ => text_body("All done."),
                    }
                };
                ([("content-type", "text/event-stream")], reply)
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

fn start(endpoint: Endpoint, dir: &std::path::Path) -> Session {
    Session::spawn(SessionConfig {
        id: "test".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint,
            model: "model".into(),
            local: false,
            context_window: None,
            kind: "openai".into(),
            reasoning: None,
            pricing: None,
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
    })
    .unwrap()
}

async fn next(events: &mut broadcast::Receiver<AgentEvent>) -> (String, SessionEvent) {
    let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("an event within 10 s")
        .unwrap();
    (event.agent_id, event.event)
}

/// Sends the first message and runs until the root has asked and the subagent has finished;
/// returns the request id of the batch.
async fn run_to_the_question(
    session: &Session,
    events: &mut broadcast::Receiver<AgentEvent>,
) -> u64 {
    session.send("do the slow work and ask me".into()).unwrap();
    let mut asked = None;
    let mut finished = false;
    while asked.is_none() || !finished {
        match next(events).await {
            (
                _,
                SessionEvent::QuestionsAsked {
                    request_id,
                    questions,
                },
            ) => {
                assert_eq!(questions[0].question, "Which database?");
                assert_eq!(questions[0].options, ["sqlite", "postgres"]);
                asked = Some(request_id);
            }
            (_, SessionEvent::TaskFinished { ok, .. }) => {
                assert!(ok);
                finished = true;
            }
            (agent, SessionEvent::TurnFailed { message }) => panic!("{agent} failed: {message}"),
            _ => {}
        }
    }
    asked.unwrap()
}

#[tokio::test]
async fn ask_user_pauses_the_root_without_model_calls_while_subagents_keep_running() {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let sub_calls = Arc::new(AtomicUsize::new(0));
    let endpoint = endpoint(
        Arc::clone(&root_calls),
        Arc::clone(&sub_calls),
        Duration::from_millis(200),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();

    let request_id = run_to_the_question(&session, &mut events).await;
    // The subagent finished while the root waited, and the root made no further model call.
    assert_eq!(sub_calls.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(root_calls.load(Ordering::SeqCst), 1);

    session.answer(request_id, vec!["postgres".into()]).unwrap();
    let mut answer_seen = false;
    loop {
        match next(&mut events).await {
            (_, SessionEvent::QuestionsAnswered { request_id: id }) => {
                assert_eq!(id, request_id);
            }
            (agent, SessionEvent::ToolFinished { output, .. })
                if agent == "root" && output.contains("Which database?") =>
            {
                assert!(output.contains("A1: postgres"));
                answer_seen = true;
            }
            (agent, SessionEvent::TurnCompleted { steps, .. }) if agent == "root" => {
                assert_eq!(steps, 3);
                break;
            }
            (agent, SessionEvent::TurnFailed { message }) => panic!("{agent} failed: {message}"),
            _ => {}
        }
    }
    assert!(answer_seen);
    assert_eq!(root_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn ask_user_with_a_blank_answer_takes_the_recommendation() {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let sub_calls = Arc::new(AtomicUsize::new(0));
    let endpoint = endpoint(root_calls, sub_calls, Duration::ZERO).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path());
    let mut events = session.subscribe();

    let request_id = run_to_the_question(&session, &mut events).await;
    session.answer(request_id, vec![String::new()]).unwrap();
    loop {
        match next(&mut events).await {
            (agent, SessionEvent::ToolFinished { output, .. })
                if agent == "root" && output.contains("Which database?") =>
            {
                assert!(output.contains("go with your recommendation: sqlite, because"));
                return;
            }
            (agent, SessionEvent::TurnFailed { message }) => panic!("{agent} failed: {message}"),
            _ => {}
        }
    }
}
