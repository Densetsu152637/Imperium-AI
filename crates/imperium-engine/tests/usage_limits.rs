//! When a provider's usage runs out, a turn waits as the session's resume policy says and
//! continues afterwards (or stops, if the policy says so).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use imperium_engine::{ResolvedModel, Session, SessionConfig, default_resume_policy};
use imperium_models::Endpoint;
use imperium_protocol::session::{ResumePolicy, SessionEvent, SessionStatus};
use imperium_tools::Workspace;
use serde_json::json;

/// Refuses the first call with 429 and a one-hour Retry-After, then answers.
async fn limited_model(calls: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let calls = Arc::clone(&calls);
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "3600")],
                        "usage limit reached",
                    )
                        .into_response();
                }
                let chunk = json!({"choices":[{"delta":{"content":"done"}}]});
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn start(url: &str, dir: &std::path::Path, policy: ResumePolicy) -> Session {
    Session::spawn(SessionConfig {
        id: "u".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint: Endpoint {
                base_url: url.into(),
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
        resume_policy: policy,
        instructions: None,
        max_steps: 3,
        background: None,
        auto_compact: false,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join("u.jsonl"),
    })
    .unwrap()
}

async fn next(
    events: &mut tokio::sync::broadcast::Receiver<imperium_protocol::session::AgentEvent>,
) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("an event within 10 s")
        .unwrap()
        .event
}

#[tokio::test]
async fn a_usage_limit_pauses_until_reset_and_resumes_early_on_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let url = limited_model(Arc::clone(&calls)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(&url, dir.path(), default_resume_policy());
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    let until = loop {
        if let SessionEvent::Paused {
            until_unix_ms,
            reason,
            ..
        } = next(&mut events).await
        {
            assert!(reason.contains("429"), "{reason}");
            break until_unix_ms;
        }
    };
    // The provider's hour, not the five-minute default.
    let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
    assert!(
        until > now + 3_500_000 && until < now + 3_700_000,
        "{until} vs {now}"
    );
    assert_eq!(session.status().0, SessionStatus::Paused);
    session.resume_now();
    let mut resumed = false;
    loop {
        match next(&mut events).await {
            SessionEvent::Resumed {} => resumed = true,
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert!(resumed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(session.status().0, SessionStatus::Idle);
}

#[tokio::test]
async fn a_stop_policy_fails_the_turn_at_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let url = limited_model(Arc::clone(&calls)).await;
    let dir = tempfile::tempdir().unwrap();
    let session = start(&url, dir.path(), default_resume_policy());
    // Overridden per session.
    session.set_resume_policy(Some(ResumePolicy {
        mode: "stop".into(),
        ..default_resume_policy()
    }));
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    loop {
        match next(&mut events).await {
            SessionEvent::Paused { .. } => panic!("the stop policy must not wait"),
            SessionEvent::TurnFailed { message } => {
                assert!(message.contains("429"), "{message}");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(session.status().0, SessionStatus::NeedsAttention);
    assert_eq!(session.resume_policy().mode, "stop");
}
