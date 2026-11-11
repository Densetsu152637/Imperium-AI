//! Hot-swapping a session's model: selecting it only records the switch (nothing is called);
//! the next turn goes to the new endpoint with the same conversation; a smaller context window
//! compacts older turns when that turn starts (summarised by the model switched from), keeping
//! the latest turn and its tool calls verbatim; a window nothing fits fails that turn and the
//! session can switch again; and a resumed session keeps the model it switched to.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig, SwitchError, journalled_model};
use imperium_models::Endpoint;
use imperium_protocol::session::{AgentEvent, SessionEvent, SessionStatus, TokenCountMethod};
use imperium_tools::Workspace;
use serde_json::{Value, json};

const SUMMARY: &str = "Summary: the user sent two long alpha reports; nothing is open.";

/// A fake OpenAI-compatible endpoint that records every request. It answers summary requests
/// with [`SUMMARY`], a message asking to read a.txt with a `read_file` call, a message saying
/// "slow" after a pause, and anything else with "done".
#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<Value>>,
}

impl Fake {
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    /// Requests other than summaries.
    fn turns(&self) -> Vec<Value> {
        self.requests()
            .into_iter()
            .filter(|body| !is_summary(body))
            .collect()
    }

    fn summaries(&self) -> Vec<Value> {
        self.requests().into_iter().filter(is_summary).collect()
    }
}

fn is_summary(body: &Value) -> bool {
    body["messages"][0]["content"]
        .as_str()
        .is_some_and(|system| system.starts_with("You summarise"))
}

fn stream(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn text(content: &str) -> String {
    stream(json!({"choices": [{"delta": {"content": content}}]}))
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let fake = Arc::clone(&fake);
            async move {
                fake.requests.lock().unwrap().push(body.clone());
                let last = body["messages"]
                    .as_array()
                    .and_then(|messages| messages.last())
                    .cloned()
                    .unwrap_or_default();
                let from_user = last["role"] == "user";
                let said = last["content"].as_str().unwrap_or_default().to_string();
                let reply = if is_summary(&body) {
                    text(SUMMARY)
                } else if from_user && said.ends_with("read a.txt") {
                    stream(json!({"choices": [{"delta": {"tool_calls": [{"index": 0,
                        "id": "call-1", "function": {"name": "read_file",
                        "arguments": "{\"path\":\"a.txt\"}"}}]}}]}))
                } else if from_user && said.ends_with("slow") {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    text("done slowly")
                } else {
                    text("done")
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

fn model(spec: &str, url: &str, context_window: Option<u64>) -> ResolvedModel {
    ResolvedModel {
        spec: spec.into(),
        endpoint: Endpoint {
            base_url: url.into(),
            api_key: None,
        },
        model: spec.split_once(':').unwrap().1.into(),
        pricing: None,
        local: false,
        context_window,
        kind: "openai".into(),
        reasoning: None,
    }
}

fn start(model: ResolvedModel, dir: &std::path::Path, resume: bool) -> Session {
    start_with(Some(model), dir, resume, false)
}

fn start_with(
    model: Option<ResolvedModel>,
    dir: &std::path::Path,
    resume: bool,
    auto_compact: bool,
) -> Session {
    Session::spawn(SessionConfig {
        id: "s".into(),
        workspace: Workspace::new(dir).unwrap(),
        model,
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
        resume,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 5,
        auto_compact,
        background: None,
        checkpoints: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".sessions/s.jsonl"),
    })
    .unwrap()
}

/// Sends a message and returns the turn's events once it completes.
async fn turn(session: &Session, text: &str) -> Vec<SessionEvent> {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut seen = Vec::new();
    loop {
        let AgentEvent { event, .. } = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        let done = matches!(event, SessionEvent::TurnCompleted { .. });
        assert!(
            !matches!(event, SessionEvent::TurnFailed { .. }),
            "turn failed: {event:?}"
        );
        seen.push(event);
        if done {
            return seen;
        }
    }
}

fn history(session: &Session) -> Vec<SessionEvent> {
    session
        .subscribe_with_history()
        .0
        .into_iter()
        .map(|event| event.event)
        .collect()
}

fn contents(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_large_window_switches_without_compacting_and_the_next_turn_uses_it() {
    let (a, b) = (Arc::new(Fake::default()), Arc::new(Fake::default()));
    let (url_a, url_b) = (serve(Arc::clone(&a)).await, serve(Arc::clone(&b)).await);
    let dir = tempfile::tempdir().unwrap();
    let session = start(model("a:old", &url_a, Some(1_000_000)), dir.path(), false);
    turn(&session, "remember the number 7").await;

    // While a turn runs, the switch is refused and nothing changes.
    let mut events = session.subscribe();
    session.send("go slow".into()).unwrap();
    while !matches!(
        events.recv().await.unwrap().event,
        SessionEvent::TurnStarted { .. }
    ) {}
    let busy = session
        .set_model(model("b:new", &url_b, Some(1_000_000)))
        .await;
    assert_eq!(busy, Err(SwitchError::Busy));
    while !matches!(
        events.recv().await.unwrap().event,
        SessionEvent::TurnCompleted { .. }
    ) {}
    assert_eq!(session.model().as_deref(), Some("a:old"));

    let fit = session
        .set_model(model("b:new", &url_b, Some(1_000_000)))
        .await
        .unwrap();
    assert!(fit.compaction.is_none());
    assert_eq!(fit.context_window, Some(1_000_000));
    assert_eq!(fit.method, TokenCountMethod::Estimated);
    assert!(fit.tokens > 0 && fit.tokens <= fit.budget.unwrap());
    assert_eq!(session.model().as_deref(), Some("b:new"));
    assert!(history(&session).contains(&SessionEvent::ModelChanged {
        from: "a:old".into(),
        to: "b:new".into(),
        compacted: false,
    }));

    turn(&session, "what was the number?").await;
    assert_eq!(a.turns().len(), 2);
    let sent = b.turns();
    assert_eq!(sent.len(), 1);
    // The whole conversation went to the new endpoint, with its own model name.
    assert_eq!(sent[0]["model"], "new");
    let said = contents(&sent[0]);
    assert_eq!(said.len(), 6);
    assert_eq!(said[1], "remember the number 7");
    assert_eq!(said[5], "what was the number?");
    assert!(a.summaries().is_empty() && b.summaries().is_empty());
}

#[tokio::test]
async fn a_small_window_compacts_older_turns_and_keeps_the_latest_verbatim() {
    let (a, b) = (Arc::new(Fake::default()), Arc::new(Fake::default()));
    let (url_a, url_b) = (serve(Arc::clone(&a)).await, serve(Arc::clone(&b)).await);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "the contents of a").unwrap();
    let session = start(model("a:old", &url_a, None), dir.path(), false);

    let report = "alpha ".repeat(8_000);
    let first = turn(&session, &format!("first report: {report}")).await;
    turn(&session, &format!("second report: {report}")).await;
    turn(&session, "now read a.txt").await;

    // What the instructions and tools take, from the first call's breakdown.
    let base: i64 = first
        .iter()
        .find_map(|event| match event {
            SessionEvent::ContextBreakdown { categories, .. } => Some(
                categories
                    .iter()
                    .filter(|c| c.key.starts_with("system_") || c.key.starts_with("tools_"))
                    .map(|c| c.tokens)
                    .sum(),
            ),
            _ => None,
        })
        .unwrap();
    // Room for the instructions, a summary, and the last turn, but not the reports.
    let window = (u64::try_from(base).unwrap() + 3_000) * 2;
    let fit = session
        .set_model(model("b:small", &url_b, Some(window)))
        .await
        .unwrap();
    // Selecting the model only estimates: the conversation is over the budget, and nothing
    // is summarised or sent yet.
    assert!(fit.compaction.is_none());
    assert_eq!(fit.method, TokenCountMethod::Estimated);
    assert!(fit.tokens > fit.budget.unwrap());
    assert!(a.summaries().is_empty() && b.requests().is_empty());
    let events = history(&session);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, SessionEvent::ContextCompacted { .. }))
    );
    assert!(events.contains(&SessionEvent::ModelChanged {
        from: "a:old".into(),
        to: "b:small".into(),
        compacted: false,
    }));

    // The next message compacts first: the two report turns (user and reply each) are
    // summarised, by the model switched from.
    let next = turn(&session, "anything else?").await;
    let (before, after, budget, summary_model) = next
        .iter()
        .find_map(|event| match event {
            SessionEvent::ContextCompacted {
                model,
                before_tokens,
                after_tokens,
                summarised_messages: 4,
                budget,
                summary_model,
                ..
            } if model == "b:small" => Some((
                *before_tokens,
                *after_tokens,
                *budget,
                summary_model.clone(),
            )),
            _ => None,
        })
        .expect("the conversation was compacted when the turn started");
    assert!(before > budget && after <= budget);
    assert_eq!(summary_model, "a:old");
    let summaries = a.summaries();
    assert_eq!(summaries.len(), 1);
    assert!(contents(&summaries[0])[1].contains("first report: alpha"));
    assert!(b.summaries().is_empty());
    // The compacted conversation is saved for a restart.
    let saved = std::fs::read_to_string(dir.path().join(".sessions/s.messages.json")).unwrap();
    assert!(saved.contains(SUMMARY) && !saved.contains("first report"));

    let sent = b.turns();
    assert_eq!(sent.len(), 1);
    let messages = sent[0]["messages"].as_array().unwrap().clone();
    let said = contents(&sent[0]);
    // System, the summary before the kept turn's message, its call, result, reply, and the
    // new message.
    assert_eq!(said.len(), 6);
    assert!(said[1].starts_with("<conversation-summary>"));
    assert!(said[1].contains(SUMMARY));
    assert!(said[1].ends_with("now read a.txt"));
    assert!(!said.iter().any(|text| text.contains("alpha alpha")));
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call-1");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call-1");
    assert!(said[3].contains("the contents of a"));
    assert_eq!(said[5], "anything else?");
}

#[tokio::test]
async fn a_window_nothing_fits_fails_the_next_turn_and_another_model_can_be_picked() {
    let (a, b) = (Arc::new(Fake::default()), Arc::new(Fake::default()));
    let (url_a, url_b) = (serve(Arc::clone(&a)).await, serve(Arc::clone(&b)).await);
    let dir = tempfile::tempdir().unwrap();
    let session = start(model("a:old", &url_a, None), dir.path(), false);
    turn(&session, "hello").await;

    // Selecting it is accepted: nothing is measured with the model yet.
    let fit = session
        .set_model(model("b:tiny", &url_b, Some(200)))
        .await
        .unwrap();
    assert_eq!(fit.budget, Some(170));
    assert_eq!(session.model().as_deref(), Some("b:tiny"));

    // The next message fails with the reason, and reaches no model.
    let mut events = session.subscribe();
    session.send("too big?".into()).unwrap();
    let failure = loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if let SessionEvent::TurnFailed { message } = event {
            break message;
        }
    };
    assert!(
        failure.contains("b:tiny") && failure.contains("cannot fit"),
        "{failure}"
    );
    assert!(b.requests().is_empty());
    assert!(a.summaries().is_empty());

    // The session stays usable: back on the old model, the conversation goes on.
    session
        .set_model(model("a:old", &url_a, None))
        .await
        .unwrap();
    turn(&session, "still there?").await;
    assert_eq!(a.turns().len(), 2);
    assert!(b.requests().is_empty());
    assert!(
        !history(&session)
            .iter()
            .any(|event| matches!(event, SessionEvent::ContextCompacted { .. }))
    );
}

#[tokio::test]
async fn a_resumed_session_keeps_the_model_it_switched_to() {
    let (a, b) = (Arc::new(Fake::default()), Arc::new(Fake::default()));
    let (url_a, url_b) = (serve(Arc::clone(&a)).await, serve(Arc::clone(&b)).await);
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join(".sessions/s.jsonl");
    let session = start(model("a:old", &url_a, None), dir.path(), false);
    turn(&session, "remember the number 7").await;
    assert_eq!(journalled_model(&journal), None);
    session
        .set_model(model("b:new", &url_b, None))
        .await
        .unwrap();
    drop(session);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The daemon resumes with the model the journal names (resolving the spec it records).
    assert_eq!(journalled_model(&journal).as_deref(), Some("b:new"));
    let resumed = start(model("b:new", &url_b, None), dir.path(), true);
    assert_eq!(resumed.model().as_deref(), Some("b:new"));
    assert!(history(&resumed).contains(&SessionEvent::ModelChanged {
        from: "a:old".into(),
        to: "b:new".into(),
        compacted: false,
    }));
    turn(&resumed, "what was the number?").await;
    let sent = b.turns();
    assert_eq!(sent.len(), 1);
    let said = contents(&sent[0]);
    assert_eq!(said[1], "remember the number 7");
    assert_eq!(said.last().unwrap(), "what was the number?");
    assert_eq!(a.turns().len(), 1);
}

#[tokio::test]
async fn compacting_now_keeps_the_last_turns() {
    let a = Arc::new(Fake::default());
    let url_a = serve(Arc::clone(&a)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(model("a:old", &url_a, Some(1_000_000)), dir.path(), false);
    turn(&session, "one").await;
    turn(&session, "two").await;
    // Two turns are all a manual compaction keeps: nothing to do.
    let fit = session.compact().await.unwrap();
    assert!(fit.compaction.is_none());
    turn(&session, "three").await;
    let fit = session.compact().await.unwrap();
    let compaction = fit.compaction.expect("the first turn was summarised");
    assert_eq!(compaction.summarised_messages, 2);
    assert_eq!(a.summaries().len(), 1);
    turn(&session, "four").await;
    let said = contents(a.turns().last().unwrap());
    assert!(said[1].starts_with("<conversation-summary>") && said[1].ends_with("two"));
    assert_eq!(said[2..], ["done", "three", "done", "four"]);
}

#[tokio::test]
async fn a_session_without_a_model_holds_messages_until_one_is_set() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start_with(None, dir.path(), false, false);
    assert_eq!(session.model(), None);
    assert_eq!(session.status().0, SessionStatus::Idle);
    // Compacting needs a model.
    assert_eq!(session.compact().await, Err(SwitchError::NoModel));

    let mut events = session.subscribe();
    session.send("first".into()).unwrap();
    session.send("second".into()).unwrap();
    // The messages wait, and the status says a model is needed; nothing reaches an endpoint.
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if event
            == (SessionEvent::StatusChanged {
                status: SessionStatus::NeedsModel,
                detail: None,
            })
        {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(session.status().0, SessionStatus::NeedsModel);
    assert_eq!(session.queue(None).unwrap().len(), 2);
    assert!(fake.requests().is_empty());

    // Once it has a model, both run in order.
    session
        .set_model(model("a:new", &url, Some(1_000_000)))
        .await
        .unwrap();
    assert_eq!(session.model().as_deref(), Some("a:new"));
    let mut completed = 0;
    while completed < 2 {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        assert!(
            !matches!(event, SessionEvent::TurnFailed { .. }),
            "{event:?}"
        );
        if matches!(event, SessionEvent::TurnCompleted { .. }) {
            completed += 1;
        }
    }
    let turns = fake.turns();
    assert_eq!(turns.len(), 2);
    assert!(
        contents(&turns[0])
            .iter()
            .any(|text| text.ends_with("first"))
    );
    assert!(
        contents(&turns[1])
            .iter()
            .any(|text| text.ends_with("second"))
    );
    assert!(history(&session).contains(&SessionEvent::ModelChanged {
        from: String::new(),
        to: "a:new".into(),
        compacted: false,
    }));
    assert_eq!(session.status().0, SessionStatus::Idle);
    // The journal remembers the model, so a resumed session keeps it.
    assert_eq!(
        journalled_model(&dir.path().join(".sessions/s.jsonl")).as_deref(),
        Some("a:new")
    );
}

/// What the instructions and tools take, from a turn's first context breakdown.
fn fixed_tokens(events: &[SessionEvent]) -> u64 {
    let base: i64 = events
        .iter()
        .find_map(|event| match event {
            SessionEvent::ContextBreakdown { categories, .. } => Some(
                categories
                    .iter()
                    .filter(|c| c.key.starts_with("system_") || c.key.starts_with("tools_"))
                    .map(|c| c.tokens)
                    .sum(),
            ),
            _ => None,
        })
        .unwrap();
    u64::try_from(base).unwrap()
}

#[tokio::test]
async fn a_long_lived_session_compacts_itself_when_a_turn_starts_over_budget() {
    let fake = Arc::new(Fake::default());
    let url = serve(Arc::clone(&fake)).await;
    let dir = tempfile::tempdir().unwrap();
    // The model's window is unknown at first, so the conversation grows unchecked.
    let session = start(model("a:m", &url, None), dir.path(), false);
    let report = "alpha ".repeat(8_000);
    let first = turn(&session, &format!("first report: {report}")).await;
    turn(&session, &format!("second report: {report}")).await;
    turn(&session, "a short note").await;
    let base = fixed_tokens(&first);
    drop(session);

    // After a restart, with a window the reports no longer fit, the next turn compacts first.
    let window = (base + 3_000) * 2;
    let session = start_with(
        Some(model("a:m", &url, Some(window))),
        dir.path(),
        true,
        true,
    );
    turn(&session, "anything new?").await;
    assert_eq!(fake.summaries().len(), 1);
    assert!(history(&session).iter().any(|event| matches!(
        event,
        SessionEvent::ContextCompacted { model, .. } if model == "a:m"
    )));
    let last = fake.turns().pop().unwrap();
    let said = contents(&last);
    assert!(
        said[1].starts_with("<conversation-summary>"),
        "{:?}",
        said[1]
    );
    assert!(!said.iter().any(|text| text.contains("alpha alpha")));
    assert_eq!(said.last().unwrap(), "anything new?");

    // Under the budget, nothing more is summarised.
    turn(&session, "and now?").await;
    assert_eq!(fake.summaries().len(), 1);
}
