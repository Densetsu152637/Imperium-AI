//! An environment holds zero or more persistent agents. A new one has none; agents of one
//! environment run independently, each with its own name, role and conversation; an environment
//! saved with the old single persistent session gets it back as an agent, history and all; and
//! the deprecated `persistent_agent` field still works for one protocol minor.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<Value>>>;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Opens (`send(true)`) the sessions the fake model holds back.
type Gate = Arc<tokio::sync::watch::Sender<bool>>;

/// Answers "Echo: <last user message>" and records every request. An agent asked to "please
/// start a session" does, and a session told "slow job" answers only once the gate is open.
async fn fake_model(log: Log) -> String {
    fake_model_gated(log, Arc::new(tokio::sync::watch::channel(true).0)).await
}

async fn fake_model_gated(log: Log, gate: Gate) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let log = Arc::clone(&log);
            let gate = Arc::clone(&gate);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let last_user = messages.iter().rposition(|m| m["role"] == "user");
                let said = last_user
                    .and_then(|at| messages[at]["content"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let tools_since = last_user.map_or(0, |at| {
                    messages[at..].iter().filter(|m| m["role"] == "tool").count()
                });
                let system = messages[0]["content"].as_str().unwrap_or_default().to_string();
                log.lock().unwrap().push(body);
                if said.starts_with("slow job") {
                    let _ = gate.subscribe().wait_for(|open| *open).await;
                }
                let reply = if system.contains("persistent agents") && said.starts_with("please start a session") {
                    if tools_since == 0 {
                        let arguments = json!({"brief": said.replace("please start a session", "slow job")});
                        sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
                            "function":{"name":"start_session","arguments":arguments.to_string()}}]}}]}))
                    } else {
                        sse(json!({"choices":[{"delta":{"content":"Started."}}]}))
                    }
                } else {
                    sse(json!({"choices":[{"delta":{"content":format!("Echo: {said}")}}]}))
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

fn try_call(connection: &mut Connection, method: &str, params: Value) -> Result<Value, String> {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match handled.response {
        Some(Response {
            outcome: Outcome::Result(value),
            ..
        }) => Ok(value),
        Some(Response {
            outcome: Outcome::Error(error),
            ..
        }) => Err(error.message),
        None => panic!("no immediate reply to {method}"),
    }
}

fn call(connection: &mut Connection, method: &str, params: Value) -> Value {
    try_call(connection, method, params).unwrap_or_else(|error| panic!("{method}: {error}"))
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

/// Sends a message to a session and returns its reply.
async fn say(state: &Arc<DaemonState>, session_id: &str, text: &str) -> String {
    let session = state.session(session_id).unwrap();
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::TextDelta { text } => reply.push_str(&text),
                SessionEvent::TurnCompleted { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");
    reply
}

fn new_environment(connection: &mut Connection, folder: &std::path::Path) -> String {
    let env = call(
        connection,
        "environments.create",
        json!({"name": "Work", "folders": [folder]}),
    );
    env["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn a_new_environment_has_no_agents_and_nothing_starts() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &fake_model(Arc::default()).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());

    let listed = call(&mut connection, "environments.list", json!({}));
    let env = &listed["environments"][1];
    assert_eq!(env["agents"], json!([]));
    assert_eq!(env["persistent_agent"], false);
    assert!(env["agent_session_id"].is_null());
    let agents = call(
        &mut connection,
        "environments.agent_list",
        json!({"environment_id": id}),
    );
    assert_eq!(agents["agents"], json!([]));
    state.start_project_agents();
    assert!(state.project_agents().environment_agents().is_empty());
    assert!(state.sessions().is_empty());
}

#[tokio::test]
async fn two_agents_in_one_environment_run_independently() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let log: Log = Arc::default();
    let state = daemon(home.path(), &fake_model(Arc::clone(&log)).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());

    let planner = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Planner", "role": "Keep the roadmap current."}),
    );
    let reviewer = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Reviewer", "role": "Critique open designs."}),
    );
    assert_ne!(planner["session_id"], reviewer["session_id"]);
    assert_eq!(state.project_agents().agents_of(&id).len(), 2);
    // The same name twice is refused, ignoring case.
    let duplicate = try_call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "planner"}),
    );
    assert!(duplicate.unwrap_err().contains("already has an agent"));

    let planner_session = planner["session_id"].as_str().unwrap().to_string();
    let reviewer_session = reviewer["session_id"].as_str().unwrap().to_string();
    assert_eq!(
        say(&state, &planner_session, "plan the week").await,
        "Echo: plan the week"
    );
    assert_eq!(
        say(&state, &reviewer_session, "review the memory design").await,
        "Echo: review the memory design"
    );

    // Each has its own role in its own instructions, and its conversation holds only what it
    // was told.
    {
        let log = log.lock().unwrap();
        let system_of = |needle: &str| -> String {
            log.iter()
                .filter_map(|body| {
                    let messages = body["messages"].as_array()?;
                    let said = messages
                        .iter()
                        .any(|m| m["content"].as_str().is_some_and(|text| text == needle));
                    said.then(|| {
                        messages[0]["content"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                })
                .next()
                .unwrap()
        };
        let planner_system = system_of("plan the week");
        assert!(
            planner_system.contains("Your name is Planner."),
            "{planner_system}"
        );
        assert!(planner_system.contains("Keep the roadmap current."));
        assert!(!planner_system.contains("Critique open designs."));
        let reviewer_system = system_of("review the memory design");
        assert!(reviewer_system.contains("Your name is Reviewer."));
        assert!(reviewer_system.contains("Critique open designs."));
        let reviewer_saw_planner = log.iter().any(|body| {
            body["messages"].as_array().is_some_and(|messages| {
                messages
                    .iter()
                    .any(|m| m["content"].as_str() == Some("plan the week"))
                    && messages
                        .iter()
                        .any(|m| m["content"].as_str() == Some("review the memory design"))
            })
        });
        assert!(!reviewer_saw_planner, "the agents share no conversation");
    }

    // Deleting one stops and archives it; the other keeps running.
    call(
        &mut connection,
        "environments.agent_delete",
        json!({"environment_id": id, "agent_id": planner["id"]}),
    );
    assert!(state.session(&planner_session).is_none());
    assert!(
        state
            .archived_sessions()
            .iter()
            .any(|s| s.session_id == planner_session)
    );
    assert!(state.session(&reviewer_session).is_some());
    assert_eq!(
        say(&state, &reviewer_session, "still here?").await,
        "Echo: still here?"
    );

    // Renaming restarts the agent with its conversation and shows the new name.
    let renamed = call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": reviewer["id"], "name": "Critic"}),
    );
    assert_eq!(renamed["name"], "Critic");
    assert_eq!(renamed["session_id"], reviewer_session.as_str());
    let session = state.session(&reviewer_session).unwrap();
    assert_eq!(state.summary(&session).title, "Work · Critic");
}

#[tokio::test]
async fn an_old_persistent_session_becomes_an_agent_with_its_history() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let url = fake_model(Arc::default()).await;
    let state = daemon(home.path(), &url);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());
    // The deprecated field still turns an agent on.
    let env = call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": true}),
    );
    assert_eq!(env["agents"].as_array().unwrap().len(), 1);
    assert_eq!(env["agents"][0]["name"], "Persistent agent");
    assert_eq!(env["persistent_agent"], true);
    let session_id = env["agent_session_id"].as_str().unwrap().to_string();
    assert_eq!(env["agents"][0]["session_id"], session_id.as_str());
    say(&state, &session_id, "remember this").await;
    drop((connection, state));

    // Rewrite the saved environments as an older daemon wrote them: one persistent session,
    // no agents.
    let file = home.path().join("data").join("environments.json");
    let file = if file.exists() {
        file
    } else {
        find_file(home.path(), "environments.json")
    };
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for env in saved["environments"].as_array_mut().unwrap() {
        // Only the environment the test made; Home stays as it is.
        if env["builtin"] == true {
            continue;
        }
        let object = env.as_object_mut().unwrap();
        object.remove("agents");
        object.insert("persistent_agent".into(), json!(true));
        object.insert("agent_session_id".into(), json!(session_id));
    }
    std::fs::write(&file, saved.to_string()).unwrap();

    let state = daemon(home.path(), &url);
    let mut connection = connect(&state);
    let listed = call(&mut connection, "environments.list", json!({}));
    let env = &listed["environments"][1];
    assert_eq!(env["agents"].as_array().unwrap().len(), 1, "{env}");
    assert_eq!(env["agents"][0]["session_id"], session_id.as_str());
    state.start_project_agents();
    let session = state.session(&session_id).unwrap();
    assert_eq!(session.title(), "remember this", "its history came back");
    assert!(state.summary(&session).persistent);
    // The migration was saved: another start finds the agent without migrating again.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        saved["environments"][1]["agents"].as_array().unwrap().len(),
        1
    );

    // Turning the deprecated field off deletes the agents.
    call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": false}),
    );
    assert!(state.project_agents().environment_agents().is_empty());
    let env = call(&mut connection, "environments.list", json!({}));
    assert_eq!(env["environments"][1]["agents"], json!([]));
    assert_eq!(env["environments"][1]["persistent_agent"], false);
}

fn find_file(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                return path;
            }
        }
    }
    panic!("no {name} under {}", root.display());
}

// --- review follow-ups ---------------------------------------------------------------------

fn system_prompts_of(log: &Log, needle: &str) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|body| {
            let messages = body["messages"].as_array()?;
            let system = messages[0]["content"].as_str()?.to_string();
            system.contains(needle).then_some(system)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_syncs_start_each_agent_exactly_once() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &fake_model(Arc::default()).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());
    for round in 0..8 {
        state
            .environments()
            .add_agent(&id, &format!("Agent {round}"), None)
            .unwrap();
        // Several places sync at once (an RPC, a restart, the daemon start).
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let state = Arc::clone(&state);
                let id = id.clone();
                tokio::task::spawn_blocking(move || state.sync_environment_agent(&id))
            })
            .collect();
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(state.project_agents().agents_of(&id).len(), round + 1);
        assert_eq!(state.sessions().len(), round + 1, "no duplicate sessions");
    }
    // Every agent's recorded session is the one running, and nothing else is.
    let env = state.environments().get(&id).unwrap();
    let mut recorded: Vec<_> = env
        .agents
        .iter()
        .filter_map(|a| a.session_id.clone())
        .collect();
    let mut running: Vec<_> = state
        .sessions()
        .iter()
        .map(|s| s.id().to_string())
        .collect();
    recorded.sort();
    running.sort();
    assert_eq!(recorded, running);
}

#[tokio::test]
async fn an_unchanged_agent_is_not_restarted_and_a_restart_keeps_its_limits_and_reports() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let log: Log = Arc::default();
    // The sessions the agent starts stay running until the test lets them finish.
    let gate: Gate = Arc::new(tokio::sync::watch::channel(false).0);
    let state = daemon(
        home.path(),
        &fake_model_gated(Arc::clone(&log), Arc::clone(&gate)).await,
    );
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());
    let agent = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Planner", "role": "Plans."}),
    );
    let agent_id = agent["id"].as_str().unwrap().to_string();
    let session_id = agent["session_id"].as_str().unwrap().to_string();
    let first = state.session(&session_id).unwrap();

    // Saving the same name and role (padded) restarts nothing.
    call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": agent_id, "name": " Planner ", "role": "Plans."}),
    );
    call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": agent_id}),
    );
    assert!(Arc::ptr_eq(&first, &state.session(&session_id).unwrap()));

    // One session started (it runs for eight seconds), then the agent is renamed: a restart.
    say(&state, &session_id, "please start a session one").await;
    call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": agent_id, "name": "Planner 2"}),
    );
    let second = state.session(&session_id).unwrap();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "a changed name restarts the agent"
    );
    assert_eq!(state.summary(&second).title, "Work · Planner 2");

    // The count of sessions running carried over: the second start makes two (the limit), so the
    // third is refused. Without it the restart would have reset the count.
    say(&state, &session_id, "please start a session two").await;
    say(&state, &session_id, "please start a session three").await;
    let refused = log.lock().unwrap().iter().any(|body| {
        body["messages"].as_array().is_some_and(|messages| {
            messages.iter().any(|m| {
                m["role"] == "tool"
                    && m["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("sessions are already running"))
            })
        })
    });
    assert!(refused, "the third session is refused: two are running");

    // The first session finishes after the restart; its report reaches the restarted agent.
    gate.send_replace(true);
    let delivered = async {
        loop {
            let seen = log.lock().unwrap().iter().any(|body| {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let system = messages[0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                system.contains("Your name is Planner 2.")
                    && messages.iter().any(|m| {
                        m["role"] == "user"
                            && m["content"].as_str().is_some_and(|text| {
                                text.starts_with("Session ") && text.contains("finished")
                            })
                    })
            });
            if seen {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), delivered)
        .await
        .expect("the report of a session started before the restart reaches the agent");
}

#[tokio::test]
async fn agent_requests_are_validated_and_stay_inside_their_environment() {
    let home = tempfile::tempdir().unwrap();
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let log: Log = Arc::default();
    let state = daemon(home.path(), &fake_model(Arc::clone(&log)).await);
    let mut connection = connect(&state);
    let work = new_environment(&mut connection, one.path());
    let play = call(
        &mut connection,
        "environments.create",
        json!({"name": "Play", "folders": [two.path()]}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Name and role limits, as the daemon enforces them whatever a client checks.
    for (name, role, expected) in [
        ("x".repeat(61), "", "1 to 60"),
        ("   ".to_string(), "", "1 to 60"),
        ("Plan\nner".to_string(), "", "control characters"),
        (
            "Planner".to_string(),
            "r".repeat(4001).leak() as &str,
            "4000",
        ),
    ] {
        let error = try_call(
            &mut connection,
            "environments.agent_create",
            json!({"environment_id": work, "name": name, "role": role}),
        )
        .unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    assert!(state.project_agents().environment_agents().is_empty());

    // A name already used in the environment, in any case, is refused on create and on rename.
    let planner = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": work, "name": "Planner"}),
    );
    let reviewer = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": work, "name": "Reviewer"}),
    );
    let error = try_call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": work, "agent_id": reviewer["id"], "name": "PLANNER"}),
    )
    .unwrap_err();
    assert!(error.contains("already has an agent"), "{error}");
    // The same name in another environment is fine.
    call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": play, "name": "Planner"}),
    );

    // An agent of one environment cannot be changed or deleted through another.
    let update = try_call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": play, "agent_id": reviewer["id"], "name": "Hijacked"}),
    );
    assert!(update.is_err());
    let delete = try_call(
        &mut connection,
        "environments.agent_delete",
        json!({"environment_id": play, "agent_id": reviewer["id"]}),
    );
    assert!(delete.is_err());
    let agents = call(
        &mut connection,
        "environments.agent_list",
        json!({"environment_id": work}),
    );
    let names: Vec<&str> = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Planner", "Reviewer"]);
    assert!(
        state
            .session(reviewer["session_id"].as_str().unwrap())
            .is_some()
    );

    // The agents keep notes apart: each is told its own entry prefix in the shared memory.
    say(&state, planner["session_id"].as_str().unwrap(), "hello").await;
    let prompts = system_prompts_of(&log, "Your name is Planner.");
    assert!(
        prompts[0].contains("\"planner-\"") && prompts[0].contains("planner-open-tasks"),
        "{}",
        prompts[0]
    );
}

#[tokio::test]
async fn a_phone_cannot_manage_agents_and_the_alias_creates_and_never_duplicates() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &fake_model(Arc::default()).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());

    // A phone connection (here limited to what a paired phone may read) is refused every agent
    // method.
    let mut phone =
        Connection::restricted(Arc::clone(&state), vec!["system.handshake", "session.list"]);
    call(
        &mut phone,
        "system.handshake",
        json!({"client": {"name": "phone", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    for method in [
        "environments.agent_list",
        "environments.agent_create",
        "environments.agent_update",
        "environments.agent_delete",
    ] {
        let error = try_call(
            &mut phone,
            method,
            json!({"environment_id": id, "agent_id": "x", "name": "Spy"}),
        )
        .unwrap_err();
        assert!(error.contains("not allowed"), "{method}: {error}");
    }
    assert!(state.environments().get(&id).unwrap().agents.is_empty());

    // The deprecated field on create makes one default agent; "on" again adds no second.
    let created = call(
        &mut connection,
        "environments.create",
        json!({"name": "Aliased", "folders": [folder.path()], "persistent_agent": true}),
    );
    assert_eq!(created["agents"].as_array().unwrap().len(), 1);
    assert_eq!(created["agents"][0]["name"], "Persistent agent");
    let again = call(
        &mut connection,
        "environments.update",
        json!({"id": created["id"], "persistent_agent": true}),
    );
    assert_eq!(again["agents"].as_array().unwrap().len(), 1);
    assert_eq!(again["agents"][0]["id"], created["agents"][0]["id"]);

    // Off is refused when the environment has agents set up by name or role.
    call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": created["id"], "name": "Planner"}),
    );
    let error = try_call(
        &mut connection,
        "environments.update",
        json!({"id": created["id"], "persistent_agent": false}),
    )
    .unwrap_err();
    assert!(error.contains("agent_delete"), "{error}");
    assert_eq!(
        state
            .environments()
            .get(created["id"].as_str().unwrap())
            .unwrap()
            .agents
            .len(),
        2
    );
}

#[tokio::test]
async fn renaming_the_environment_retitles_its_agents() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &fake_model(Arc::default()).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());
    let agent = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Planner"}),
    );
    let session = state
        .session(agent["session_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(state.summary(&session).title, "Work · Planner");
    call(
        &mut connection,
        "environments.update",
        json!({"id": id, "name": "Studio"}),
    );
    assert_eq!(state.summary(&session).title, "Studio · Planner");
}

#[tokio::test]
async fn the_last_agent_deleted_takes_its_conversation_with_it() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &fake_model(Arc::default()).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());
    let agent = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Planner", "role": "Plans."}),
    );
    let old_session = agent["session_id"].as_str().unwrap().to_string();
    say(&state, &old_session, "remember the old plan").await;

    call(
        &mut connection,
        "environments.agent_delete",
        json!({"environment_id": id, "agent_id": agent["id"]}),
    );
    let env = state.environments().get(&id).unwrap();
    assert!(env.agents.is_empty());
    assert!(
        env.agent_session_id.is_none(),
        "no link to the deleted conversation"
    );

    // The deprecated switch makes a fresh agent with a fresh session, not the old conversation
    // under new instructions.
    let again = call(
        &mut connection,
        "environments.update",
        json!({"id": id, "persistent_agent": true}),
    );
    let new_session = again["agents"][0]["session_id"].as_str().unwrap();
    assert_ne!(new_session, old_session);
    let fresh = state.session(new_session).unwrap();
    assert_ne!(fresh.title(), "remember the old plan");
    assert!(
        state
            .archived_sessions()
            .iter()
            .any(|s| s.session_id == old_session)
    );
}

#[tokio::test]
async fn an_agent_that_cannot_start_is_added_and_says_why() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), &fake_model(Arc::default()).await);
    let mut connection = connect(&state);
    // An environment without folders cannot run an agent yet.
    let empty = call(
        &mut connection,
        "environments.create",
        json!({"name": "Empty", "folders": []}),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let created = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": empty, "name": "Planner"}),
    );
    assert_eq!(created["started"], false);
    assert!(
        created["reason"].as_str().unwrap().contains("no folders"),
        "{created}"
    );
    assert!(created["session_id"].is_null());
    assert_eq!(created["name"], "Planner");
    // Adding a folder starts it.
    call(
        &mut connection,
        "environments.add_folder",
        json!({"id": empty, "folder": folder.path()}),
    );
    let listed = call(
        &mut connection,
        "environments.agent_list",
        json!({"environment_id": empty}),
    );
    assert!(listed["agents"][0]["session_id"].is_string(), "{listed}");

    // One that starts says so.
    let work = new_environment(&mut connection, folder.path());
    let started = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": work, "name": "Reviewer"}),
    );
    assert_eq!(started["started"], true);
    assert!(started.get("reason").is_none());
}

// --- agent permission profiles -------------------------------------------------------------

/// The tools the model was offered on the request whose last user message is `said`.
fn tools_offered_for(log: &Log, said: &str) -> Option<Vec<String>> {
    log.lock().unwrap().iter().find_map(|body| {
        let messages = body["messages"].as_array()?;
        let last_user = messages.iter().rev().find(|m| m["role"] == "user")?;
        if last_user["content"].as_str()? != said {
            return None;
        }
        Some(
            body["tools"]
                .as_array()?
                .iter()
                .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_string))
                .collect(),
        )
    })
}

async fn tools_offered_after(log: &Log, said: &str) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(tools) = tools_offered_for(log, said) {
                return tools;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model was asked {said:?} within 30 s"))
}

#[tokio::test]
async fn an_agent_profile_is_stored_applied_live_and_inherited_by_the_sessions_it_starts() {
    let home = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let log: Log = Arc::default();
    let state = daemon(home.path(), &fake_model(Arc::clone(&log)).await);
    let mut connection = connect(&state);
    let id = new_environment(&mut connection, folder.path());

    // A refused profile adds no agent.
    let refused = try_call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Bad",
               "permissions": {"mcp": "named", "mcp_servers": ["bad\u{0}name"]}}),
    );
    assert!(refused.unwrap_err().contains("not a valid name"));
    let none = call(
        &mut connection,
        "environments.agent_list",
        json!({"environment_id": id}),
    );
    assert_eq!(none["agents"], json!([]));

    // No profile: no extra restriction. A profile is normalised and kept with the agent.
    let open = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Open"}),
    );
    assert_eq!(
        open["permissions"],
        json!({"mcp": "all", "mcp_servers": [], "plugins": "all", "plugin_names": [],
               "local_files_only": false})
    );
    let locked = call(
        &mut connection,
        "environments.agent_create",
        json!({"environment_id": id, "name": "Locked",
               "permissions": {"local_files_only": true, "mcp": "named",
                               "mcp_servers": [" github ", "GitHub"]}}),
    );
    assert_eq!(locked["permissions"]["local_files_only"], true);
    assert_eq!(locked["permissions"]["mcp_servers"], json!(["github"]));
    let listed = call(
        &mut connection,
        "environments.agent_list",
        json!({"environment_id": id}),
    );
    assert_eq!(listed["agents"][1]["permissions"], locked["permissions"]);

    // Each agent's own session is offered what its profile allows.
    let open_session = open["session_id"].as_str().unwrap().to_string();
    let locked_session = locked["session_id"].as_str().unwrap().to_string();
    say(&state, &open_session, "hello open").await;
    say(&state, &locked_session, "hello locked").await;
    let open_tools = tools_offered_for(&log, "hello open").unwrap();
    assert!(open_tools.iter().any(|tool| tool == "run_command"));
    let locked_tools = tools_offered_for(&log, "hello locked").unwrap();
    assert!(locked_tools.iter().any(|tool| tool == "read_file"));
    assert!(!locked_tools.iter().any(|tool| tool == "run_command"));

    // The sessions an agent starts keep its profile: the locked agent's session has no
    // commands, the open agent's has them.
    say(&state, &locked_session, "please start a session probe").await;
    say(&state, &open_session, "please start a session free").await;
    let started = tools_offered_after(&log, "slow job probe").await;
    assert!(started.iter().any(|tool| tool == "read_file"));
    assert!(
        !started.iter().any(|tool| tool == "run_command"),
        "a session started by the locked agent has commands: {started:?}"
    );
    let free = tools_offered_after(&log, "slow job free").await;
    assert!(free.iter().any(|tool| tool == "run_command"));
    // The binding is recorded with the session, so it survives a restart.
    let bound: Vec<String> = state
        .sessions()
        .iter()
        // The agents' own sessions are not remembered; the ones they started are.
        .filter(|session| session.id() != open_session && session.id() != locked_session)
        .filter_map(|session| {
            let meta = std::fs::read_to_string(find_file(
                home.path(),
                &format!("{}.meta.json", session.id()),
            ))
            .ok()?;
            let meta: Value = serde_json::from_str(&meta).ok()?;
            meta["agent_id"].as_str().map(str::to_string)
        })
        .collect();
    assert_eq!(
        bound
            .iter()
            .filter(|agent| **agent == locked["id"].as_str().unwrap())
            .count(),
        1,
        "{bound:?}"
    );

    // A change applies at once, without restarting the agent.
    let before = state.session(&locked_session).unwrap();
    call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": locked["id"],
               "permissions": {"mcp": "all", "plugins": "all", "local_files_only": false}}),
    );
    assert!(Arc::ptr_eq(
        &before,
        &state.session(&locked_session).unwrap()
    ));
    say(&state, &locked_session, "hello again").await;
    let widened = tools_offered_for(&log, "hello again").unwrap();
    assert!(widened.iter().any(|tool| tool == "run_command"));
    call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": open["id"],
               "permissions": {"plugins": "none", "local_files_only": true}}),
    );
    say(&state, &open_session, "hello narrowed").await;
    let narrowed = tools_offered_for(&log, "hello narrowed").unwrap();
    assert!(!narrowed.iter().any(|tool| tool == "run_command"));
    assert!(narrowed.iter().any(|tool| tool == "read_file"));
    // Leaving `permissions` out of an update keeps the profile.
    let kept = call(
        &mut connection,
        "environments.agent_update",
        json!({"environment_id": id, "agent_id": open["id"], "role": "Still locked."}),
    );
    assert_eq!(kept["permissions"]["local_files_only"], true);
}
