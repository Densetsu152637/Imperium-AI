//! A model client registered under its own endpoint kind is used for a session's model calls,
//! and context breakdowns ask its tokenizer.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use imperium_engine::{ChatClient, ChatClients, ResolvedModel, Session, SessionConfig};
use imperium_models::{
    ChatFuture, ChatRequest, Completion, Endpoint, TokenFuture, Tokenizer, Usage,
};
use imperium_protocol::session::{SessionEvent, TokenCountMethod};
use imperium_tools::Workspace;
use serde_json::Value;

/// Answers every request itself and counts tokens as words.
#[derive(Debug, Default)]
struct Scripted {
    /// The model and message count of each request.
    requests: Mutex<Vec<(String, usize)>>,
}

impl ChatClient for Scripted {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push((request.model.to_string(), request.messages.len()));
            on_text("Hello from ");
            on_text("the client.");
            Ok(Completion {
                text: "Hello from the client.".into(),
                tool_calls: Vec::new(),
                usage: Some(Usage {
                    prompt_tokens: 900,
                    completion_tokens: 4,
                    cached_tokens: 0,
                }),
            })
        })
    }

    fn tokenizer(&self) -> Option<&dyn Tokenizer> {
        Some(self)
    }
}

impl Tokenizer for Scripted {
    fn server(&self) -> &str {
        "scripted"
    }

    fn count_tokens<'a>(&'a self, text: &'a str) -> TokenFuture<'a, u64> {
        Box::pin(async move { Ok(text.split_whitespace().count() as u64) })
    }

    fn template_tokens<'a>(
        &'a self,
        _messages: &'a Value,
        tools: &'a [Value],
    ) -> TokenFuture<'a, u64> {
        Box::pin(async move { Ok(10 + 3 * tools.len() as u64) })
    }

    fn context_window(&self) -> TokenFuture<'_, Option<u64>> {
        Box::pin(async { Ok(Some(1234)) })
    }
}

#[tokio::test]
async fn a_session_calls_the_client_registered_for_its_endpoint_kind() {
    let scripted = Arc::new(Scripted::default());
    let mut clients = ChatClients::default();
    let shared = Arc::clone(&scripted);
    clients.register("scripted", move |_http, _endpoint| {
        Arc::clone(&shared) as Arc<dyn ChatClient>
    });
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "client".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(ResolvedModel {
            spec: "custom:model-x".into(),
            // Never contacted: the scripted client answers.
            endpoint: Endpoint {
                base_url: "http://unreachable.invalid/v1".into(),
                api_key: None,
            },
            model: "model-x".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "scripted".into(),
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
        max_steps: 3,
        auto_compact: false,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("say hello".into()).unwrap();
    let mut text = String::new();
    let mut breakdown = None;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::TextDelta { text: delta } => text.push_str(&delta),
            SessionEvent::ContextBreakdown {
                method,
                context_window,
                prompt_tokens,
                ..
            } => breakdown = Some((method, context_window, prompt_tokens)),
            SessionEvent::TurnCompleted { usage, .. } => {
                assert_eq!(usage.prompt_tokens, 900);
                break;
            }
            SessionEvent::TurnFailed { message } => panic!("turn failed: {message}"),
            _ => {}
        }
    }
    assert_eq!(text, "Hello from the client.");
    assert_eq!(
        *scripted.requests.lock().unwrap(),
        [("model-x".to_string(), 2)]
    );
    // The breakdown counted with the client's tokenizer and took its context window.
    assert_eq!(
        breakdown,
        Some((TokenCountMethod::Exact, Some(1234), Some(900)))
    );
}

#[tokio::test]
async fn an_unknown_endpoint_kind_fails_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "unknown".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(ResolvedModel {
            spec: "custom:model".into(),
            endpoint: Endpoint {
                base_url: "http://unreachable.invalid/v1".into(),
                api_key: None,
            },
            model: "model".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "anthropic".into(),
        }),
        catalog: Vec::new(),
        clients: ChatClients::default(),
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
        max_steps: 3,
        auto_compact: false,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("hi".into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        if let SessionEvent::TurnFailed { message } = event {
            assert_eq!(message, r#"no model client for endpoint kind "anthropic""#);
            break;
        }
    }
}
