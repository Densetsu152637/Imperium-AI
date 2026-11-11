//! Files attached to a message: what the model receives, what the journal keeps, and how the
//! context breakdown counts them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::session::{Attachment, AttachmentKind, SendMode, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

/// Answers every request with "seen", keeping the last request body.
async fn model(last: Arc<Mutex<Value>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let last = Arc::clone(&last);
            async move {
                *last.lock().unwrap() = body;
                let body = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"delta":{"content":"seen"}}]}),
                    json!({"choices":[],"usage":{"prompt_tokens":1200,"completion_tokens":1}})
                );
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn attachment(
    path: &std::path::Path,
    kind: AttachmentKind,
    mime: &str,
    vision: bool,
) -> Attachment {
    Attachment {
        id: format!("a-{}", path.file_name().unwrap().to_string_lossy()),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        kind,
        mime: mime.into(),
        size: std::fs::metadata(path).unwrap().len(),
        path: path.display().to_string(),
        copied: false,
        vision,
        preview: None,
    }
}

#[tokio::test]
async fn attachments_reach_the_model_and_the_journal() {
    let project = tempfile::tempdir().unwrap();
    let notes = project.path().join("notes.txt");
    std::fs::write(&notes, "the launch code is 4242").unwrap();
    let image = project.path().join("shot.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfake").unwrap();
    let last = Arc::new(Mutex::new(Value::Null));
    let journal = project.path().join(".journal.jsonl");
    let session = Session::spawn(SessionConfig {
        id: "attach".into(),
        workspace: Workspace::new(project.path()).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: model(Arc::clone(&last)).await,
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
        background: None,
        auto_compact: false,
        keep_agent_state: false,
        effort: Default::default(),
        journal_path: journal.clone(),
    })
    .unwrap();
    let mut events = session.subscribe();
    let attached = vec![
        attachment(&notes, AttachmentKind::Text, "text/plain", true),
        attachment(&image, AttachmentKind::Image, "image/png", true),
    ];
    session
        .send_with_attachments(String::new(), attached.clone(), SendMode::Queue, None)
        .unwrap();
    let mut started = None;
    let mut breakdown = None;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::TurnStarted { attachments, .. } => started = Some(attachments),
            SessionEvent::ContextBreakdown { categories, .. } => breakdown = Some(categories),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(started.as_deref(), Some(attached.as_slice()));

    // The user message carries the inlined text before the (empty) message, then the image.
    let body = last.lock().unwrap().clone();
    let user = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .cloned()
        .unwrap();
    let parts = user["content"]
        .as_array()
        .expect("array content with an image");
    let text = parts[0]["text"].as_str().unwrap();
    assert!(text.starts_with("<attachments>"), "{text}");
    assert!(text.contains("the launch code is 4242"));
    assert_eq!(parts[1]["type"], "image_url");
    assert!(
        parts[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );

    // The breakdown counts attachments on their own, the image included.
    let categories = breakdown.expect("a context breakdown");
    let attachments = categories
        .iter()
        .find(|category| category.key == "attachments")
        .expect("an attachments category");
    assert!(attachments.tokens > 0);

    // The journal keeps the attachments, so a resumed or archived session shows them.
    let journalled = std::fs::read_to_string(&journal).unwrap();
    assert!(journalled.contains("\"attachments\""));
    assert!(journalled.contains("shot.png"));
    assert!(
        !journalled.contains("base64,"),
        "image bytes stay out of the journal"
    );
}
