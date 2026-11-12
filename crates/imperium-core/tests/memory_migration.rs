//! Older project memory (private notes keyed by a project folder, and the project's shared
//! `.imperium/memory/`) is merged into the `Memory.md` of the environment that has the folder:
//! at the daemon's start, and when a session starts in an environment; a folder that no
//! environment has is left alone until one does; and running it again changes nothing.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{DaemonState, SessionOptions};
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

fn daemon(home: &Path, base_url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [workflow]\nenabled = false\n"
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

fn memory_file(home: &Path, environment: &str) -> PathBuf {
    home.join("data")
        .join("workspaces")
        .join(environment)
        .join("Memory.md")
}

fn write_entry(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!(
            "---\nname: {name}\ndescription: About {name}\ntype: project\nscope: project\n\
             created: 2026-01-02\nsource: test\n---\n\n{body}\n"
        ),
    )
    .unwrap();
}

fn private_dir(home: &Path, project: &Path) -> PathBuf {
    home.join("data")
        .join("memory")
        .join("projects")
        .join(imperium_memory::project_id(project))
}

#[tokio::test]
async fn the_daemons_start_merges_a_folders_older_memory_into_its_environment_once() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1");
    let env = environment(&state, "Work", project.path());
    write_entry(
        &private_dir(home.path(), project.path()),
        "build-notes",
        "Run just ci.",
    );
    write_entry(
        &project.path().join(".imperium").join("memory"),
        "release-flow",
        "Tag from main.",
    );
    // A folder no environment has: nothing is done with it.
    let orphan = private_dir(home.path(), elsewhere.path());
    write_entry(&orphan, "stranded", "Nobody's.");

    state.migrate_memory();
    let text = std::fs::read_to_string(memory_file(home.path(), &env)).unwrap();
    assert!(
        text.contains("## build-notes") && text.contains("Run just ci."),
        "{text}"
    );
    assert!(
        text.contains("## release-flow") && text.contains("Tag from main."),
        "{text}"
    );
    assert!(
        orphan.join("stranded.md").is_file(),
        "an unclaimed folder is left alone"
    );
    assert!(
        project
            .path()
            .join(".imperium/memory/release-flow.md")
            .is_file(),
        "the project's own files stay"
    );
    assert!(!private_dir(home.path(), project.path()).exists());
    assert!(home.path().join("data/memory/migration.json").is_file());
    let backups = std::fs::read_dir(home.path().join("data/memory/legacy"))
        .unwrap()
        .count();
    assert_eq!(backups, 2, "one backup per merged folder");

    // Again, and after a restart: nothing changes.
    state.migrate_memory();
    drop(state);
    let restarted = daemon(home.path(), "http://127.0.0.1:9/v1");
    restarted.migrate_memory();
    assert_eq!(
        std::fs::read_to_string(memory_file(home.path(), &env)).unwrap(),
        text
    );
    assert!(
        !memory_file(home.path(), &env)
            .with_file_name("Memory.archive.md")
            .exists()
    );
}

#[tokio::test]
async fn a_folder_that_gets_an_environment_later_is_merged_when_its_first_session_starts() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let url = fake_model(Arc::clone(&prompts)).await;
    let state = daemon(home.path(), &url);
    write_entry(
        &private_dir(home.path(), project.path()),
        "late-note",
        "Found later.",
    );

    // At the daemon's start there was no environment for the folder.
    state.migrate_memory();
    assert!(
        private_dir(home.path(), project.path())
            .join("late-note.md")
            .is_file()
    );

    let env = environment(&state, "Late", project.path());
    let (session, events) = state
        .create_in_environment(&env, None, SessionOptions::default())
        .unwrap();
    session.send("hello".into()).unwrap();
    state
        .wait_for_turn(&session, events, Duration::from_secs(20))
        .await
        .unwrap();
    let system = prompts
        .lock()
        .unwrap()
        .iter()
        .find(|prompt| prompt.contains("<environment-memory>"))
        .cloned()
        .expect("a request carried the environment memory");
    assert!(
        system.contains("Found later."),
        "the session reads the merged notes: {system}"
    );
    assert!(!system.contains("project-private"), "{system}");
    assert!(!private_dir(home.path(), project.path()).exists());
}

#[tokio::test]
async fn what_does_not_fit_is_archived_beside_memory_md_and_nothing_is_lost() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "http://127.0.0.1:9/v1");
    let env = environment(&state, "Big", project.path());
    let long = "x".repeat(2_500);
    write_entry(&private_dir(home.path(), project.path()), "too-long", &long);
    write_entry(&private_dir(home.path(), project.path()), "short", "Fits.");

    state.migrate_memory();
    let file = memory_file(home.path(), &env);
    let text = std::fs::read_to_string(&file).unwrap();
    let archive = std::fs::read_to_string(file.with_file_name("Memory.archive.md")).unwrap();
    assert!(text.contains("Fits.") && !text.contains(&long), "{text}");
    assert!(archive.contains(&long) && archive.contains("## too-long"));
}
