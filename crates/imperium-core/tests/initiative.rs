//! Initiative: an environment's persistent agent reviews its environment when the machine and
//! the environment are idle, and in `suggest` mode only records suggestions, in `act` mode starts
//! at most the configured number of tagged sessions in the default permission mode, and in `off`
//! mode does nothing on its own. Fake models stand in for every agent.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState, ManualLoad};
use imperium_engine::Load;
use imperium_protocol::initiative::{Initiative, NoticeKind, SuggestionStatus};
use imperium_protocol::jsonrpc::{Notification, Outcome, Response};
use imperium_protocol::methods::{EnvironmentCreateParams, EnvironmentInfo};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const SOURCE: &str = "docs/12-roadmap.md#next";
const BRIEF: &str = "Start the next unfinished item of the roadmap and improve my timeline";

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn call_body(name: &str, arguments: Value) -> String {
    sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":name,
        "function":{"name":name,"arguments":arguments.to_string()}}]}}]}),
    )
}

fn text_body(text: &str) -> String {
    let usage = json!({"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":4}});
    format!(
        "data: {}\n\ndata: {usage}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"content":text}}]})
    )
}

/// Every request the fake model got, in order.
type Log = Arc<Mutex<Vec<Value>>>;

fn content(message: &Value) -> String {
    message["content"].as_str().unwrap_or_default().to_string()
}

/// The persistent agent follows a script by the number of tool results since the last user
/// message: in a `suggest` review it first tries to start a session (which it may not), then
/// suggests and writes a digest; in an `act` review it starts three sessions (the limits decide
/// how many succeed); after the report of the first session it started it tries to start
/// another; when a user asks it to start something it does. Every other session (one the agent
/// started, or the user's) answers with what it was told.
async fn fake_model(log: Log) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let log = Arc::clone(&log);
            async move {
                log.lock().unwrap().push(body.clone());
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let system = content(&messages[0]);
                let last_user_at = messages
                    .iter()
                    .rposition(|m| m["role"] == "user")
                    .unwrap_or(0);
                let said = content(&messages[last_user_at]);
                let tools_since = messages[last_user_at..]
                    .iter()
                    .filter(|m| m["role"] == "tool")
                    .count();
                let reply = if !system.contains("this environment's persistent agent") {
                    // A session the agent started: the first takes a while, so the limits on
                    // sessions running at once can be seen.
                    if said.contains("(part 0)") {
                        tokio::time::sleep(Duration::from_millis(1200)).await;
                    }
                    text_body(&format!("Done: {said}"))
                } else if said.starts_with("Initiative review for the environment") {
                    if said.contains("Mode: act.") {
                        match tools_since {
                            0..=2 => call_body(
                                "start_session",
                                json!({
                                    "brief": format!("{BRIEF} (part {tools_since})"),
                                    "reason": "the roadmap's next item is unstarted",
                                    "source": SOURCE,
                                }),
                            ),
                            _ => text_body("Started what I could."),
                        }
                    } else {
                        match tools_since {
                            0 => call_body(
                                "start_session",
                                json!({"brief": "sneak a session in", "reason": "x", "source": "y"}),
                            ),
                            1 => call_body(
                                "suggest_work",
                                json!({
                                    "title": "Improve the timeline",
                                    "prompt": "Improve my timeline",
                                    "reason": "the roadmap lists it as next",
                                    "source": SOURCE,
                                }),
                            ),
                            2 if said.contains("Work left:") => call_body(
                                "work_digest",
                                json!({
                                    "summary": "Two roadmap items are left.",
                                    "items": [{"text": "Timeline", "source": SOURCE}],
                                }),
                            ),
                            _ => text_body("Suggested 1 item."),
                        }
                    }
                } else if said.starts_with("Session ") && said.contains("finished") {
                    if tools_since == 0 && said.contains("(part 0)") {
                        // Chasing more work after a report: the limits still apply.
                        call_body(
                            "start_session",
                            json!({"brief": "do the next one too", "reason": "more", "source": SOURCE}),
                        )
                    } else {
                        text_body("Noted.")
                    }
                } else if said.contains("please start a session") {
                    if tools_since == 0 {
                        call_body("start_session", json!({"brief": "tidy the notes"}))
                    } else {
                        text_body("Started it.")
                    }
                } else {
                    text_body("Hello.")
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

struct Fixture {
    daemon: Arc<DaemonState>,
    log: Log,
    env: EnvironmentInfo,
    load: Arc<ManualLoad>,
    _home: tempfile::TempDir,
    _folder: tempfile::TempDir,
}

/// A daemon with an environment whose persistent agent has `mode` and a quiet machine.
async fn fixture(mode: Initiative, extra: &str) -> Fixture {
    let log: Log = Arc::default();
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n{extra}",
        fake_model(Arc::clone(&log)).await
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
    let daemon = DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
    let load = ManualLoad::new(Load::Quiet);
    daemon.use_load_view(load.clone());
    let env = daemon
        .environments()
        .create(EnvironmentCreateParams {
            name: "Imperium".into(),
            folders: vec![folder.path().display().to_string()],
            persistent_agent: Some(true),
            initiative: Some(mode),
            ..EnvironmentCreateParams::default()
        })
        .unwrap();
    daemon.sync_environment_agent(&env.id);
    assert!(daemon.project_agents().environment(&env.id).is_some());
    Fixture {
        daemon,
        log,
        env,
        load,
        _home: home,
        _folder: folder,
    }
}

impl Fixture {
    /// Requests that carried an initiative review message.
    fn reviews_seen(&self) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|body| {
                body["messages"].as_array().is_some_and(|messages| {
                    messages
                        .iter()
                        .any(|m| content(m).starts_with("Initiative review for the environment"))
                })
            })
            .count()
    }

    /// Text of every tool result the fake model was shown.
    fn tool_results(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for body in self.log.lock().unwrap().iter() {
            for message in body["messages"].as_array().unwrap() {
                if message["role"] == "tool" && !seen.contains(&content(message)) {
                    seen.push(content(message));
                }
            }
        }
        seen
    }

    /// Sessions of the environment other than the persistent agent.
    fn other_sessions(&self) -> Vec<imperium_protocol::methods::SessionSummary> {
        let agent = self
            .daemon
            .project_agents()
            .environment(&self.env.id)
            .unwrap();
        self.daemon
            .sessions()
            .iter()
            .filter(|s| s.id() != agent.id())
            .map(|s| self.daemon.summary(s))
            .collect()
    }

    async fn wait_until_idle(&self) {
        for _ in 0..200 {
            let agent = self
                .daemon
                .project_agents()
                .environment(&self.env.id)
                .unwrap();
            let working = self
                .daemon
                .sessions()
                .iter()
                .any(|s| s.status().0 == imperium_protocol::session::SessionStatus::Working);
            if self.daemon.initiative().running(&self.env.id) == 0
                && !working
                && agent.status().0 == imperium_protocol::session::SessionStatus::Idle
            {
                // Reports reach the agent a moment after the sessions end.
                tokio::time::sleep(Duration::from_millis(300)).await;
                if !self
                    .daemon
                    .sessions()
                    .iter()
                    .any(|s| s.status().0 == imperium_protocol::session::SessionStatus::Working)
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the sessions did not settle");
    }
}

fn drain(
    announcements: &mut tokio::sync::broadcast::Receiver<Notification>,
    method: &str,
) -> Vec<Value> {
    let mut found = Vec::new();
    while let Ok(notification) = announcements.try_recv() {
        if notification.method == method {
            found.push(notification.params);
        }
    }
    found
}

fn outcome(response: Response) -> Result<Value, String> {
    match response.outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => Err(error.message),
    }
}

fn rpc(connection: &mut Connection, method: &str, params: Value) -> Result<Value, String> {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    outcome(handled.response.expect("an immediate reply"))
}

fn connect(daemon: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(daemon));
    rpc(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    )
    .unwrap();
    connection
}

// --- suggest -----------------------------------------------------------------------------

#[tokio::test]
async fn suggest_mode_records_suggestions_and_starts_nothing() {
    let fixture = fixture(Initiative::Suggest, "").await;
    let mut announcements = fixture.daemon.announcements();

    let reviewed = fixture.daemon.initiative_tick().await;
    assert_eq!(reviewed, vec![fixture.env.id.clone()]);

    // The agent tried to start a session during the review and was refused: no session besides
    // the agent exists.
    assert!(fixture.other_sessions().is_empty());
    assert!(
        fixture
            .tool_results()
            .iter()
            .any(|result| result.contains("plan mode, which only reads")),
        "{:?}",
        fixture.tool_results()
    );

    let state = imperium_core_state(&fixture);
    assert_eq!(state["mode"], "suggest");
    let suggestions = state["suggestions"].as_array().unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0]["title"], "Improve the timeline");
    assert_eq!(suggestions[0]["prompt"], "Improve my timeline");
    assert_eq!(suggestions[0]["source"], SOURCE);
    assert_eq!(suggestions[0]["status"], "open");
    assert_eq!(state["digest"]["items"][0]["source"], SOURCE);
    assert_eq!(state["started_today"], 0);

    // The user hears about both.
    let notices = drain(&mut announcements, "initiative.notice");
    let kinds: Vec<&str> = notices
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"suggestions") && kinds.contains(&"digest"),
        "{kinds:?}"
    );
    assert!(!kinds.contains(&"started"));

    // The agent's permission mode is put back after the review.
    let agent = fixture
        .daemon
        .project_agents()
        .environment(&fixture.env.id)
        .unwrap();
    assert_eq!(
        agent.permission_mode(),
        imperium_api::PermissionMode::Default
    );

    // Nothing is due again until the interval passes.
    assert!(fixture.daemon.initiative_tick().await.is_empty());
    assert_eq!(fixture.reviews_seen(), 1);
}

fn imperium_core_state(fixture: &Fixture) -> Value {
    let mut connection = connect(&fixture.daemon);
    rpc(
        &mut connection,
        "initiative.get",
        json!({"environment_id": fixture.env.id}),
    )
    .unwrap()
}

#[tokio::test]
async fn the_user_starts_a_suggestion_with_one_call_and_dismisses_another() {
    let fixture = fixture(Initiative::Suggest, "").await;
    fixture.daemon.initiative_tick().await;
    let state = imperium_core_state(&fixture);
    let id = state["suggestions"][0]["id"].as_str().unwrap().to_string();

    let mut connection = connect(&fixture.daemon);
    let started = rpc(
        &mut connection,
        "initiative.start",
        json!({"environment_id": fixture.env.id, "suggestion_id": id}),
    )
    .unwrap();
    let session_id = started["session_id"].as_str().unwrap().to_string();

    let others = fixture.other_sessions();
    assert_eq!(others.len(), 1);
    let summary = &others[0];
    assert_eq!(summary.session_id, session_id);
    // The user's own session: not background, in their permission mode, tagged as a suggestion.
    assert!(!summary.background);
    let by = summary.started_by.as_ref().unwrap();
    assert_eq!(by.kind, "suggestion");
    assert_eq!(by.source.as_deref(), Some(SOURCE));
    let first = fixture
        .log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|body| {
            let messages = body["messages"].as_array()?;
            let system = content(&messages[0]);
            (!system.contains("persistent agent")).then(|| content(messages.last()?))
        })
        .next()
        .expect("the session called the model");
    assert!(first.starts_with("Improve my timeline"), "{first}");
    assert!(first.contains(SOURCE), "{first}");

    // It is no longer open, and cannot be started or dismissed twice.
    let state = imperium_core_state(&fixture);
    assert_eq!(state["suggestions"][0]["status"], "started");
    assert_eq!(state["suggestions"][0]["session_id"], session_id.as_str());
    assert!(
        rpc(
            &mut connection,
            "initiative.dismiss",
            json!({"environment_id": fixture.env.id, "suggestion_id": id}),
        )
        .is_err()
    );
}

// --- act ---------------------------------------------------------------------------------

#[tokio::test]
async fn act_mode_starts_at_most_the_configured_number_of_tagged_sessions() {
    let fixture = fixture(
        Initiative::Act,
        "[initiative]\nmax_sessions_per_day = 2\nmax_concurrent = 5\nidle_min = 0\n",
    )
    .await;
    let mut announcements = fixture.daemon.announcements();

    assert_eq!(fixture.daemon.initiative_tick().await.len(), 1);
    fixture.wait_until_idle().await;

    // The agent asked for three; the daily limit allows two.
    let started = fixture.other_sessions();
    assert_eq!(started.len(), 2, "{started:#?}");
    assert_eq!(
        fixture.daemon.initiative().started_today(&fixture.env.id),
        2
    );
    assert!(
        fixture
            .tool_results()
            .iter()
            .any(|result| result.contains("initiative.max_sessions_per_day")),
        "{:?}",
        fixture.tool_results()
    );

    for summary in &started {
        // Tagged, in the default permission mode, and background (it gives way to the user).
        let by = summary.started_by.as_ref().expect("tagged");
        assert_eq!(by.kind, "initiative");
        assert_eq!(by.environment_name, "Imperium");
        assert_eq!(by.reason, "the roadmap's next item is unstarted");
        assert_eq!(by.source.as_deref(), Some(SOURCE));
        assert!(summary.background);
        assert_eq!(summary.permission_mode.as_deref(), Some("default"));
        assert_eq!(
            summary.environment_id.as_deref(),
            Some(fixture.env.id.as_str())
        );
    }

    // G2: each session's first message is the user-like prompt the agent wrote, naming the
    // source it read.
    let prompts: Vec<String> = fixture
        .log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|body| {
            let messages = body["messages"].as_array()?;
            (!content(&messages[0]).contains("persistent agent")).then(|| content(&messages[1]))
        })
        .collect();
    assert!(prompts.len() >= 2, "{prompts:?}");
    for prompt in prompts {
        assert!(
            prompt.starts_with("Start the next unfinished item"),
            "{prompt}"
        );
        assert!(prompt.contains(SOURCE), "{prompt}");
    }

    // The review message told the agent to write as the user would and to cite what it read.
    let review = fixture
        .log
        .lock()
        .unwrap()
        .iter()
        .flat_map(|body| body["messages"].as_array().cloned().unwrap_or_default())
        .map(|m| content(&m))
        .find(|text| text.starts_with("Initiative review for the environment"))
        .unwrap();
    assert!(
        review.contains("the way the user would type it"),
        "{review}"
    );
    assert!(review.contains("Never invent a task"), "{review}");

    // The user is told about each one.
    let notices = drain(&mut announcements, "initiative.notice");
    let told: Vec<_> = notices
        .iter()
        .filter(|n| n["kind"] == serde_json::to_value(NoticeKind::Started).unwrap())
        .collect();
    assert_eq!(told.len(), 2, "{notices:?}");
    assert!(told[0]["session_id"].is_string());
}

#[tokio::test]
async fn act_mode_runs_one_session_at_a_time_and_a_follow_up_follows_the_same_rules() {
    let fixture = fixture(
        Initiative::Act,
        "[initiative]\nmax_sessions_per_day = 5\nmax_concurrent = 1\nidle_min = 0\n",
    )
    .await;
    assert_eq!(fixture.daemon.initiative_tick().await.len(), 1);
    // While the first session runs, the review's other two asks are held back.
    assert!(
        fixture
            .tool_results()
            .iter()
            .any(|result| result.contains("initiative.max_concurrent")),
        "{:?}",
        fixture.tool_results()
    );
    fixture.wait_until_idle().await;

    // The first session, and the one the agent started after its report (a turn that follows
    // the same rules: tagged as the agent's initiative, counted against the day).
    let started = fixture.other_sessions();
    assert_eq!(started.len(), 2, "{started:#?}");
    assert!(started.iter().all(|s| {
        s.started_by
            .as_ref()
            .is_some_and(|by| by.kind == "initiative")
    }));
    assert_eq!(
        fixture.daemon.initiative().started_today(&fixture.env.id),
        2
    );
}

#[tokio::test]
async fn a_report_of_a_started_session_cannot_start_more_than_the_daily_limit() {
    let fixture = fixture(
        Initiative::Act,
        "[initiative]\nmax_sessions_per_day = 1\nmax_concurrent = 3\nidle_min = 0\n",
    )
    .await;
    assert_eq!(fixture.daemon.initiative_tick().await.len(), 1);
    fixture.wait_until_idle().await;
    assert_eq!(fixture.other_sessions().len(), 1);
    // The agent's follow-up turn asked for another session and was refused.
    assert!(
        fixture
            .tool_results()
            .iter()
            .any(|result| result.contains("initiative.max_sessions_per_day")),
        "{:?}",
        fixture.tool_results()
    );
    assert_eq!(
        fixture.daemon.initiative().started_today(&fixture.env.id),
        1
    );
}

#[tokio::test]
async fn act_mode_falls_back_to_suggesting_once_the_day_is_used_up() {
    let fixture = fixture(
        Initiative::Act,
        "[initiative]\nmax_sessions_per_day = 0\nidle_min = 0\n",
    )
    .await;
    assert_eq!(fixture.daemon.initiative_tick().await.len(), 1);
    // No allowance: the review is a suggest review (read-only), so nothing starts.
    assert!(fixture.other_sessions().is_empty());
    let state = imperium_core_state(&fixture);
    assert_eq!(state["suggestions"].as_array().unwrap().len(), 1);
}

// --- when it may run -----------------------------------------------------------------------

#[tokio::test]
async fn nothing_happens_while_the_machine_or_the_environment_is_busy() {
    let fixture = fixture(Initiative::Suggest, "").await;

    // A busy machine: no review, no model call.
    fixture.load.set(Load::Busy);
    assert!(fixture.daemon.initiative_tick().await.is_empty());
    assert_eq!(fixture.reviews_seen(), 0);
    let state = imperium_core_state(&fixture);
    assert_eq!(state["waiting"], "the machine is busy");

    // Quiet, but the user was just working in the environment (their session wrote its journal
    // a moment ago): the environment is not idle.
    fixture.load.set(Load::Quiet);
    let (session, _events) = fixture
        .daemon
        .create_in_environment(
            &fixture.env.id,
            None,
            imperium_core::SessionOptions::default(),
        )
        .unwrap();
    session.send("hello".into()).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(fixture.daemon.initiative_tick().await.is_empty());
    assert_eq!(fixture.reviews_seen(), 0);
    let state = imperium_core_state(&fixture);
    assert_eq!(state["waiting"], "you are working in this environment");

    // A person's own request does not wait.
    let outcome = fixture
        .daemon
        .initiative_review_now(&fixture.env.id)
        .await
        .unwrap();
    assert_eq!(outcome.suggestions, 1);
    assert_eq!(fixture.reviews_seen(), 1);
}

#[tokio::test]
async fn off_mode_is_the_agent_as_before() {
    let fixture = fixture(Initiative::Off, "").await;
    assert!(fixture.daemon.initiative_tick().await.is_empty());
    assert_eq!(fixture.reviews_seen(), 0);
    assert!(
        fixture
            .daemon
            .initiative_review_now(&fixture.env.id)
            .await
            .unwrap_err()
            .contains("off")
    );

    // The agent still starts sessions when the user asks, with no initiative limits and no
    // suggestions made.
    let agent = fixture
        .daemon
        .project_agents()
        .environment(&fixture.env.id)
        .unwrap();
    agent.send("please start a session".into()).unwrap();
    fixture.wait_until_idle().await;
    let started = fixture.other_sessions();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].started_by.as_ref().unwrap().kind, "agent");
    assert_eq!(
        fixture.daemon.initiative().started_today(&fixture.env.id),
        0
    );
    let state = imperium_core_state(&fixture);
    assert_eq!(state["mode"], "off");
    assert!(state["suggestions"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_used_up_budget_stops_the_review_before_it_spends_anything() {
    let fixture = fixture(Initiative::Suggest, "[initiative]\nidle_min = 0\n").await;
    // The user's session spends 10 tokens.
    let (session, mut events) = fixture
        .daemon
        .create_in_environment(
            &fixture.env.id,
            None,
            imperium_core::SessionOptions::default(),
        )
        .unwrap();
    session.send("hello".into()).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let imperium_protocol::session::SessionEvent::TurnCompleted { .. } =
                events.recv().await.unwrap().event
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut connection = connect(&fixture.daemon);
    rpc(
        &mut connection,
        "budgets.set",
        json!({
            "week_start": "monday", "month_start_day": 1, "thresholds": [100],
            "limits": [{
                "id": "", "name": "", "scope": "global", "target": "",
                "window": "daily", "tokens": 5, "cost_usd": null,
                "thresholds": null, "action": "block"
            }]
        }),
    )
    .unwrap();
    assert!(fixture.daemon.initiative_tick().await.is_empty());
    assert_eq!(fixture.reviews_seen(), 0);
    let error = fixture
        .daemon
        .initiative_review_now(&fixture.env.id)
        .await
        .unwrap_err();
    assert!(error.to_lowercase().contains("budget"), "{error}");
}

#[test]
fn suggestion_status_names_are_stable_on_the_wire() {
    assert_eq!(
        serde_json::to_value(SuggestionStatus::Open).unwrap(),
        "open"
    );
    assert_eq!(serde_json::to_value(Initiative::Act).unwrap(), "act");
    assert_eq!(Initiative::default(), Initiative::Suggest);
}
