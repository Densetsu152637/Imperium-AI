//! Environments' working folders: a scratch folder per session that archiving deletes and
//! unarchiving makes again, an environment's `Memory.md` that only its own sessions read, and
//! moving the whole tree when `workspaces.root` changes (refused for a folder that is not empty,
//! and the configuration unchanged when it is).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState, SessionOptions};
use imperium_protocol::jsonrpc::{ErrorObject, Outcome, Response};
use imperium_protocol::methods::EnvironmentCreateParams;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// Answers every request "ok" and keeps the system prompt of each.
async fn fake_model(prompts: Arc<Mutex<Vec<String>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let prompts = Arc::clone(&prompts);
            async move {
                let system = body["messages"][0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                prompts.lock().unwrap().push(system);
                (
                    [("content-type", "text/event-stream")],
                    sse(json!({"choices":[{"delta":{"content":"ok"}}]})),
                )
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn settings(base_url: &str) -> imperium_config::Settings {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [workflow]\nenabled = false\n"
    );
    imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap()
}

fn daemon(home: &Path, base_url: &str) -> Arc<DaemonState> {
    DaemonState::new(
        imperium_config::Paths::under(home),
        settings(base_url),
        None,
    )
}

fn environment(state: &DaemonState, name: &str, folder: &Path) -> String {
    state
        .environments()
        .create(EnvironmentCreateParams {
            name: name.into(),
            folders: vec![folder.display().to_string()],
            ..EnvironmentCreateParams::default()
        })
        .unwrap()
        .id
}

fn root_of(home: &Path) -> PathBuf {
    home.join("data").join("workspaces")
}

fn scratch(home: &Path, environment: &str, session: &str) -> PathBuf {
    root_of(home)
        .join(environment)
        .join("sessions")
        .join(session)
}

async fn one_turn(state: &Arc<DaemonState>, environment: &str) -> String {
    let (session, events) = state
        .create_in_environment(environment, None, SessionOptions::default())
        .unwrap();
    session.send("hello".into()).unwrap();
    state
        .wait_for_turn(&session, events, Duration::from_secs(20))
        .await
        .unwrap();
    session.id().to_string()
}

fn handshake(connection: &mut Connection) {
    call(
        connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    )
    .unwrap();
}

fn call(
    connection: &mut Connection,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value, ErrorObject> {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let Some(Response { outcome, .. }) = handled.response else {
        panic!("no immediate reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => Err(error),
    }
}

fn seed_session_files(home: &Path, environment: &str, session: &str) {
    let dir = scratch(home, environment, session);
    std::fs::create_dir_all(dir.join("deep")).unwrap();
    std::fs::write(dir.join("notes.txt"), "scratch").unwrap();
    std::fs::write(dir.join("deep").join("x.bin"), vec![1u8; 4096]).unwrap();
}

#[tokio::test]
async fn a_session_gets_a_scratch_folder_that_archiving_deletes_and_unarchiving_makes_again() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1");
    let env = environment(&state, "Work", project.path());
    assert!(
        !root_of(home.path()).exists(),
        "making an environment makes no folder"
    );

    let (session, _events) = state
        .create_in_environment(&env, None, SessionOptions::default())
        .unwrap();
    let id = session.id().to_string();
    drop(session);
    let folder = scratch(home.path(), &env, &id);
    assert!(
        folder.is_dir(),
        "{} is made with the session",
        folder.display()
    );
    let layout = state.workspaces_where();
    assert_eq!(layout.environments[0].session_folders, 1);
    assert_eq!(layout.environments[0].environment_id, env);
    assert!(layout.root.ends_with("workspaces"));

    // Another session of the environment keeps its own folder through all of this.
    let (other, _other_events) = state
        .create_in_environment(&env, None, SessionOptions::default())
        .unwrap();
    let other_folder = scratch(home.path(), &env, other.id());
    assert!(other_folder.is_dir());

    seed_session_files(home.path(), &env, &id);
    state.archive(&id).unwrap();
    assert!(!folder.exists(), "archiving deletes the scratch folder");
    assert!(
        other_folder.is_dir(),
        "other sessions' folders are not touched"
    );
    // Memory.md and the environment's folder are not scratch.
    assert!(root_of(home.path()).join(&env).is_dir());

    state.unarchive(&id).unwrap();
    assert!(folder.is_dir(), "unarchiving makes a new scratch folder");
    assert_eq!(
        std::fs::read_dir(&folder).unwrap().count(),
        0,
        "the new folder is empty"
    );

    // Deleting a session deletes its folder, running or archived.
    seed_session_files(home.path(), &env, &id);
    state.delete_session(&id, false).unwrap();
    assert!(!folder.exists());
    state.archive(other.id()).unwrap();
    assert!(!other_folder.exists());
    state.delete_session(other.id(), false).unwrap();
    assert!(!other_folder.exists());
}

#[tokio::test]
async fn memory_md_reaches_only_the_sessions_of_its_environment() {
    let home = tempfile::tempdir().unwrap();
    let (a_dir, b_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let url = fake_model(Arc::clone(&prompts)).await;
    let state = daemon(home.path(), &url);
    let a = environment(&state, "Alpha", a_dir.path());
    let b = environment(&state, "Beta", b_dir.path());
    std::fs::create_dir_all(root_of(home.path()).join(&a)).unwrap();
    std::fs::write(
        root_of(home.path()).join(&a).join("Memory.md"),
        "## deploy\nAlways deploy alpha with the green button.\n",
    )
    .unwrap();

    one_turn(&state, &a).await;
    let alpha = prompts.lock().unwrap().last().cloned().unwrap();
    assert!(alpha.contains("<environment-memory>"), "{alpha}");
    assert!(alpha.contains("deploy alpha with the green button"));
    assert!(
        alpha.contains("scratch folder"),
        "the prompt names the scratch folder"
    );

    prompts.lock().unwrap().clear();
    one_turn(&state, &b).await;
    let beta = prompts.lock().unwrap().last().cloned().unwrap();
    assert!(
        !beta.contains("green button"),
        "another environment's sessions never see it"
    );
    assert!(beta.contains("<environment-memory>") && beta.contains("It is empty."));
}

#[tokio::test]
async fn changing_the_root_moves_the_folders_and_a_target_that_is_not_empty_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1");
    let env = environment(&state, "Work", project.path());
    let (session, _events) = state
        .create_in_environment(&env, None, SessionOptions::default())
        .unwrap();
    let id = session.id().to_string();
    seed_session_files(home.path(), &env, &id);
    std::fs::write(
        root_of(home.path()).join(&env).join("Memory.md"),
        "# kept\n",
    )
    .unwrap();

    let mut connection = Connection::new(Arc::clone(&state));
    handshake(&mut connection);

    // A folder that is not empty is refused: nothing moves and the setting is not written.
    let occupied = elsewhere.path().join("occupied");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("mine.txt"), "mine").unwrap();
    let refused = call(
        &mut connection,
        2,
        "config.set",
        json!({"key": "workspaces.root", "value": occupied.display().to_string()}),
    )
    .unwrap_err();
    assert!(refused.message.contains("not empty"), "{}", refused.message);
    assert!(scratch(home.path(), &env, &id).join("notes.txt").is_file());
    assert!(occupied.join("mine.txt").is_file());
    assert!(
        !std::fs::read_to_string(home.path().join("config").join("config.toml"))
            .unwrap_or_default()
            .contains("workspaces")
    );

    // A folder inside the current one is refused too.
    let inside = root_of(home.path()).join("nested");
    assert!(
        call(
            &mut connection,
            3,
            "config.set",
            json!({"key": "workspaces.root", "value": inside.display().to_string()}),
        )
        .is_err()
    );

    // A new folder takes everything, and sessions are told the new place.
    let target = elsewhere.path().join("moved");
    let set = call(
        &mut connection,
        4,
        "config.set",
        json!({"key": "workspaces.root", "value": target.display().to_string()}),
    )
    .unwrap();
    assert_eq!(set["entry"]["source"], "user");
    assert!(!root_of(home.path()).exists(), "the old folder is gone");
    let moved = target.join(&env).join("sessions").join(&id);
    assert_eq!(
        std::fs::read_to_string(moved.join("notes.txt")).unwrap(),
        "scratch"
    );
    assert_eq!(
        std::fs::read(moved.join("deep").join("x.bin"))
            .unwrap()
            .len(),
        4096
    );
    assert_eq!(
        std::fs::read_to_string(target.join(&env).join("Memory.md")).unwrap(),
        "# kept\n"
    );
    assert_eq!(state.workspaces_where().root, target.display().to_string());
    let saved = std::fs::read_to_string(home.path().join("config").join("config.toml")).unwrap();
    assert!(
        saved.contains("[workspaces]") && saved.contains("root"),
        "{saved}"
    );
    assert!(
        state.session(&id).is_some(),
        "the idle session was restarted, not lost"
    );

    // New sessions use the new place.
    let (second, _events) = state
        .create_in_environment(&env, None, SessionOptions::default())
        .unwrap();
    assert!(
        target
            .join(&env)
            .join("sessions")
            .join(second.id())
            .is_dir()
    );
}

#[tokio::test]
async fn a_root_edited_in_the_config_file_is_moved_when_the_daemon_starts() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let first = daemon(home.path(), "http://127.0.0.1:9/v1");
    let env = environment(&first, "Work", project.path());
    let (session, _events) = first
        .create_in_environment(&env, None, SessionOptions::default())
        .unwrap();
    seed_session_files(home.path(), &env, session.id());
    first.reconcile_workspaces();
    let id = session.id().to_string();
    drop((session, first));

    // Edited by hand while the daemon is off.
    let target = elsewhere.path().join("by-hand");
    let config = imperium_config::Paths::under(home.path()).user_config_file();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!("[workspaces]\nroot = {:?}\n", target.display().to_string()),
    )
    .unwrap();
    let loaded = imperium_config::load(
        &imperium_config::Paths::under(home.path()),
        None,
        std::iter::empty::<(String, String)>(),
    )
    .unwrap()
    .settings()
    .unwrap();
    let second = DaemonState::new(imperium_config::Paths::under(home.path()), loaded, None);
    second.reconcile_workspaces();
    assert!(
        target
            .join(&env)
            .join("sessions")
            .join(&id)
            .join("notes.txt")
            .is_file()
    );
    assert!(!root_of(home.path()).exists());
    assert!(second.workspaces_where().warning.is_none());

    // A target that cannot be used leaves the folders where they are and says why.
    let blocked = elsewhere.path().join("blocked");
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(blocked.join("in-the-way"), "x").unwrap();
    std::fs::write(
        &config,
        format!("[workspaces]\nroot = {:?}\n", blocked.display().to_string()),
    )
    .unwrap();
    let loaded = imperium_config::load(
        &imperium_config::Paths::under(home.path()),
        None,
        std::iter::empty::<(String, String)>(),
    )
    .unwrap()
    .settings()
    .unwrap();
    let third = DaemonState::new(imperium_config::Paths::under(home.path()), loaded, None);
    third.reconcile_workspaces();
    let found = third.workspaces_where();
    assert_eq!(found.root, target.display().to_string());
    assert!(found.warning.unwrap().contains("not empty"));
    assert!(
        target
            .join(&env)
            .join("sessions")
            .join(&id)
            .join("notes.txt")
            .is_file()
    );
}
