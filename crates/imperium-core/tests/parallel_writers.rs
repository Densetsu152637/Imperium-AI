//! Worktrees for parallel writing subagents (roadmap item 26): writers started with
//! `access: "write"` work in git worktrees of their own, read-only subagents keep sharing the
//! delegator's folders, each writer's work comes back as a commit on its branch with a merge
//! report (conflicts reported, never resolved), and the worktrees go with the session on the
//! same terms as a session's own.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState, GitWriterTrees};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_models::Endpoint;
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use imperium_tools::{Workspace, WriterTrees};
use serde_json::{Value, json};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn repository() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("shared.txt"), "base\n").unwrap();
    std::fs::write(repo.path().join("other.txt"), "other\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    repo
}

fn tool_call_body(name: &str, arguments: &str) -> String {
    let chunk = json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":"call-1","function":{"name":name,"arguments":arguments}}]}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20}}"#
    )
}

fn text_body(text: &str) -> String {
    let chunk = json!({"choices":[{"delta":{"content":text}}]});
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":10}}"#
    )
}

/// Every request the fake model received: who sent it (`root`, or the subagent's marker) and
/// its body.
type Requests = Arc<Mutex<Vec<(String, Value)>>>;

fn tool_results(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["content"].to_string())
        .collect()
}

/// A fake model. The root starts the subagents `subagents` names (marker, access) one call at a
/// time, awaits them, and answers. A writer writes `shared.txt` (then waits until every writer
/// has, so their edits overlap in time) and, for `WRITER-A`, runs a command; the reader reads
/// `shared.txt`.
async fn endpoint(requests: Requests, subagents: Vec<(&'static str, &'static str)>) -> Endpoint {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let writers = subagents
        .iter()
        .filter(|(_, access)| *access == "write")
        .count();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let requests = Arc::clone(&requests);
            let root_calls = Arc::clone(&root_calls);
            let writes = Arc::clone(&writes);
            let subagents = subagents.clone();
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                let marker = if system.starts_with("You are an Imperium subagent") {
                    body["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|message| message["role"] == "user")
                        .map(|message| message.to_string())
                        .find_map(|text| {
                            ["WRITER-A", "WRITER-B", "READER"]
                                .into_iter()
                                .find(|marker| text.contains(marker))
                        })
                        .unwrap_or("UNKNOWN")
                } else {
                    "root"
                };
                requests
                    .lock()
                    .unwrap()
                    .push((marker.to_string(), body.clone()));
                let results = tool_results(&body).len();
                let reply = match marker {
                    "root" => {
                        let call = root_calls.fetch_add(1, Ordering::SeqCst);
                        if let Some((marker, access)) = subagents.get(call) {
                            let mut args = json!({"brief": format!("{marker}: do your part")});
                            if *access == "write" {
                                args["access"] = json!("write");
                            }
                            tool_call_body("spawn_agent", &args.to_string())
                        } else if call == subagents.len() {
                            tool_call_body("await_tasks", "{}")
                        } else {
                            text_body("all done")
                        }
                    }
                    "READER" => match results {
                        0 => tool_call_body("read_file", r#"{"path":"shared.txt"}"#),
                        _ => text_body("read it"),
                    },
                    writer => match results {
                        0 => {
                            let content = if writer == "WRITER-A" {
                                "from A\n"
                            } else {
                                "from B\n"
                            };
                            writes.fetch_add(1, Ordering::SeqCst);
                            tool_call_body(
                                "write_file",
                                &json!({"path": "shared.txt", "content": content}).to_string(),
                            )
                        }
                        1 => {
                            // Both writers have written the same file before either goes on.
                            for _ in 0..500 {
                                if writes.load(Ordering::SeqCst) >= writers {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                            if writer == "WRITER-A" {
                                tool_call_body(
                                    "run_command",
                                    r#"{"command":"echo made > shell.txt"}"#,
                                )
                            } else {
                                text_body("B done")
                            }
                        }
                        _ => text_body("A done"),
                    },
                };
                ([("content-type", "text/event-stream")], reply)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    Endpoint {
        base_url,
        api_key: None,
    }
}

fn start(endpoint: Endpoint, journal: &Path, workspace: Workspace) -> Session {
    Session::spawn(SessionConfig {
        id: "test".into(),
        workspace,
        model: Some(ResolvedModel {
            spec: "fake:model".into(),
            endpoint,
            model: "model".into(),
            local: false,
            context_window: Some(128_000),
            kind: "openai".into(),
            reasoning: None,
            pricing: None,
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
        pre_approved: ["write_file", "read_file", "run_command"]
            .map(String::from)
            .to_vec(),
        launcher: None,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        personality: None,
        max_steps: 8,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: journal.to_path_buf(),
    })
    .unwrap()
}

/// Runs the root through its script until its turn completes; returns the notices it gave.
async fn run(session: &Session) -> Vec<String> {
    let mut events = session.subscribe();
    session.send("go".into()).unwrap();
    let mut notices = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .expect("an event within 60 s")
            .unwrap();
        match event.event {
            SessionEvent::Notice { message } => notices.push(message),
            SessionEvent::TurnCompleted { .. } if event.agent_id == "root" => return notices,
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            _ => {}
        }
    }
}

fn workspace_with_writers(folder: &Path, journal: &Path) -> Workspace {
    Workspace::new(folder)
        .unwrap()
        .with_writer_trees(Arc::new(GitWriterTrees::new("test", journal, None)))
}

#[tokio::test]
async fn concurrent_writers_edit_the_same_file_in_worktrees_of_their_own() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("test.jsonl");
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let branch = git(repo.path(), &["branch", "--show-current"]);
    let requests: Requests = Arc::default();
    let endpoint = endpoint(
        Arc::clone(&requests),
        vec![
            ("WRITER-A", "write"),
            ("WRITER-B", "write"),
            ("READER", "read"),
        ],
    )
    .await;
    let session = start(
        endpoint,
        &journal,
        workspace_with_writers(repo.path(), &journal),
    );
    run(&session).await;

    // G1: each writer's edit of the same file is on its own branch, made in its own worktree.
    let a = git(repo.path(), &["show", "imperium/test-t1:shared.txt"]);
    let b = git(repo.path(), &["show", "imperium/test-t2:shared.txt"]);
    assert_eq!(a, "from A");
    assert_eq!(b, "from B");
    let tree_a = repo.path().join(".worktrees/imperium-test-t1");
    let tree_b = repo.path().join(".worktrees/imperium-test-t2");
    assert_eq!(
        std::fs::read_to_string(tree_a.join("shared.txt"))
            .unwrap()
            .trim(),
        "from A"
    );
    assert_eq!(
        std::fs::read_to_string(tree_b.join("shared.txt"))
            .unwrap()
            .trim(),
        "from B"
    );
    // Commands are rooted in the worktree too.
    assert!(tree_a.join("shell.txt").is_file());
    assert!(git(repo.path(), &["show", "imperium/test-t1:shell.txt"]).contains("made"));
    assert!(!repo.path().join("shell.txt").exists());
    // Both results reached the delegator, each naming its branch and merge outlook.
    let requests = requests.lock().unwrap();
    let last_root = requests
        .iter()
        .rev()
        .find(|(who, _)| who == "root")
        .unwrap()
        .1
        .to_string();
    for branch in ["imperium/test-t1", "imperium/test-t2"] {
        assert!(last_root.contains(branch), "{branch} not reported");
    }
    assert!(last_root.contains("merges cleanly"));
    // The writers were told about their worktree.
    let writer_system = requests
        .iter()
        .find(|(who, _)| who == "WRITER-A")
        .unwrap()
        .1["messages"][0]["content"]
        .to_string();
    assert!(writer_system.contains("worktrees of your own on branch imperium/test-t1"));

    // G2: the read-only subagent read the delegator's own folder and got no worktree.
    let reader: Vec<_> = requests.iter().filter(|(who, _)| who == "READER").collect();
    assert!(
        reader
            .iter()
            .any(|(_, body)| tool_results(body).iter().any(|text| text.contains("base"))),
        "the reader saw the project's file"
    );
    assert!(!repo.path().join(".worktrees/imperium-test-t3").exists());
    assert!(
        git(repo.path(), &["branch", "--list", "imperium/test-t3"]).is_empty(),
        "a reader gets no branch"
    );

    // G4: the delegator's checkout is as it was.
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(git(repo.path(), &["branch", "--show-current"]), branch);
    assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("shared.txt")).unwrap(),
        "base\n"
    );
}

#[tokio::test]
async fn a_writer_outside_git_shares_the_folder_with_a_notice() {
    let folder = tempfile::tempdir().unwrap();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("test.jsonl");
    let requests: Requests = Arc::default();
    let endpoint = endpoint(Arc::clone(&requests), vec![("WRITER-B", "write")]).await;
    let session = start(
        endpoint,
        &journal,
        workspace_with_writers(folder.path(), &journal),
    );
    let notices = run(&session).await;
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("is in a repository, so the writer shares")),
        "{notices:?}"
    );
    // Today's behaviour: the writer edited the shared folder.
    assert_eq!(
        std::fs::read_to_string(folder.path().join("shared.txt")).unwrap(),
        "from B\n"
    );
}

#[test]
fn an_unmergeable_writer_is_reported_and_nothing_is_merged() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("s1.jsonl");
    let writers = GitWriterTrees::new("s1", &journal, None);
    let folders = [repo.path().canonicalize().unwrap()];
    let one = writers.start("t1", &folders).unwrap().unwrap();
    let two = writers.start("t2", &folders).unwrap().unwrap();
    assert_ne!(one.folders, two.folders);
    std::fs::write(one.folders[0].join("shared.txt"), "one\n").unwrap();
    std::fs::write(two.folders[0].join("other.txt"), "two\n").unwrap();

    // The delegator moves on in the meantime, touching the file the first writer changed.
    std::fs::write(repo.path().join("shared.txt"), "moved on\n").unwrap();
    git(repo.path(), &["commit", "-q", "-am", "moved on"]);
    let head = git(repo.path(), &["rev-parse", "HEAD"]);

    let conflicted = writers.finish("t1");
    assert!(conflicted.contains("imperium/s1-t1"), "{conflicted}");
    assert!(
        conflicted.contains("does NOT merge cleanly"),
        "{conflicted}"
    );
    assert!(conflicted.contains("shared.txt"), "{conflicted}");
    let clean = writers.finish("t2");
    assert!(clean.contains("merges cleanly"), "{clean}");
    assert!(clean.contains("M other.txt"), "{clean}");

    // Reporting merged nothing: the delegator's HEAD, index and files are untouched.
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("shared.txt")).unwrap(),
        "moved on\n"
    );
    // The clean branch merges through plain git, and the conflicting one stops for a person.
    git(
        repo.path(),
        &["merge", "--no-ff", "-m", "writer two", "imperium/s1-t2"],
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("other.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "two\n"
    );
    let merge = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["merge", "imperium/s1-t1"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the conflicting merge stops");
    git(repo.path(), &["merge", "--abort"]);

    // A writer with no changes says so and leaves no commit behind.
    let three = writers.start("t3", &folders).unwrap().unwrap();
    let nothing = writers.finish("t3");
    assert!(nothing.contains("changed no files"), "{nothing}");
    assert_eq!(three.branch, "imperium/s1-t3");
    assert_eq!(
        git(repo.path(), &["rev-parse", "imperium/s1-t3"]),
        git(&three.folders[0], &["rev-parse", "HEAD"])
    );
}

fn call(connection: &mut Connection, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let Some(Response { outcome, .. }) = connection.handle_text(&frame.to_string()).response else {
        panic!("no reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {error:?}"),
    }
}

#[tokio::test]
async fn deleting_the_session_removes_its_writers_worktrees_and_keeps_their_branches() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let config = "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n\
                  [workflow]\nenabled = false\n[memory]\nenabled = false\n";
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    let paths = imperium_config::Paths::under(home.path());
    let state = DaemonState::new(paths.clone(), settings, None);
    let mut connection = Connection::new(Arc::clone(&state));
    call(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    let env = call(
        &mut connection,
        "environments.create",
        json!({"name": "Repo", "folders": [repo.path()]}),
    );
    let created = call(
        &mut connection,
        "session.create",
        json!({"environment_id": env["id"]}),
    );
    let id = created["session_id"].as_str().unwrap().to_string();
    let head = git(repo.path(), &["rev-parse", "HEAD"]);

    // The session's own writers record their worktrees beside its journal.
    let journal = paths
        .data_dir()
        .join("sessions")
        .join(format!("{id}.jsonl"));
    let writers = GitWriterTrees::new(&id, &journal, None);
    let folders = [repo.path().canonicalize().unwrap()];
    let started = writers.start("t1", &folders).unwrap().unwrap();
    std::fs::write(started.folders[0].join("shared.txt"), "kept\n").unwrap();
    assert!(writers.finish("t1").contains("merges cleanly"));
    assert!(started.folders[0].is_dir());

    let deleted = call(
        &mut connection,
        "session.delete",
        json!({"session_id": id, "remove_worktrees": true}),
    );
    assert_eq!(deleted["removed_worktrees"].as_array().unwrap().len(), 1);
    assert!(!started.folders[0].exists());
    // The branch, with the writer's commit, stays; the checkout is as it was.
    assert_eq!(
        git(
            repo.path(),
            &["show", &format!("{}:shared.txt", started.branch)]
        ),
        "kept"
    );
    assert_eq!(git(repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
    assert!(GitWriterTrees::recorded(&journal).is_empty());
}
