//! Phase 10 exit criterion: an agent generates an image and audio (after approval), and the
//! files land in the project's media folder.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use base64::Engine;
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_media::{Media, MediaTools, OpenAiImages, Service};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;
use serde_json::{Value, json};

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn call(name: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
    )
}

#[tokio::test]
async fn agents_generate_images_and_audio() {
    let services = serve(
        Router::new()
            .route(
                "/v1/images/generations",
                post(|| async {
                    let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG fake");
                    axum::Json(json!({"data": [{"b64_json": png}]}))
                }),
            )
            .route(
                "/v1/audio/speech",
                post(|| async { b"ID3 fake mp3".to_vec() }),
            )
            .route(
                "/v1/audio/transcriptions",
                post(|| async { axum::Json(json!({"text": "Welcome to Imperium."})) }),
            ),
    )
    .await;
    let step = Arc::new(AtomicUsize::new(0));
    let model = {
        let step = Arc::clone(&step);
        serve(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let step = Arc::clone(&step);
                async move {
                    let body = match step.fetch_add(1, Ordering::SeqCst) {
                        0 => call(
                            "media_image",
                            json!({"prompt": "A lighthouse at dusk", "name": "lighthouse"}),
                        ),
                        1 => call(
                            "media_speak",
                            json!({"text": "Welcome to Imperium.", "name": "welcome"}),
                        ),
                        _ => sse(json!({"choices":[{"delta":{"content":"Made both."}}]})),
                    };
                    ([("content-type", "text/event-stream")], body)
                }
            }),
        ))
        .await
    };
    let service = |model: &str| Service {
        base_url: format!("{services}/v1"),
        api_key: None,
        model: model.into(),
    };
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "media".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: format!("{model}/v1"),
                api_key: None,
            },
            model: "model".into(),
            pricing: None,
            local: false,
            context_window: None,
            kind: "openai".into(),
        }),
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        clients: Default::default(),
        local: None,
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
        tools: vec![Arc::new(MediaTools::new(Arc::new(Media {
            image: Some(Arc::new(OpenAiImages {
                service: service("sdxl"),
            })),
            speech: Some((service("tts"), "alloy".into())),
            transcription: Some(service("whisper")),
        })))],
        max_steps: 6,
        background: None,
        auto_compact: false,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session
        .send("Make a lighthouse picture and a spoken welcome".into())
        .unwrap();
    let mut created = Vec::new();
    let mut asked = 0;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap()
            .event;
        match event {
            SessionEvent::PermissionRequested { request_id, .. } => {
                asked += 1;
                session
                    .respond(request_id, PermissionDecision::Allow)
                    .unwrap();
            }
            SessionEvent::MediaCreated { path, kind, .. } => created.push((kind, path)),
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(asked, 2);
    assert_eq!(created.len(), 2);
    assert_eq!(created[0].0, "image");
    assert!(created[0].1.starts_with("media/lighthouse-") && created[0].1.ends_with(".png"));
    assert!(created[1].1.starts_with("media/welcome-") && created[1].1.ends_with(".mp3"));
    for (_, path) in &created {
        assert!(dir.path().join(path).is_file(), "{path}");
        assert!(
            dir.path().join(format!("{path}.json")).is_file(),
            "{path} provenance"
        );
    }
}
