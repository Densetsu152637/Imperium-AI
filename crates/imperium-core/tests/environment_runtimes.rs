//! Environments with a remote runtime run their sessions' tools through it (here an SSH
//! environment and a fake runner), `environments.test_runtime` reports what it found, and an
//! environment's persistent agent session is listed as persistent, keeps its id with the
//! environment, and continues its conversation after a restart.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use imperium_tools::runtime::RunFuture;
use imperium_tools::{ProcessSpec, RunOutput, Runner};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Runs `uname` once, then answers "Done."; any other conversation gets "Hello.".
async fn fake_model() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let messages = body["messages"].as_array().cloned().unwrap_or_default();
            let last = messages.last().cloned().unwrap_or_default();
            let said = last["content"].as_str().unwrap_or_default();
            let body = if said == "run uname" {
                sse(
                    json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
                    "function":{"name":"run_command","arguments":"{\"command\":\"uname\"}"}}]}}]}),
                )
            } else if last["role"] == "tool" {
                sse(json!({"choices":[{"delta":{"content":"Done."}}]}))
            } else {
                sse(json!({"choices":[{"delta":{"content":"Hello."}}]}))
            };
            ([("content-type", "text/event-stream")], body)
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// Records every process and says it worked, printing `Linux`.
#[derive(Debug, Default)]
struct Fake {
    ran: Mutex<Vec<ProcessSpec>>,
}

impl Runner for Fake {
    fn run<'a>(
        &'a self,
        spec: &'a ProcessSpec,
        _stdin: Option<&'a [u8]>,
        _timeout: Duration,
    ) -> RunFuture<'a> {
        self.ran.lock().unwrap().push(spec.clone());
        Box::pin(async {
            Ok(RunOutput {
                code: Some(0),
                stdout: b"Linux\n".to_vec(),
                stderr: Vec::new(),
            })
        })
    }
}

fn daemon(home: &std::path::Path, base_url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false"
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
    DaemonState::new(imperium_config::Paths::under(home), settings, None)
}

fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    call(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    connection
}

fn outcome(response: Response) -> Value {
    match response.outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{error:?}"),
    }
}

fn call(connection: &mut Connection, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    outcome(
        handled
            .response
            .unwrap_or_else(|| panic!("no immediate reply to {method}")),
    )
}

async fn call_later(connection: &mut Connection, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let deferred = handled.deferred.expect("a deferred reply");
    outcome(deferred.await)
}

#[tokio::test]
async fn an_ssh_environment_runs_commands_over_ssh_and_its_runtime_can_be_tested() {
    let home = tempfile::tempdir().unwrap();
    let url = fake_model().await;
    let state = daemon(home.path(), &url);
    let fake = Arc::new(Fake::default());
    state.use_runner(fake.clone());
    let mut connection = connect(&state);

    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Box", "folders": ["/srv/app"],
               "runtime": {"kind": "ssh", "host": "me@box:2222"}}),
    );
    assert_eq!(
        env["runtime"],
        json!({"kind": "ssh", "host": "me@box:2222"})
    );
    assert_eq!(env["folders"], json!(["/srv/app"]));
    let id = env["id"].as_str().unwrap().to_string();

    let tested = call_later(
        &mut connection,
        "environments.test_runtime",
        json!({"id": id}),
    )
    .await;
    assert_eq!(tested["ok"], true, "{tested}");
    assert_eq!(
        fake.ran.lock().unwrap().last().unwrap().to_string(),
        "ssh -o BatchMode=yes -o ConnectTimeout=10 -T -p 2222 me@box true"
    );

    let created = call(
        &mut connection,
        "session.create",
        json!({"environment_id": id}),
    );
    assert_eq!(created["isolation"], "runtime · ssh me@box:2222");
    assert_eq!(created["folders"], json!(["/srv/app"]));
    let session = state
        .session(created["session_id"].as_str().unwrap())
        .unwrap();
    let mut events = session.subscribe();
    session.send("run uname".into()).unwrap();
    let mut output = None;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::PermissionRequested { request_id, .. } => {
                    session
                        .respond(
                            request_id,
                            imperium_protocol::session::PermissionDecision::Allow,
                        )
                        .unwrap();
                }
                SessionEvent::ToolFinished { output: text, .. } => output = Some(text),
                SessionEvent::TurnCompleted { .. } => break,
                SessionEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");
    assert!(output.unwrap().contains("Linux"));
    let last = fake.ran.lock().unwrap().last().unwrap().clone();
    assert_eq!(last.program, "ssh");
    assert_eq!(
        last.args.last().unwrap(),
        "sh -c 'cd '\\''/srv/app'\\'' && uname'"
    );
}

#[tokio::test]
async fn a_persistent_agent_is_listed_first_class_and_continues_after_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let url = fake_model().await;
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state);
    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Work", "folders": [project.path()]}),
    );
    let id = env["id"].as_str().unwrap().to_string();
    assert_eq!(env["persistent_agent"], false);
    assert!(state.project_agents().environments().is_empty());

    let env = call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": true}),
    );
    let agent_id = env["agent_session_id"].as_str().unwrap().to_string();
    let listed = call(&mut connection, "session.list", json!({}));
    let row = &listed["sessions"][0];
    assert_eq!(row["session_id"], agent_id.as_str());
    assert_eq!(row["persistent"], true);
    assert_eq!(row["environment_id"], id.as_str());

    // A conversation that should survive the restart.
    let agent = state.session(&agent_id).unwrap();
    let mut events = agent.subscribe();
    agent.send("remember this".into()).unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !matches!(
            events.recv().await.unwrap().event,
            SessionEvent::TurnCompleted { .. }
        ) {}
    })
    .await
    .unwrap();
    drop((events, agent, connection, state));

    let state = daemon(home.path(), &url);
    assert_eq!(
        state.resume_sessions(),
        0,
        "the agent is not an ordinary session"
    );
    state.start_project_agents();
    let agents = state.project_agents().environments();
    assert_eq!(agents, vec![(id.clone(), agent_id.clone())]);
    let agent = state.session(&agent_id).unwrap();
    assert_eq!(agent.title(), "remember this", "its history came back");
    assert!(state.summary(&agent).persistent);

    // Turned off, it stops and is archived; the environment keeps its id for next time.
    let mut connection = connect(&state);
    let env = call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": false}),
    );
    assert_eq!(env["agent_session_id"], agent_id.as_str());
    assert!(state.session(&agent_id).is_none());
    assert!(state.project_agents().environments().is_empty());
    let archived = state.archived_sessions();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].session_id, agent_id);
    assert!(archived[0].archived);

    // Turned back on, the same session leaves the archive and continues.
    call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": true}),
    );
    assert!(state.archived_sessions().is_empty());
    let agent = state.session(&agent_id).unwrap();
    assert_eq!(agent.title(), "remember this");
    assert!(state.summary(&agent).persistent);
}
