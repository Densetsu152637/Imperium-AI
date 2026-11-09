//! Background work that was released from its wait for a quiet machine (`max_defer` passed, or a
//! person ran it now) makes its model calls even though the machine is still busy: it does not
//! give way a second time at its first step boundary.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::LoadView;
use imperiumd::ManualLoad;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// A model that plays a project agent (start a session, then acknowledge) or any other session
/// (answer at once). `plain` counts requests from sessions other than a project agent.
async fn fake_model(plain: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let plain = Arc::clone(&plain);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let system = messages[0]["content"].as_str().unwrap_or_default();
                let last_role = messages.last().map(|m| m["role"].clone());
                let reply = if system.contains("this project's agent") {
                    if last_role == Some(json!("tool")) {
                        sse(json!({"choices":[{"delta":{"content":"Dispatched."}}]}))
                    } else {
                        let arguments = json!({"brief": "Look around"});
                        sse(
                            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"s",
                            "function":{"name":"start_session","arguments":arguments.to_string()}}]}}]}),
                        )
                    }
                } else {
                    plain.fetch_add(1, Ordering::SeqCst);
                    sse(json!({"choices":[{"delta":{"content":"Done."}}]}))
                };
                ([("content-type", "text/event-stream")], reply)
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

struct Setup {
    daemon: Arc<imperiumd::DaemonState>,
    load: Arc<ManualLoad>,
    plain: Arc<AtomicUsize>,
    project: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Setup {
    /// A second daemon on the same data, as after a restart, with its own load view (busy).
    /// The first one keeps running, but its machine stays busy so it does nothing more.
    fn restart(&self) -> (Arc<imperiumd::DaemonState>, Arc<ManualLoad>) {
        let daemon = imperiumd::DaemonState::new(
            imperium_config::Paths::under(self.home.path()),
            (*self.daemon.settings()).clone(),
            None,
        );
        let load = ManualLoad::new(imperium_core::Load::Busy);
        daemon.use_load_view(load.clone());
        (daemon, load)
    }

    fn meta(&self, id: &str) -> Value {
        let path = self
            .home
            .path()
            .join("data")
            .join("sessions")
            .join(format!("{id}.meta.json"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// A background session of the user's project, as a schedule would start.
    fn background(&self) -> Arc<imperium_core::Session> {
        let options = imperiumd::SessionOptions {
            background: true,
            ..imperiumd::SessionOptions::default()
        };
        self.daemon
            .create_session_with(&self.project.path().display().to_string(), None, options)
            .unwrap()
            .0
    }
}

/// A daemon whose view stays busy, with `extra` appended to its configuration (`{path}` is the
/// project folder).
async fn setup(extra: &str) -> Setup {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let path = project.path().display().to_string().replace('\\', "/");
    let plain = Arc::new(AtomicUsize::new(0));
    let config = format!(
        r#"
        [models]
        default = "fake:model"
        [endpoints.fake]
        base_url = "{model}"
        [memory]
        enabled = false
        {extra}
        "#,
        model = fake_model(Arc::clone(&plain)).await,
        extra = extra.replace("{path}", &path),
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
    let load = ManualLoad::new(imperium_core::Load::Busy);
    daemon.use_load_view(load.clone());
    Setup {
        daemon,
        load,
        plain,
        project,
        home,
    }
}

async fn until(what: &str, within: Duration, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn quiet_release_a_schedule_prompt_runs_after_max_defer_though_the_machine_stays_busy() {
    let setup = setup(
        r#"
        [[schedules]]
        id = "audit"
        trigger = { every = "1s" }
        action = { project = "{path}", prompt = "Audit." }
        prefer_quiet = true
        max_defer = "3s"
        "#,
    )
    .await;
    setup.daemon.start_scheduler();
    // Deferred first (nothing is asked of the model), then released by max_defer.
    until("the run is deferred", Duration::from_secs(10), || {
        setup
            .daemon
            .scheduler()
            .is_some_and(|s| s.list().schedules[0].deferred.is_some())
    })
    .await;
    assert_eq!(setup.plain.load(Ordering::SeqCst), 0);
    until(
        "a model call after max_defer",
        Duration::from_secs(30),
        || setup.plain.load(Ordering::SeqCst) >= 1,
    )
    .await;
    assert_eq!(setup.load.level(), imperium_core::Load::Busy);
}

#[tokio::test]
async fn quiet_release_a_manual_schedule_run_calls_the_model_while_busy() {
    let setup = setup(
        r#"
        [[schedules]]
        id = "manual"
        trigger = { at = "2099-01-01 00:00" }
        action = { project = "{path}", prompt = "Audit." }
        "#,
    )
    .await;
    let scheduler = setup.daemon.start_scheduler();
    assert_eq!(scheduler.run_now(&setup.daemon, "manual"), Ok(true));
    until("a model call", Duration::from_secs(10), || {
        setup.plain.load(Ordering::SeqCst) >= 1
    })
    .await;
}

#[tokio::test]
async fn quiet_release_running_a_deferred_schedule_now_calls_the_model_while_busy() {
    let setup = setup(
        r#"
        [[schedules]]
        id = "later"
        trigger = { every = "1s" }
        action = { project = "{path}", prompt = "Audit." }
        conditions = { when_quiet = true }
        "#,
    )
    .await;
    let scheduler = setup.daemon.start_scheduler();
    until("the run is deferred", Duration::from_secs(10), || {
        scheduler.list().schedules[0].deferred.is_some()
    })
    .await;
    assert_eq!(scheduler.run_now(&setup.daemon, "later"), Ok(true));
    until("a model call", Duration::from_secs(10), || {
        setup.plain.load(Ordering::SeqCst) >= 1
    })
    .await;
}

/// Tells the project agent something and returns the session it dispatches.
async fn dispatched(setup: &Setup) -> Arc<imperium_core::Session> {
    setup.daemon.start_project_agents();
    let agents = setup.daemon.project_agents().list();
    let agent_id = agents[0].1.clone();
    assert!(
        setup
            .daemon
            .tell_project_agent(setup.project.path(), "Look around.".into())
    );
    let mut found = None;
    until("the session is dispatched", Duration::from_secs(10), || {
        found = setup
            .daemon
            .sessions()
            .into_iter()
            .find(|session| session.id() != agent_id);
        found.is_some()
    })
    .await;
    found.unwrap()
}

#[tokio::test]
async fn quiet_release_a_project_agents_session_runs_after_max_defer_though_busy() {
    let setup = setup(
        r#"
        [[projects]]
        path = "{path}"
        agent = true
        max_defer = "3s"
        "#,
    )
    .await;
    let session = dispatched(&setup).await;
    assert!(
        session.hold().is_some(),
        "deferred while the machine is busy"
    );
    assert_eq!(setup.plain.load(Ordering::SeqCst), 0);
    until(
        "a model call after max_defer",
        Duration::from_secs(30),
        || setup.plain.load(Ordering::SeqCst) >= 1,
    )
    .await;
    assert_eq!(setup.load.level(), imperium_core::Load::Busy);
    assert!(session.hold().is_none());
}

#[tokio::test]
async fn quiet_release_running_a_project_agents_session_now_calls_the_model_while_busy() {
    let setup = setup(
        r#"
        [[projects]]
        path = "{path}"
        agent = true
        "#,
    )
    .await;
    let session = dispatched(&setup).await;
    assert!(session.hold().is_some());
    session.resume_now();
    until("a model call", Duration::from_secs(10), || {
        setup.plain.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(session.hold().is_none());
}

#[tokio::test]
async fn quiet_release_a_waiting_brief_survives_a_restart_and_runs_when_quiet() {
    let setup = setup(
        r#"
        [[projects]]
        path = "{path}"
        agent = true
        "#,
    )
    .await;
    let session = dispatched(&setup).await;
    assert!(session.hold().is_some());
    let id = session.id().to_string();
    assert_eq!(setup.meta(&id)["deferred_brief"]["text"], "Look around");

    let (daemon, load) = setup.restart();
    assert_eq!(daemon.resume_sessions(), 1);
    daemon.start_project_agents();
    let restored = daemon.session(&id).unwrap();
    assert!(restored.is_background());
    assert!(
        restored.hold().is_some(),
        "it waits for a quiet machine again"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(setup.plain.load(Ordering::SeqCst), 0);

    load.set(imperium_core::Load::Quiet);
    until("the brief is sent", Duration::from_secs(30), || {
        restored.title() == "Look around"
    })
    .await;
    until("a model call", Duration::from_secs(30), || {
        setup.plain.load(Ordering::SeqCst) >= 1
    })
    .await;
    assert!(setup.meta(&id).get("deferred_brief").is_none(), "sent once");
}

#[tokio::test]
async fn quiet_release_a_restart_keeps_background_and_run_now() {
    let setup = setup("").await;
    let released = setup.background();
    setup.daemon.release_run_now(&released);
    let yielding = setup.background();

    let (daemon, _load) = setup.restart();
    assert_eq!(daemon.resume_sessions(), 2);
    let released = daemon.session(released.id()).unwrap();
    let yielding = daemon.session(yielding.id()).unwrap();
    assert!(released.is_background() && released.run_now().released());
    assert!(yielding.is_background() && !yielding.run_now().released());
}

#[tokio::test]
async fn quiet_release_a_person_writing_to_background_work_releases_it() {
    let setup = setup("").await;
    // Work sent by the daemon (a schedule, a project agent) still gives way.
    let automatic = setup.background();
    automatic.send("Audit.".into()).unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!automatic.run_now().released());
    assert_eq!(setup.plain.load(Ordering::SeqCst), 0);

    let session = setup.background();
    let mut connection = imperiumd::Connection::new(Arc::clone(&setup.daemon));
    let handshake = json!({"jsonrpc":"2.0","id":1,"method":"system.handshake","params":{
        "client":{"name":"test","version":"0"},"protocol":imperium_protocol::PROTOCOL_VERSION}});
    connection.handle_text(&handshake.to_string());
    let send = json!({"jsonrpc":"2.0","id":2,"method":"session.send","params":{
        "session_id": session.id(), "text": "What did you find?"}});
    connection.handle_text(&send.to_string());
    assert!(session.run_now().released());
    assert_eq!(setup.meta(session.id())["run_now"], true);
    until("a model call while busy", Duration::from_secs(10), || {
        setup.plain.load(Ordering::SeqCst) >= 1
    })
    .await;
}
