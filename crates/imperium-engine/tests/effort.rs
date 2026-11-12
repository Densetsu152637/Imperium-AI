//! Reasoning effort: a session's effort reaches every call of a model that can reason, changes
//! from the next call when set, is overridden by a subagent spawned with its own, and is never
//! sent to a model that cannot reason.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use imperium_api::{Effort, FunctionCall, ToolCall};
use imperium_engine::{ChatClient, ChatClients, ResolvedModel, Session, SessionConfig};
use imperium_models::{ChatFuture, ChatMessage, ChatRequest, Completion, Endpoint};
use imperium_protocol::session::{ROOT_AGENT, SessionEvent};
use imperium_tools::Workspace;

/// Who called and with what effort.
#[derive(Debug, Clone, PartialEq)]
struct Call {
    root: bool,
    effort: Option<Effort>,
}

/// Records each request's effort. The root agent's first call of a turn spawns a subagent
/// when the user's message says `delegate`, then waits for it; every other call answers.
#[derive(Debug, Default)]
struct Recorder {
    calls: Mutex<Vec<Call>>,
}

fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        kind: "function".into(),
        function: FunctionCall {
            name: name.into(),
            arguments: arguments.into(),
        },
    }
}

impl ChatClient for Recorder {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(async move {
            let root = matches!(
                request.messages.first(),
                Some(ChatMessage::System { content }) if content.starts_with("You are Imperium, a coding agent")
            );
            self.calls.lock().unwrap().push(Call {
                root,
                effort: request.effort,
            });
            let last_user = request
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    ChatMessage::User { content, .. } => Some(content.as_str()),
                    _ => None,
                });
            let tools_used = request
                .messages
                .iter()
                .filter(|message| matches!(message, ChatMessage::Tool { .. }))
                .count();
            let tool_calls = match (
                root,
                last_user.is_some_and(|text| text.contains("delegate")),
                tools_used,
            ) {
                (true, true, 0) => vec![call(
                    "c1",
                    "spawn_agent",
                    r#"{"brief":"think hard","effort":"max"}"#,
                )],
                (true, true, 1) => vec![call("c2", "await_tasks", "{}")],
                _ => Vec::new(),
            };
            if tool_calls.is_empty() {
                on_text("done");
            }
            Ok(Completion {
                text: if tool_calls.is_empty() {
                    "done".into()
                } else {
                    String::new()
                },
                tool_calls,
                ..Completion::default()
            })
        })
    }
}

fn start(
    dir: &std::path::Path,
    reasoning: Option<bool>,
    model: &str,
    effort: Effort,
) -> (Session, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let mut clients = ChatClients::default();
    let shared = Arc::clone(&recorder);
    clients.register("recorder", move |_http, _endpoint| {
        Arc::clone(&shared) as Arc<dyn ChatClient>
    });
    let session = Session::spawn(SessionConfig {
        id: "effort".into(),
        workspace: Workspace::new(dir).unwrap(),
        effort,
        model: Some(ResolvedModel {
            spec: format!("custom:{model}"),
            endpoint: Endpoint {
                base_url: "http://unreachable.invalid/v1".into(),
                api_key: None,
            },
            model: model.into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "recorder".into(),
            reasoning,
        }),
        catalog: Vec::new(),
        clients,
        routing: imperium_config::RoutingSettings::default(),
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
        keep_agent_state: false,
        journal_path: dir.join("journal.jsonl"),
    })
    .unwrap();
    (session, recorder)
}

/// Sends `text` and waits for the root agent's turn to end.
async fn turn(session: &Session, text: &str) {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        if event.agent_id != ROOT_AGENT {
            continue;
        }
        match event.event {
            SessionEvent::TurnCompleted { .. } => return,
            SessionEvent::TurnFailed { message } => panic!("turn failed: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn the_session_effort_reaches_each_call_and_changes_from_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let (session, recorder) = start(dir.path(), Some(true), "plain", Effort::High);
    assert_eq!(session.effort(), Effort::High);
    turn(&session, "hello").await;
    session.set_effort(Effort::Low);
    assert_eq!(session.effort(), Effort::Low);
    turn(&session, "again").await;
    let efforts: Vec<_> = recorder
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.effort)
        .collect();
    assert_eq!(efforts, [Some(Effort::High), Some(Effort::Low)]);
}

#[tokio::test]
async fn a_model_that_cannot_reason_is_sent_no_effort() {
    let dir = tempfile::tempdir().unwrap();
    // Configured off, even though the name is a known reasoning model.
    let (session, recorder) = start(dir.path(), Some(false), "o3", Effort::Max);
    turn(&session, "hello").await;
    assert_eq!(recorder.calls.lock().unwrap()[0].effort, None);

    // Unconfigured, the name decides.
    let dir = tempfile::tempdir().unwrap();
    let (session, recorder) = start(dir.path(), None, "gpt-4o", Effort::Max);
    turn(&session, "hello").await;
    assert_eq!(recorder.calls.lock().unwrap()[0].effort, None);
}

#[tokio::test]
async fn a_subagent_spawned_with_an_effort_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let (session, recorder) = start(dir.path(), Some(true), "plain", Effort::Low);
    turn(&session, "delegate this").await;
    let calls = recorder.calls.lock().unwrap().clone();
    assert!(
        calls
            .iter()
            .filter(|call| call.root)
            .all(|call| call.effort == Some(Effort::Low)),
        "{calls:?}"
    );
    let sub: Vec<_> = calls.iter().filter(|call| !call.root).collect();
    assert_eq!(sub.len(), 1, "{calls:?}");
    assert_eq!(sub[0].effort, Some(Effort::Max));
}
