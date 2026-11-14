//! Worktrees for parallel writing subagents (roadmap item 26): writers started with
//! `access: "write"` work in git worktrees of their own, read-only subagents keep sharing the
//! delegator's folders, each writer's work comes back as a commit on its branch with a merge
//! report (conflicts reported, never resolved), and the worktrees go with the session on the
//! same terms as a session's own.

use std::path::{Path, PathBuf};
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
use imperium_tools::permissions::{
    PermissionMode, PermissionRule, Permissions, RuleAction, SharedRules, Target,
};
use imperium_tools::{Invocation, Workspace, WriterTrees};
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
                // A call that is not an agent's own (such as a session title) gets a plain
                // answer and does not advance the script.
                let agent_call = body["tools"]
                    .as_array()
                    .is_some_and(|tools| !tools.is_empty());
                if !agent_call {
                    return (
                        [("content-type", "text/event-stream")],
                        text_body("A title"),
                    );
                }
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
                            if *access != "read" {
                                args["access"] = json!(access);
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
    start_with(endpoint, journal, workspace, Vec::new(), Default::default())
}

fn start_with(
    endpoint: Endpoint,
    journal: &Path,
    workspace: Workspace,
    tools: Vec<Arc<dyn imperium_api::ToolProvider>>,
    extensions: imperium_extensions::Extensions,
) -> Session {
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
        tools,
        notices: Vec::new(),
        extensions,
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
        harness: Default::default(),
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
    // Both edited the same file, so whichever finished second is told it collides with the other.
    assert!(
        last_root.contains("conflicts with writer t1")
            || last_root.contains("conflicts with writer t2"),
        "{last_root}"
    );
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
            .any(|notice| notice.contains("can have a worktree")),
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

    let conflicted = writers.finish("t1", true);
    assert!(conflicted.contains("imperium/s1-t1"), "{conflicted}");
    assert!(
        conflicted.contains("does NOT merge cleanly"),
        "{conflicted}"
    );
    assert!(conflicted.contains("shared.txt"), "{conflicted}");
    let clean = writers.finish("t2", true);
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
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(["merge", "imperium/s1-t1"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the conflicting merge stops");
    git(repo.path(), &["merge", "--abort"]);

    // A writer with no changes says so and leaves no commit behind.
    let three = writers.start("t3", &folders).unwrap().unwrap();
    let nothing = writers.finish("t3", true);
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
    // The writer is stopped part-way: nothing of it is committed yet.
    assert!(started.folders[0].is_dir());

    let deleted = call(
        &mut connection,
        "session.delete",
        json!({"session_id": id, "remove_worktrees": true}),
    );
    assert_eq!(deleted["removed_worktrees"].as_array().unwrap().len(), 1);
    assert!(!started.folders[0].exists());
    assert!(
        !git(repo.path(), &["worktree", "list"]).contains(".worktrees"),
        "git forgot the removed worktree"
    );
    // The branch, with what the writer had left uncommitted now committed, stays; the checkout
    // is as it was.
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

// ---------------------------------------------------------------------------------------------
// Review fixes: writers against each other, resume, permissions, secrets, records, confinement.
// ---------------------------------------------------------------------------------------------

fn git_fails(dir: &Path, args: &[&str]) -> bool {
    !Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs")
        .status
        .success()
}

fn canonical(repo: &tempfile::TempDir) -> [PathBuf; 1] {
    [repo.path().canonicalize().unwrap()]
}

#[test]
fn two_writers_changing_the_same_file_are_reported_as_colliding_with_each_other() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let writers = GitWriterTrees::new("s3", &journals.path().join("s3.jsonl"), None);
    let folders = canonical(&repo);
    let one = writers.start("t1", &folders).unwrap().unwrap();
    let two = writers.start("t2", &folders).unwrap().unwrap();
    std::fs::write(one.folders[0].join("shared.txt"), "one\n").unwrap();
    std::fs::write(two.folders[0].join("shared.txt"), "two\n").unwrap();
    std::fs::write(two.folders[0].join("new.txt"), "new\n").unwrap();

    // Each merges cleanly into the delegator's checkout on its own; the first to finish has
    // nothing to collide with yet, the second finds the first's commit.
    let first = writers.finish("t1", true);
    assert!(first.contains("merges cleanly"), "{first}");
    assert!(!first.contains("conflicts with writer"), "{first}");
    let second = writers.finish("t2", true);
    assert!(second.contains("merges cleanly into"), "{second}");
    assert!(
        second.contains("conflicts with writer t1 (branch imperium/s3-t1) in shared.txt"),
        "{second}"
    );
    assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
}

#[test]
fn a_writer_is_told_when_the_delegators_checkout_has_edits_to_the_same_files() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let writers = GitWriterTrees::new("s4", &journals.path().join("s4.jsonl"), None);
    let folders = canonical(&repo);
    let one = writers.start("t1", &folders).unwrap().unwrap();
    assert!(one.notice.is_empty(), "{}", one.notice);
    std::fs::write(one.folders[0].join("shared.txt"), "writer\n").unwrap();
    std::fs::write(repo.path().join("shared.txt"), "edited by hand\n").unwrap();

    let report = writers.finish("t1", true);
    assert!(report.contains("Caution"), "{report}");
    assert!(report.contains("shared.txt"), "{report}");
    // A writer started now is told its worktree lacks those edits.
    let two = writers.start("t2", &folders).unwrap().unwrap();
    assert!(two.notice.contains("uncommitted changes"), "{}", two.notice);
}

#[test]
fn files_that_look_like_secrets_are_left_out_of_the_commit_and_a_switched_branch_is_refused() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let writers = GitWriterTrees::new("s5", &journals.path().join("s5.jsonl"), None);
    let folders = canonical(&repo);
    let one = writers.start("t1", &folders).unwrap().unwrap();
    let tree = one.folders[0].clone();
    std::fs::write(tree.join(".env"), "KEY=1\n").unwrap();
    std::fs::write(tree.join("a.txt"), "fine\n").unwrap();
    let report = writers.finish("t1", true);
    assert!(report.contains("Not committed"), "{report}");
    assert!(report.contains(".env"), "{report}");
    assert_eq!(git(&tree, &["show", "imperium/s5-t1:a.txt"]), "fine");
    assert!(git_fails(&tree, &["show", "imperium/s5-t1:.env"]));
    assert!(tree.join(".env").is_file(), "left in the worktree");

    let two = writers.start("t2", &folders).unwrap().unwrap();
    let tree = two.folders[0].clone();
    let base = git(&tree, &["rev-parse", "HEAD"]);
    git(&tree, &["checkout", "-q", "-b", "elsewhere"]);
    std::fs::write(tree.join("b.txt"), "b\n").unwrap();
    let report = writers.finish("t2", true);
    assert!(report.contains("instead of its branch"), "{report}");
    assert_eq!(git(repo.path(), &["rev-parse", "imperium/s5-t2"]), base);
    assert_eq!(git(repo.path(), &["rev-parse", "elsewhere"]), base);
}

#[test]
fn a_secret_file_in_a_folder_with_an_unusual_name_is_still_held_back() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let writers = GitWriterTrees::new("s5b", &journals.path().join("s5b.jsonl"), None);
    let folders = canonical(&repo);
    let one = writers.start("t1", &folders).unwrap().unwrap();
    let tree = one.folders[0].clone();
    // Git quotes names like these in its default listing, so the floor must see them as written.
    std::fs::create_dir_all(tree.join("café")).unwrap();
    std::fs::write(tree.join("café").join(".env"), "KEY=1\n").unwrap();
    std::fs::write(tree.join("café").join("notes naïve.txt"), "fine\n").unwrap();
    let report = writers.finish("t1", true);
    assert!(report.contains("Not committed"), "{report}");
    assert!(report.contains("café/.env"), "{report}");
    assert!(git_fails(&tree, &["show", "imperium/s5b-t1:café/.env"]));
    assert_eq!(
        git(&tree, &["show", "imperium/s5b-t1:café/notes naïve.txt"]),
        "fine"
    );
}

#[test]
fn the_record_of_a_deleted_session_keeps_only_the_writers_that_could_not_be_removed() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("s7.jsonl");
    let writers = GitWriterTrees::new("s7", &journal, None);
    let folders = canonical(&repo);
    writers.start("t1", &folders).unwrap().unwrap();
    writers.start("t2", &folders).unwrap().unwrap();
    let record = journals.path().join("s7.writers.json");
    let read = || -> Vec<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap()
    };
    let entries = read();
    assert_eq!(entries.len(), 2);
    let root_of = |task: &str| {
        let entry = entries.iter().find(|entry| entry["task"] == task).unwrap();
        entry["trees"][0]["root"].as_str().unwrap().to_string()
    };

    // Asked to remove the worktrees, and t2's could not be: only t2 stays on record.
    GitWriterTrees::settle(&journal, true, &[format!("{}: in use", root_of("t2"))]);
    let kept = read();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["task"], "t2");
    // Everything removed: it goes.
    GitWriterTrees::settle(&journal, true, &[]);
    assert!(!record.exists());
}

#[test]
fn a_failure_spelling_the_folder_differently_still_keeps_the_record() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("s9.jsonl");
    let writers = GitWriterTrees::new("s9", &journal, None);
    writers.start("t1", &canonical(&repo)).unwrap().unwrap();
    let record = journals.path().join("s9.writers.json");
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    let root = entries[0]["trees"][0]["root"].as_str().unwrap().to_string();
    // The same folder as git might spell it: not the recorded text, but the same place.
    let spelled = std::path::Path::new(&root).join(".");
    GitWriterTrees::settle(&journal, true, &[format!("{}: in use", spelled.display())]);
    assert!(record.exists(), "the writer whose worktree stayed is kept");
}

#[test]
fn the_record_goes_when_worktrees_were_not_asked_to_be_removed() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("s8.jsonl");
    let writers = GitWriterTrees::new("s8", &journal, None);
    writers.start("t1", &canonical(&repo)).unwrap().unwrap();
    let record = journals.path().join("s8.writers.json");
    assert!(record.is_file());
    GitWriterTrees::settle(&journal, false, &["x: failed".into()]);
    assert!(!record.exists());
}

#[test]
fn a_writer_is_not_committed_when_committing_is_forbidden() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let writers = GitWriterTrees::new("s6", &journals.path().join("s6.jsonl"), None);
    let folders = canonical(&repo);
    let one = writers.start("t1", &folders).unwrap().unwrap();
    let base = git(&one.folders[0], &["rev-parse", "HEAD"]);
    std::fs::write(one.folders[0].join("shared.txt"), "w\n").unwrap();
    let report = writers.finish("t1", false);
    assert!(report.contains("not committed"), "{report}");
    assert_eq!(git(repo.path(), &["rev-parse", "imperium/s6-t1"]), base);
    assert!(!git(&one.folders[0], &["status", "--porcelain"]).is_empty());
}

#[test]
fn a_corrupt_record_is_kept_aside_and_an_unrecordable_writer_is_not_started() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("s7.jsonl");
    let registry = journals.path().join("s7.writers.json");
    std::fs::write(&registry, "{not json").unwrap();
    let writers = GitWriterTrees::new("s7", &journal, None);
    assert!(!writers.is_writer("t1"));
    assert!(
        journals.path().join("s7.writers.json.corrupt").is_file(),
        "the corrupt record is kept"
    );
    let folders = canonical(&repo);
    writers.start("t1", &folders).unwrap().unwrap();
    // The record is replaced whole: valid JSON, and no temporary file left behind.
    let text = std::fs::read_to_string(&registry).unwrap();
    assert!(serde_json::from_str::<Value>(&text).is_ok(), "{text}");
    assert!(!journals.path().join("s7.writers.json.tmp").exists());

    // Where the record cannot be written, no worktree or branch is left behind.
    let missing = journals.path().join("nowhere").join("s8.jsonl");
    let broken = GitWriterTrees::new("s8", &missing, None);
    let error = broken.start("t1", &folders).unwrap_err();
    assert!(error.contains("could not record"), "{error}");
    assert!(!repo.path().join(".worktrees/imperium-s8-t1").exists());
    assert_eq!(
        git(repo.path(), &["branch", "--list", "imperium/s8-t1"]),
        ""
    );
    assert!(!broken.is_writer("t1"));
}

#[test]
fn a_repository_without_commits_falls_back_to_sharing_the_folder() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    let journals = tempfile::tempdir().unwrap();
    let writers = GitWriterTrees::new("s9", &journals.path().join("s9.jsonl"), None);
    let started = writers
        .start("t1", &[repo.path().canonicalize().unwrap()])
        .unwrap();
    assert!(started.is_none());
    assert!(!writers.is_writer("t1"));
}

#[tokio::test]
async fn a_writer_cannot_reach_the_delegators_checkout_and_rules_see_its_paths_as_the_delegators() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let writers = Arc::new(GitWriterTrees::new(
        "s10",
        &journals.path().join("s10.jsonl"),
        None,
    ));
    let parent = repo.path().canonicalize().unwrap();
    let deny = PermissionRule {
        tool: "write_file".into(),
        pattern: format!(
            "{}/shared.txt",
            imperium_tools::plain_path(&parent).replace('\\', "/")
        ),
        action: RuleAction::Deny,
    };
    let parent_workspace = Workspace::new(&parent)
        .unwrap()
        .with_project_worktrees()
        .with_writer_trees(writers.clone())
        .with_permissions(Permissions::new(
            SharedRules::new(vec![deny]),
            SharedRules::default(),
        ));
    let start = writers
        .start("t1", std::slice::from_ref(&parent))
        .unwrap()
        .unwrap();
    let workspace = parent_workspace.with_writer_folders(&start).unwrap();
    let tree = start.folders[0].clone();

    // The worktrees of the project are not open to it, nor the checkout it was made from.
    assert!(workspace.project_worktrees().is_empty());
    let hostile = [
        "../shared.txt".to_string(),
        "../../shared.txt".to_string(),
        parent.join("shared.txt").display().to_string(),
    ];
    for path in &hostile {
        let call = Invocation::parse(
            "write_file",
            &json!({"path": path, "content": "hacked\n"}).to_string(),
        )
        .unwrap();
        assert!(
            call.execute(&workspace).await.is_err(),
            "{path} was written"
        );
    }
    // A link inside the worktree that leads out is followed, then refused.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&parent, tree.join("out")).unwrap();
        let call = Invocation::parse(
            "write_file",
            &json!({"path": "out/shared.txt", "content": "hacked\n"}).to_string(),
        )
        .unwrap();
        assert!(call.execute(&workspace).await.is_err());
    }
    assert_eq!(
        std::fs::read_to_string(parent.join("shared.txt")).unwrap(),
        "base\n"
    );

    // A rule written for the delegator's path matches the same file in the worktree.
    let target = workspace.rule_target("shared.txt");
    let Target::Path { absolute, relative } = &target else {
        panic!("a path target");
    };
    assert_eq!(
        *absolute,
        format!(
            "{}/shared.txt",
            imperium_tools::plain_path(&parent).replace('\\', "/")
        )
    );
    assert_eq!(relative.as_deref(), Some("shared.txt"));
    assert_eq!(
        workspace.permissions().evaluate("write_file", &target),
        Some(RuleAction::Deny)
    );
    // And the file itself is the worktree's.
    let call = Invocation::parse(
        "write_file",
        &json!({"path": "shared.txt", "content": "mine\n"}).to_string(),
    )
    .unwrap();
    call.execute(&workspace).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tree.join("shared.txt")).unwrap(),
        "mine\n"
    );
}

/// A fake model for resuming a writer: the root starts `WRITER-A`, awaits it, optionally has
/// the writer's worktree removed, resumes it, and (when it can) awaits it again. The writer
/// writes `from A` the first time and `again` when resumed.
async fn resume_endpoint(requests: Requests, repo: PathBuf, remove: bool) -> Endpoint {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let requests = Arc::clone(&requests);
            let root_calls = Arc::clone(&root_calls);
            let repo = repo.clone();
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                let subagent = system.starts_with("You are an Imperium subagent");
                requests.lock().unwrap().push((
                    if subagent { "WRITER-A" } else { "root" }.to_string(),
                    body.clone(),
                ));
                let reply = if subagent {
                    let last = body["messages"].as_array().unwrap().last().unwrap().clone();
                    if last["role"] == "tool" {
                        text_body("A done")
                    } else {
                        let again = last.to_string().contains("again");
                        let content = if again { "again\n" } else { "from A\n" };
                        tool_call_body(
                            "write_file",
                            &json!({"path": "shared.txt", "content": content}).to_string(),
                        )
                    }
                } else {
                    match (root_calls.fetch_add(1, Ordering::SeqCst), remove) {
                        (0, _) => tool_call_body(
                            "spawn_agent",
                            r#"{"brief":"WRITER-A: write","access":"write"}"#,
                        ),
                        (1, _) => tool_call_body("await_tasks", "{}"),
                        (2, _) => {
                            if remove {
                                let tree = repo.join(".worktrees/imperium-test-t1");
                                git(
                                    &repo,
                                    &["worktree", "remove", "--force", &tree.display().to_string()],
                                );
                            }
                            tool_call_body(
                                "resume_agent",
                                r#"{"agent_id":"t1","text":"WRITER-A: again"}"#,
                            )
                        }
                        (3, false) => tool_call_body("await_tasks", "{}"),
                        _ => text_body("all done"),
                    }
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

#[tokio::test]
async fn a_resumed_writer_goes_back_to_its_worktree_with_its_prompt() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("test.jsonl");
    let requests: Requests = Arc::default();
    let endpoint = resume_endpoint(Arc::clone(&requests), repo.path().to_path_buf(), false).await;
    let session = start(
        endpoint,
        &journal,
        workspace_with_writers(repo.path(), &journal),
    );
    run(&session).await;

    // The second run wrote in the worktree and committed again; the checkout is untouched.
    assert_eq!(
        git(repo.path(), &["show", "imperium/test-t1:shared.txt"]),
        "again"
    );
    assert_eq!(
        git(
            repo.path(),
            &["rev-list", "--count", "HEAD..imperium/test-t1"]
        ),
        "2"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("shared.txt")).unwrap(),
        "base\n"
    );
    // The resumed writer was told about its worktree again.
    let requests = requests.lock().unwrap();
    let resumed = requests
        .iter()
        .rev()
        .find(|(who, body)| who == "WRITER-A" && body.to_string().contains("again"))
        .unwrap();
    let system = resumed.1["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("worktrees of your own on branch imperium/test-t1"));
}

#[tokio::test]
async fn a_writer_whose_worktree_is_gone_is_not_resumed_in_the_delegators_folder() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("test.jsonl");
    let requests: Requests = Arc::default();
    let endpoint = resume_endpoint(Arc::clone(&requests), repo.path().to_path_buf(), true).await;
    let session = start(
        endpoint,
        &journal,
        workspace_with_writers(repo.path(), &journal),
    );
    run(&session).await;

    let requests = requests.lock().unwrap();
    let last_root = requests
        .iter()
        .rev()
        .find(|(who, _)| who == "root")
        .unwrap()
        .1
        .to_string();
    assert!(last_root.contains("worktrees are gone"), "{last_root}");
    // Nothing ran again, in the checkout or elsewhere.
    assert_eq!(
        requests.iter().filter(|(who, _)| who == "WRITER-A").count(),
        2,
        "the writer's first run only"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("shared.txt")).unwrap(),
        "base\n"
    );
}

/// The last request body the root sent.
fn last_root(requests: &Requests) -> String {
    requests
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(who, _)| who == "root")
        .unwrap()
        .1
        .to_string()
}

#[tokio::test]
async fn a_writer_is_refused_in_plan_mode_and_with_an_unknown_access() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("test.jsonl");
    let requests: Requests = Arc::default();
    let endpoint = endpoint(Arc::clone(&requests), vec![("WRITER-A", "write")]).await;
    let workspace = workspace_with_writers(repo.path(), &journal);
    workspace.permissions().set_mode(PermissionMode::Plan);
    let session = start(endpoint, &journal, workspace);
    run(&session).await;
    assert!(last_root(&requests).contains("plan mode"));
    assert_eq!(
        git(repo.path(), &["branch", "--list", "imperium/test-t1"]),
        ""
    );
    assert!(!repo.path().join(".worktrees").exists());

    let requests: Requests = Arc::default();
    let endpoint = endpoint_for(&requests, vec![("WRITER-B", "bogus")]).await;
    let journal = journals.path().join("test2.jsonl");
    let session = start(
        endpoint,
        &journal,
        workspace_with_writers(repo.path(), &journal),
    );
    run(&session).await;
    assert!(last_root(&requests).contains("access must be"));
    assert_eq!(
        git(repo.path(), &["branch", "--list", "imperium/test-t1"]),
        ""
    );
}

async fn endpoint_for(
    requests: &Requests,
    subagents: Vec<(&'static str, &'static str)>,
) -> Endpoint {
    endpoint(Arc::clone(requests), subagents).await
}

#[tokio::test]
async fn a_rule_denying_git_commit_keeps_the_writers_changes_uncommitted() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("test.jsonl");
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    let requests: Requests = Arc::default();
    let endpoint = endpoint(Arc::clone(&requests), vec![("WRITER-B", "write")]).await;
    let deny = PermissionRule {
        tool: "git_commit".into(),
        pattern: String::new(),
        action: RuleAction::Deny,
    };
    let workspace = workspace_with_writers(repo.path(), &journal).with_permissions(
        Permissions::new(SharedRules::new(vec![deny]), SharedRules::default()),
    );
    let session = start(endpoint, &journal, workspace);
    run(&session).await;

    assert!(last_root(&requests).contains("not committed"));
    assert_eq!(git(repo.path(), &["rev-parse", "imperium/test-t1"]), head);
    let tree = repo.path().join(".worktrees/imperium-test-t1");
    assert_eq!(
        std::fs::read_to_string(tree.join("shared.txt")).unwrap(),
        "from B\n"
    );
    assert!(!git(&tree, &["status", "--porcelain"]).is_empty());
}

/// Offered to the root and to read-only subagents, but not to writers.
#[derive(Debug)]
struct SessionOnly;

impl imperium_api::ToolProvider for SessionOnly {
    fn for_writers(&self) -> bool {
        false
    }

    fn specs(&self) -> Vec<imperium_api::ToolSpec> {
        vec![imperium_api::ToolSpec {
            name: "session_tool".into(),
            description: "Tied to the session's folders.".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }]
    }

    fn handles(&self, name: &str) -> bool {
        name == "session_tool"
    }

    fn check(&self, _name: &str, _arguments: &str) -> Result<imperium_api::ToolCheck, String> {
        Ok(imperium_api::ToolCheck {
            summary: "session tool".into(),
            ask: false,
            needs_context: false,
            command: None,
        })
    }

    fn call<'a>(
        &'a self,
        _context: &'a imperium_api::ToolContext<'a>,
        _name: &'a str,
        _arguments: &'a str,
    ) -> imperium_api::ToolFuture<'a> {
        Box::pin(async { Ok("ok".to_string()) })
    }
}

fn offers(body: &Value, tool: &str) -> bool {
    body["tools"].as_array().is_some_and(|tools| {
        tools
            .iter()
            .any(|spec| spec["function"]["name"].as_str() == Some(tool))
    })
}

#[tokio::test]
async fn session_bound_tools_and_hooks_follow_the_writers_own_worktree() {
    let repo = repository();
    let journals = tempfile::tempdir().unwrap();
    let journal = journals.path().join("test.jsonl");
    let config = tempfile::tempdir().unwrap();
    let marker = if cfg!(windows) {
        "echo ran> hook-marker.txt"
    } else {
        "echo ran > hook-marker.txt"
    };
    let extensions = imperium_extensions::load(&imperium_extensions::Sources {
        config_dir: config.path().to_path_buf(),
        project_dir: repo.path().to_path_buf(),
        plugin_dirs: Vec::new(),
        disabled_plugins: Vec::new(),
        hooks: vec![imperium_extensions::ConfiguredHook {
            event: "PostToolUse".into(),
            matcher: "Write".into(),
            command: marker.into(),
            url: None,
            headers: Default::default(),
            timeout_s: Some(20),
        }],
        server_timeout: Duration::from_secs(5),
        lazy: true,
        eager: Vec::new(),
    });
    assert!(extensions.notices.is_empty(), "{:?}", extensions.notices);
    let requests: Requests = Arc::default();
    let endpoint = endpoint(
        Arc::clone(&requests),
        vec![("WRITER-B", "write"), ("READER", "read")],
    )
    .await;
    let tools: Vec<Arc<dyn imperium_api::ToolProvider>> = vec![Arc::new(SessionOnly)];
    let session = start_with(
        endpoint,
        &journal,
        workspace_with_writers(repo.path(), &journal),
        tools,
        extensions,
    );
    run(&session).await;

    let requests = requests.lock().unwrap();
    let first = |who: &str| {
        requests
            .iter()
            .find(|(name, _)| name == who)
            .unwrap()
            .1
            .clone()
    };
    assert!(offers(&first("root"), "session_tool"));
    assert!(offers(&first("READER"), "session_tool"));
    assert!(
        !offers(&first("WRITER-B"), "session_tool"),
        "a writer is not offered tools tied to the session's folders"
    );
    // The writer's hook ran in its own worktree, not in the delegator's folder.
    let tree = repo.path().join(".worktrees/imperium-test-t1");
    assert!(tree.join("hook-marker.txt").is_file());
    assert!(!repo.path().join("hook-marker.txt").exists());
}

#[tokio::test]
async fn a_daemon_session_gives_its_writers_worktrees_and_records_them() {
    let home = tempfile::tempdir().unwrap();
    let repo = repository();
    let requests: Requests = Arc::default();
    let endpoint = endpoint(Arc::clone(&requests), vec![("WRITER-A", "write")]).await;
    let config = format!(
        "[models]\ndefault = \"fake:model\"\n[endpoints.fake]\nbase_url = \"{}\"\n\
         [permissions]\nmode = \"full\"\n[workflow]\nenabled = false\n[memory]\nenabled = false\n",
        endpoint.base_url
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
    let paths = imperium_config::Paths::under(home.path());
    let state = DaemonState::new(paths.clone(), settings, None);
    let (session, mut events) = state
        .create_session(&repo.path().display().to_string(), None)
        .unwrap();
    session.send("go".into()).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .expect("an event within 60 s")
            .unwrap();
        match event.event {
            SessionEvent::TurnCompleted { .. } if event.agent_id == "root" => break,
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            _ => {}
        }
    }
    let id = session.id().to_string();
    let branch = format!("imperium/{}-t1", &id[..8]);
    assert_eq!(
        git(repo.path(), &["show", &format!("{branch}:shared.txt")]),
        "from A"
    );
    assert!(
        paths
            .data_dir()
            .join("sessions")
            .join(format!("{id}.writers.json"))
            .is_file(),
        "the writer's worktree is recorded beside the journal"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("shared.txt")).unwrap(),
        "base\n"
    );
}
