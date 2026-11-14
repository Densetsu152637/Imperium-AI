//! The harness aids for small models: the de-reference step, the session state, and
//! `request_clarification`, against a fake endpoint.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_config::{Dereference, HarnessSettings};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::clarification::{ClarificationCase, Observed, Verdict, score};
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

fn call_body(name: &str, arguments: &str) -> String {
    let calls =
        serde_json::json!([{"index":0,"id":"c1","function":{"name":name,"arguments":arguments}}]);
    let chunk = serde_json::json!({"choices":[{"delta":{"tool_calls":calls}}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// What the fake endpoint saw.
#[derive(Default)]
struct Seen {
    /// The request bodies of the root agent's calls.
    root: Mutex<Vec<serde_json::Value>>,
    /// How many de-reference calls came.
    rewrites: AtomicUsize,
}

impl Seen {
    fn root_bodies(&self) -> Vec<serde_json::Value> {
        self.root.lock().unwrap().clone()
    }
}

/// The root's replies, in order (`call:<tool>:<arguments>` makes a tool call; the last one
/// repeats), and what the de-reference step answers.
async fn endpoint(script: Vec<String>, rewrite: &str, seen: Arc<Seen>) -> Endpoint {
    let rewrite = rewrite.to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let (script, rewrite, seen, calls) = (
                script.clone(),
                rewrite.clone(),
                Arc::clone(&seen),
                Arc::clone(&calls),
            );
            async move {
                let system = body["messages"][0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let reply = if system.contains("standalone prompt") {
                    seen.rewrites.fetch_add(1, Ordering::SeqCst);
                    text_body(&rewrite)
                } else {
                    seen.root.lock().unwrap().push(body.clone());
                    let step = calls.fetch_add(1, Ordering::SeqCst).min(script.len() - 1);
                    match script[step].strip_prefix("call:") {
                        Some(rest) => {
                            let (name, arguments) = rest.split_once(':').unwrap();
                            call_body(name, arguments)
                        }
                        None => text_body(&script[step]),
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

fn start(endpoint: Endpoint, dir: &std::path::Path, harness: HarnessSettings) -> Session {
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
        max_steps: 8,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        harness,
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

/// Sends `text` and returns the root's events until its turn completes.
async fn turn(
    session: &Session,
    events: &mut broadcast::Receiver<AgentEvent>,
    text: &str,
) -> Vec<SessionEvent> {
    session.send(text.into()).unwrap();
    let mut seen = Vec::new();
    loop {
        let (agent, event) = next(events).await;
        if agent != "root" {
            continue;
        }
        match &event {
            SessionEvent::TurnCompleted { .. } => {
                seen.push(event);
                return seen;
            }
            SessionEvent::TurnFailed { message } => panic!("the turn failed: {message}"),
            _ => seen.push(event),
        }
    }
}

fn rewriting() -> HarnessSettings {
    HarnessSettings {
        dereference: Dereference::On,
        ..Default::default()
    }
}

/// The text of the last user message in a request body.
fn last_user(body: &serde_json::Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Every message of a request except the system prompt, as JSON text.
fn all_text(body: &serde_json::Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(ToString::to_string)
        .collect()
}

#[tokio::test]
async fn the_planner_reads_the_rewritten_prompt_and_the_state_while_the_transcript_keeps_the_original()
 {
    let seen = Arc::new(Seen::default());
    let rewrite = r#"{"prompt":"Change the timeout in src/main.rs","state":{"entities":[{"kind":"file","name":"src/main.rs","note":"entry point"}]}}"#;
    let script = vec![
        "Opened it.".to_string(),
        r#"call:update_state:{"goals":["tune the timeout"]}"#.to_string(),
        "Done.".to_string(),
    ];
    let endpoint = endpoint(script, rewrite, Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path(), rewriting());
    let mut events = session.subscribe();

    turn(&session, &mut events, "open src/main.rs").await;
    // Nothing to resolve against yet: no rewrite.
    assert_eq!(seen.rewrites.load(Ordering::SeqCst), 0);

    let second = turn(&session, &mut events, "change the timeout in that file").await;
    assert_eq!(seen.rewrites.load(Ordering::SeqCst), 1);
    // The transcript holds what the user wrote; a notice says how it was understood.
    assert!(second.iter().any(|event| matches!(
        event,
        SessionEvent::TurnStarted { text, .. } if text == "change the timeout in that file"
    )));
    assert!(second.iter().any(|event| matches!(
        event,
        SessionEvent::Notice { message } if message.contains("Change the timeout in src/main.rs")
    )));

    let bodies = seen.root_bodies();
    assert_eq!(bodies.len(), 3);
    let planner = last_user(&bodies[1]);
    assert!(planner.starts_with("<session-state>\n{"), "{planner}");
    assert!(planner.contains(r#""name":"src/main.rs""#));
    assert!(
        planner
            .contains("<resolved-request>\nChange the timeout in src/main.rs\n</resolved-request>")
    );
    assert!(planner.contains("<original-message>\nchange the timeout in that file\n"));
    assert!(planner.contains("<ambiguity-hints>"));

    // update_state rewrites the one state block in place, so the next call reads the change.
    let after = all_text(&bodies[2]);
    assert_eq!(after.matches("<session-state>").count(), 1);
    assert!(after.contains("tune the timeout"));
    let tool_result = second.iter().find_map(|event| match event {
        SessionEvent::ToolFinished { output, .. } => Some(output.clone()),
        _ => None,
    });
    assert!(tool_result.unwrap().contains("tune the timeout"));

    // The state is recorded in the saved conversation, which is how a resumed session
    // gets it back.
    let saved = std::fs::read_to_string(dir.path().join(".journal/test.messages.json")).unwrap();
    assert!(saved.contains("tune the timeout"));
    assert!(saved.contains("src/main.rs"));
}

#[tokio::test]
async fn an_unreadable_rewrite_leaves_the_message_as_it_was() {
    let seen = Arc::new(Seen::default());
    let endpoint = endpoint(vec!["Ok.".into()], "I cannot do that.", Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path(), rewriting());
    let mut events = session.subscribe();

    turn(&session, &mut events, "open src/main.rs").await;
    turn(&session, &mut events, "now run it").await;
    assert_eq!(seen.rewrites.load(Ordering::SeqCst), 1);
    let message = last_user(&seen.root_bodies()[1]);
    assert_eq!(message, "now run it");
}

#[tokio::test]
async fn hosted_models_are_not_rewritten_by_default() {
    let seen = Arc::new(Seen::default());
    let endpoint = endpoint(vec!["Ok.".into()], "{}", Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    // Auto, and the fake model is not local.
    let session = start(endpoint, dir.path(), HarnessSettings::default());
    let mut events = session.subscribe();

    turn(&session, &mut events, "open src/main.rs").await;
    turn(&session, &mut events, "now run it").await;
    assert_eq!(seen.rewrites.load(Ordering::SeqCst), 0);
    assert_eq!(last_user(&seen.root_bodies()[1]), "now run it");
}

#[tokio::test]
async fn the_state_can_be_switched_off() {
    let seen = Arc::new(Seen::default());
    let script = vec![
        r#"call:update_state:{"goals":["x"]}"#.to_string(),
        "Ok.".to_string(),
    ];
    let endpoint = endpoint(script, "{}", Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    let harness = HarnessSettings {
        state: false,
        ..Default::default()
    };
    let session = start(endpoint, dir.path(), harness);
    let mut events = session.subscribe();

    turn(&session, &mut events, "hello").await;
    let body = seen.root_bodies()[0].to_string();
    assert!(!body.contains("update_state\""), "the tool is not offered");
    assert!(!body.contains("<session-state>"));
}

/// Waits for the clarification prompt and returns its request id and options.
async fn clarification_prompt(
    events: &mut broadcast::Receiver<AgentEvent>,
    seen: &mut Vec<SessionEvent>,
) -> (u64, Vec<String>) {
    loop {
        let (agent, event) = next(events).await;
        if agent != "root" {
            continue;
        }
        let asked = match &event {
            SessionEvent::QuestionsAsked {
                request_id,
                questions,
            } => Some((*request_id, questions[0].options.clone())),
            SessionEvent::TurnFailed { message } => panic!("the turn failed: {message}"),
            _ => None,
        };
        seen.push(event);
        if let Some(asked) = asked {
            return asked;
        }
    }
}

#[tokio::test]
async fn request_clarification_waits_for_the_answer_and_scores_well_on_an_ambiguous_case() {
    let seen = Arc::new(Seen::default());
    let script = vec![
        r#"call:request_clarification:{"missing_parameter":"which file","options":["src/main.rs","src/lib.rs"],"confidence":0.3}"#.to_string(),
        "Changed src/lib.rs.".to_string(),
    ];
    let endpoint = endpoint(script, "{}", Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path(), HarnessSettings::default());
    let mut events = session.subscribe();

    session
        .send("open that file and change the timeout".into())
        .unwrap();
    let mut observed_events = Vec::new();
    let (request_id, options) = clarification_prompt(&mut events, &mut observed_events).await;
    assert_eq!(options, ["src/main.rs", "src/lib.rs"]);
    // The root makes no further model call while it waits.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(seen.root_bodies().len(), 1);

    session
        .answer(request_id, vec!["src/lib.rs".into()])
        .unwrap();
    loop {
        let (agent, event) = next(&mut events).await;
        if agent != "root" {
            continue;
        }
        let done = matches!(event, SessionEvent::TurnCompleted { .. });
        if let SessionEvent::ToolFinished { output, .. } = &event {
            assert!(output.contains("which file = src/lib.rs"), "{output}");
            assert!(output.contains("0.30"));
        }
        observed_events.push(event);
        if done {
            break;
        }
    }

    // The evaluation reads the same events and rewards the clarification.
    let case = ClarificationCase {
        id: "that-file".into(),
        context: Vec::new(),
        message: "open that file and change the timeout".into(),
        ambiguous: true,
        missing: Some("file".into()),
        options: vec!["src/lib.rs".into()],
    };
    let observed = Observed::from_events(&observed_events);
    assert!(observed.asked);
    let scored = score(&case, &observed);
    assert_eq!(scored.verdict, Verdict::CorrectAsk);
    assert!((scored.reward - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn a_blank_clarification_answer_is_not_taken_as_a_guess() {
    let seen = Arc::new(Seen::default());
    let script = vec![
        r#"call:request_clarification:{"missing_parameter":"which branch","options":["main","dev"]}"#.to_string(),
        "Waiting.".to_string(),
    ];
    let endpoint = endpoint(script, "{}", Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(endpoint, dir.path(), HarnessSettings::default());
    let mut events = session.subscribe();

    session.send("merge it".into()).unwrap();
    let mut collected = Vec::new();
    let (request_id, _) = clarification_prompt(&mut events, &mut collected).await;
    session.answer(request_id, vec![String::new()]).unwrap();
    loop {
        let (agent, event) = next(&mut events).await;
        if agent != "root" {
            continue;
        }
        match event {
            SessionEvent::ToolFinished { output, .. } => {
                assert!(output.contains("still unknown"), "{output}");
                assert!(output.contains("Do not guess"));
                return;
            }
            SessionEvent::TurnFailed { message } => panic!("the turn failed: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn the_root_prompt_tells_it_when_to_ask() {
    let seen = Arc::new(Seen::default());
    let endpoint = endpoint(vec!["Ok.".into()], "{}", Arc::clone(&seen)).await;
    let dir = tempfile::tempdir().unwrap();
    let harness = HarnessSettings {
        clarify_below: 0.6,
        ..Default::default()
    };
    let session = start(endpoint, dir.path(), harness);
    let mut events = session.subscribe();
    turn(&session, &mut events, "hello").await;
    let body = &seen.root_bodies()[0];
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("below 60%"), "{system}");
    assert!(system.contains("request_clarification"));
    assert!(system.contains("<session-state>"));
}
