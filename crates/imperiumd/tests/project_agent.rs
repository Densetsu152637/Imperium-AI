//! A project agent woken by a schedule dispatches work into a new session, that session's
//! permission request becomes an approval request, and the session's report comes back to the
//! agent as a message.

use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn call(name: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
    )
}

fn text(text: &str) -> String {
    sse(json!({"choices":[{"delta":{"content":text}}]}))
}

/// The agent starts a session for a schedule and acknowledges reports; the dispatched session
/// writes a file (needing approval) and reports.
async fn fake_model() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let system = messages[0]["content"].as_str().unwrap_or_default();
            let last = messages.last().cloned().unwrap_or_default();
            let last_user = messages
                .iter()
                .rev()
                .find(|m| m["role"] == "user")
                .and_then(|m| m["content"].as_str())
                .unwrap_or_default()
                .to_string();
            let body = if system.contains("this project's agent") {
                if last["role"] == "tool" {
                    text("Dispatched.")
                } else if last_user.contains("finished") {
                    text("Noted the report.")
                } else {
                    call(
                        "start_session",
                        json!({"brief": "Write notes.txt summarising open issues"}),
                    )
                }
            } else if last["role"] == "tool" {
                text("Wrote notes.txt with 2 open issues.")
            } else {
                call(
                    "write_file",
                    json!({"path": "notes.txt", "content": "2 open issues\n"}),
                )
            };
            ([("content-type", "text/event-stream")], body)
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
async fn a_scheduled_event_wakes_the_project_agent_which_dispatches_and_hears_back() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let path = project.path().display().to_string().replace('\\', "/");
    let config = format!(
        r#"
        [models]
        default = "fake:model"
        [endpoints.fake]
        base_url = "{model}"
        [memory]
        enabled = false
        [[projects]]
        path = "{path}"
        agent = true
        [[schedules]]
        id = "morning"
        trigger = {{ every = "1s" }}
        action = {{ project = "{path}", agent = "Summarise the open issues." }}
        "#,
        model = fake_model().await
    );
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    let daemon =
        imperiumd::DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
    let mut announcements = daemon.announcements();
    daemon.start_project_agents();
    let agents = daemon.project_agents().list();
    assert_eq!(agents.len(), 1);
    let agent = daemon.session(&agents[0].1).unwrap();
    let mut agent_events = agent.subscribe();
    daemon.start_scheduler();

    // The dispatched session's write waits for approval.
    let approval = loop {
        let notification = tokio::time::timeout(Duration::from_secs(20), announcements.recv())
            .await
            .expect("an approval request within 20 s")
            .unwrap();
        if notification.method == "approval.requested" {
            break notification.params;
        }
    };
    assert!(approval["summary"].as_str().unwrap().contains("write_file"));
    assert!(
        daemon
            .approvals()
            .respond(approval["id"].as_u64().unwrap(), true)
    );

    // The agent hears the report.
    let report = loop {
        let event = tokio::time::timeout(Duration::from_secs(20), agent_events.recv())
            .await
            .expect("the report within 20 s")
            .unwrap();
        if let SessionEvent::TurnStarted { text } = event.event
            && text.contains("finished")
        {
            break text;
        }
    };
    assert!(
        report.contains("Wrote notes.txt with 2 open issues."),
        "{report}"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("notes.txt")).unwrap(),
        "2 open issues\n"
    );
    // The agent, its dispatched session, and nothing more.
    assert!(daemon.sessions().len() >= 2);
}
