//! The built-in Home environment: always there (once, first, across restarts), never deleted,
//! the default for a session started with no environment or project, working in the session's
//! own scratch folder with no project paths, no persistent agent, and web tools that follow the
//! permission mode while local files outside the working folder stay out of reach.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_api::PermissionMode;
use imperium_core::{Connection, DaemonState, SessionOptions};
use imperium_protocol::jsonrpc::{ErrorObject, Outcome};
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// What the fake model saw and does: every request's tool names, and every tool result.
#[derive(Default)]
struct Seen {
    tool_names: Vec<Vec<String>>,
    tool_results: Vec<String>,
    /// The system prompt of the latest request.
    system: String,
}

/// Calls `tool` with `arguments` once, then answers "Done."
async fn fake_model(seen: Arc<Mutex<Seen>>, tool: &'static str, arguments: String) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let seen = Arc::clone(&seen);
            let arguments = arguments.clone();
            async move {
                let names = body["tools"]
                    .as_array()
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(|tool| {
                                tool["function"]["name"].as_str().map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let results: Vec<String> = messages
                    .iter()
                    .filter(|m| m["role"] == "tool")
                    .filter_map(|m| m["content"].as_str().map(str::to_string))
                    .collect();
                {
                    let mut seen = seen.lock().unwrap();
                    seen.tool_names.push(names);
                    seen.tool_results = results.clone();
                    seen.system = messages
                        .first()
                        .and_then(|m| m["content"].as_str())
                        .unwrap_or_default()
                        .to_string();
                }
                let reply = if results.is_empty() {
                    sse(
                        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1",
                        "function":{"name":tool,"arguments":arguments}}]}}]}),
                    )
                } else {
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

fn daemon(home: &Path, base_url: &str, extra: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [workflow]\nenabled = false\n{extra}"
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

fn call(
    connection: &mut Connection,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value, ErrorObject> {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match handled.response.expect("an immediate reply").outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => Err(error),
    }
}

fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    call(
        &mut connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    )
    .unwrap();
    connection
}

fn root_of(home: &Path) -> PathBuf {
    home.join("data").join("workspaces")
}

fn homes(listing: &Value) -> Vec<&Value> {
    listing["environments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|env| env["builtin"] == true)
        .collect()
}

#[tokio::test]
async fn home_exists_on_a_fresh_start_and_once_after_every_restart() {
    let home = tempfile::tempdir().unwrap();
    for round in 0..3 {
        let state = daemon(home.path(), "http://127.0.0.1:9/v1", "");
        let mut connection = connect(&state);
        let listing = call(&mut connection, 2, "environments.list", json!({})).unwrap();
        let found = homes(&listing);
        assert_eq!(found.len(), 1, "round {round}");
        assert_eq!(found[0]["id"], "home");
        assert_eq!(found[0]["name"], "Home");
        assert_eq!(found[0]["folders"], json!([]), "no project folders");
        assert_eq!(found[0]["persistent_agent"], false);
        assert!(found[0]["agent_session_id"].is_null());
        assert_eq!(
            listing["environments"][0]["id"], "home",
            "Home is listed first"
        );
    }
}

#[tokio::test]
async fn home_cannot_be_deleted_but_can_be_renamed() {
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1", "");
    let mut connection = connect(&state);
    let refused = call(
        &mut connection,
        2,
        "environments.delete",
        json!({"id": "home"}),
    )
    .unwrap_err();
    assert!(
        refused.message.contains("cannot be deleted"),
        "{}",
        refused.message
    );
    let renamed = call(
        &mut connection,
        3,
        "environments.update",
        json!({"id": "home", "name": "My place"}),
    )
    .unwrap();
    assert_eq!(renamed["name"], "My place");
    assert_eq!(renamed["builtin"], true);
    let listing = call(&mut connection, 4, "environments.list", json!({})).unwrap();
    assert_eq!(homes(&listing).len(), 1);
}

#[tokio::test]
async fn a_session_with_no_environment_or_project_starts_in_home_in_its_scratch_folder() {
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1", "");
    let mut connection = connect(&state);
    let created = call(&mut connection, 2, "session.create", json!({})).unwrap();
    let id = created["session_id"].as_str().unwrap().to_string();
    assert_eq!(created["environment_id"], "home");
    let folder = root_of(home.path()).join("home").join("sessions").join(&id);
    let folders = created["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 1, "the scratch folder is the only folder");
    assert!(folder.is_dir());
    std::fs::write(folder.join("note.txt"), "x").unwrap();

    // Archiving deletes it, unarchiving makes an empty one, and a restart keeps the session.
    state.archive(&id).unwrap();
    assert!(!folder.exists());
    state.unarchive(&id).unwrap();
    assert!(folder.is_dir() && std::fs::read_dir(&folder).unwrap().next().is_none());
    drop(connection);
    drop(state);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let restarted = daemon(home.path(), "http://127.0.0.1:9/v1", "");
    assert_eq!(restarted.resume_sessions(), 1);
    assert!(restarted.session(&id).is_some());
    assert!(folder.is_dir());
    let listing = restarted.environment_list();
    assert_eq!(
        listing
            .environments
            .iter()
            .filter(|env| env.builtin)
            .count(),
        1
    );

    // Naming an environment or a project still works as before.
    let project = tempfile::tempdir().unwrap();
    let mut connection = connect(&restarted);
    let there = call(
        &mut connection,
        2,
        "session.create",
        json!({"project_dir": project.path()}),
    )
    .unwrap();
    assert_ne!(there["environment_id"], "home");
}

#[tokio::test]
async fn home_sessions_have_no_project_paths_and_start_no_persistent_agent() {
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1", "");
    state.start_project_agents();
    assert!(state.project_agents().environments().is_empty());
    let (session, _events) = state
        .create_in_environment("home", None, SessionOptions::default())
        .unwrap();
    let summary = state.summary(&session);
    let scratch = root_of(home.path())
        .join("home")
        .join("sessions")
        .join(session.id());
    assert_eq!(summary.folders.len(), 1);
    assert!(
        Path::new(&summary.folders[0])
            .ends_with(scratch.strip_prefix(root_of(home.path())).unwrap())
    );
    assert!(state.environments().get("home").unwrap().folders.is_empty());
    // A worktree needs a project, which Home does not have.
    assert!(
        state
            .create_in_environment(
                "home",
                None,
                SessionOptions {
                    worktree: true,
                    ..SessionOptions::default()
                }
            )
            .is_err()
    );
}

async fn run_turn(
    session: &Arc<imperium_engine::Session>,
    answer: PermissionDecision,
) -> (Vec<String>, Vec<SessionEvent>) {
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    let mut asked = Vec::new();
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("an event within 20 s")
            .unwrap()
            .event;
        if let SessionEvent::PermissionRequested {
            request_id, tool, ..
        } = &event
        {
            asked.push(tool.clone());
            session.respond(*request_id, answer).unwrap();
        }
        let done = matches!(
            event,
            SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
        );
        seen.push(event);
        if done {
            return (asked, seen);
        }
    }
}

#[tokio::test]
async fn web_tools_are_offered_and_follow_the_permission_mode_while_local_files_stay_out_of_reach()
{
    let home = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = fake_model(
        Arc::clone(&seen),
        "web_fetch",
        json!({"url": "https://example.com/page"}).to_string(),
    )
    .await;
    let state = daemon(home.path(), &url, "");

    // In the default mode an unlisted site asks first; the user says no.
    let (asking, _events) = state
        .create_in_environment("home", None, SessionOptions::default())
        .unwrap();
    asking.set_permission_mode(PermissionMode::Default);
    let (asked, _) = run_turn(&asking, PermissionDecision::Deny).await;
    assert_eq!(asked, vec!["web_fetch".to_string()]);
    assert!(
        seen.lock().unwrap().tool_names[0].contains(&"web_fetch".to_string()),
        "web_fetch is offered to Home's agents"
    );

    // In plan mode the same call is refused without asking.
    *seen.lock().unwrap() = Seen::default();
    let (planning, _events) = state
        .create_in_environment("home", None, SessionOptions::default())
        .unwrap();
    planning.set_permission_mode(PermissionMode::Plan);
    let (asked, _) = run_turn(&planning, PermissionDecision::Deny).await;
    assert!(asked.is_empty(), "plan mode does not ask: {asked:?}");
    let results = seen.lock().unwrap().tool_results.clone();
    assert!(
        results
            .iter()
            .any(|result| result.to_lowercase().contains("plan")),
        "{results:?}"
    );
}

#[tokio::test]
async fn web_tools_follow_the_web_setting_and_a_file_outside_the_working_folder_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "not for Home").unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = fake_model(
        Arc::clone(&seen),
        "read_file",
        json!({"path": outside.path().join("secret.txt")}).to_string(),
    )
    .await;
    let state = daemon(home.path(), &url, "[web]\nenabled = false\n");
    let (session, _events) = state
        .create_in_environment("home", None, SessionOptions::default())
        .unwrap();
    let (asked, _) = run_turn(&session, PermissionDecision::Deny).await;
    assert!(asked.is_empty());
    let seen = seen.lock().unwrap();
    assert!(
        !seen.tool_names[0].contains(&"web_fetch".to_string()),
        "web tools are off when the setting is off"
    );
    assert!(
        seen.tool_results
            .iter()
            .all(|result| !result.contains("not for Home")),
        "{:?}",
        seen.tool_results
    );
    assert!(
        seen.tool_results
            .iter()
            .any(|result| result.to_lowercase().contains("outside")),
        "{:?}",
        seen.tool_results
    );
}

#[tokio::test]
async fn home_memory_md_is_read_into_home_sessions_only() {
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = fake_model(
        Arc::clone(&seen),
        "list_dir",
        json!({"path": "."}).to_string(),
    )
    .await;
    let state = daemon(home.path(), &url, "");
    std::fs::create_dir_all(root_of(home.path()).join("home")).unwrap();
    std::fs::write(
        root_of(home.path()).join("home").join("Memory.md"),
        "## errands\nThe weekly shop list lives in the shared spreadsheet.\n",
    )
    .unwrap();
    let (session, _events) = state
        .create_in_environment("home", None, SessionOptions::default())
        .unwrap();
    run_turn(&session, PermissionDecision::Deny).await;
    {
        let seen = seen.lock().unwrap();
        assert!(
            seen.system.contains("<environment-memory>"),
            "{}",
            seen.system
        );
        assert!(seen.system.contains("weekly shop list"));
    }
    // Another environment's sessions do not see it.
    let env = state
        .environments()
        .for_folder(&other.path().display().to_string())
        .unwrap();
    let (elsewhere, _events) = state
        .create_in_environment(&env.id, None, SessionOptions::default())
        .unwrap();
    run_turn(&elsewhere, PermissionDecision::Deny).await;
    assert!(!seen.lock().unwrap().system.contains("weekly shop list"));
}

/// An MCP server over HTTP with `lookup` (allowed in the test) and `delete` (not).
async fn mcp_server() -> String {
    let app = Router::new().route(
        "/mcp",
        post(move |axum::Json(request): axum::Json<Value>| async move {
            let Some(id) = request.get("id").cloned() else {
                return axum::Json(json!({}));
            };
            let result = match request["method"].as_str() {
                Some("initialize") => json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
                Some("tools/list") => json!({"tools": [
                    {"name": "lookup", "description": "Look up a word", "inputSchema": {"type": "object"}},
                    {"name": "delete", "description": "Delete a word", "inputSchema": {"type": "object"}}]}),
                _ => json!({"content": [{"type": "text", "text": "ok"}]}),
            };
            axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
async fn connector_tools_are_offered_to_home_and_follow_the_permission_mode() {
    let home = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = fake_model(
        Arc::clone(&seen),
        "mcp__words__delete",
        json!({"word": "x"}).to_string(),
    )
    .await;
    let mcp = mcp_server().await;
    let extra = format!("[mcp.servers.words]\nurl = \"{mcp}\"\nallow = [\"lookup\"]\n");
    let state = daemon(home.path(), &url, &extra);

    // In the default mode a tool that is not allowed asks first; the user says no.
    let (asking, _events) = state
        .create_in_environment("home", None, SessionOptions::default())
        .unwrap();
    asking.set_permission_mode(PermissionMode::Default);
    let (asked, _) = run_turn(&asking, PermissionDecision::Deny).await;
    assert_eq!(asked, vec!["mcp__words__delete".to_string()]);
    {
        let names = seen.lock().unwrap().tool_names[0].clone();
        assert!(
            names.contains(&"mcp__words__lookup".to_string()),
            "{names:?}"
        );
    }

    // In plan mode the same call is refused without asking.
    *seen.lock().unwrap() = Seen::default();
    let (planning, _events) = state
        .create_in_environment("home", None, SessionOptions::default())
        .unwrap();
    planning.set_permission_mode(PermissionMode::Plan);
    let (asked, _) = run_turn(&planning, PermissionDecision::Deny).await;
    assert!(asked.is_empty(), "plan mode does not ask: {asked:?}");
    let results = seen.lock().unwrap().tool_results.clone();
    assert!(
        results
            .iter()
            .any(|result| result.to_lowercase().contains("plan")),
        "{results:?}"
    );
}

#[tokio::test]
async fn a_home_session_that_cannot_start_leaves_no_folder_behind() {
    let home = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1", "");
    let refused =
        state.create_in_environment("home", Some("nowhere:model"), SessionOptions::default());
    assert!(refused.is_err());
    let sessions = root_of(home.path()).join("home").join("sessions");
    let left = std::fs::read_dir(&sessions).map_or(0, |entries| entries.count());
    assert_eq!(
        left, 0,
        "no scratch folder for a session that never started"
    );
}
