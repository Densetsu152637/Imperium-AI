//! The GitHub token never leaves the client: an auto-fix run against the fake GitHub, through
//! `HttpGitHub` with a sentinel token, must not put the sentinel in the journal, the
//! notifications, the prompts sent to the model, or the approval text. GitHub's error reply here
//! echoes the token (the fake does that on purpose), so the client's scrubbing is on the path.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_github::{AutoMerge, FailingPr, GitHub, HttpGitHub, MergeMethod, StaticToken};
use imperium_github_fake::FakeGithub;
use serde_json::{Value, json};
use tokio::net::TcpListener;

const SENTINEL: &str = "ghp_SENTINEL0123456789abcdefghijklmnop";

/// A tracing writer that keeps everything the daemon logs, so the test can search it.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;

    fn make_writer(&'a self) -> LogBuffer {
        self.clone()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Writes `greeting.txt` on the first request and summarises on the second; keeps every request.
async fn fake_model(requests: Arc<Mutex<Vec<String>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let requests = Arc::clone(&requests);
            async move {
                requests.lock().unwrap().push(body.to_string());
                let fixed = body["messages"]
                    .as_array()
                    .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"));
                let chunk = if fixed {
                    json!({"choices":[{"delta":{"content":"Added the missing newline."}}]})
                } else {
                    json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"w",
                        "function":{"name":"write_file",
                        "arguments":json!({"path":"greeting.txt","content":"hello\n"}).to_string()}}]}}]})
                };
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                )
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// Every file under `dir`, as bytes.
fn files(dir: &Path, found: &mut Vec<(std::path::PathBuf, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, found);
        } else if let Ok(bytes) = std::fs::read(&path) {
            found.push((path, bytes));
        }
    }
}

#[tokio::test]
async fn the_github_token_is_absent_from_an_auto_fix_runs_journal_notifications_prompts_and_approval()
 {
    // Every log line of the run, at every level, on this thread (a current-thread test runtime
    // runs the daemon's tasks here).
    let logs = LogBuffer::default();
    let _logging = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    );
    let fake = FakeGithub::start(SENTINEL).await;
    fake.add_repo("owner/repo");
    let github = HttpGitHub::new(&fake.url(), Arc::new(StaticToken::new(SENTINEL))).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    let clone = dir.path().join("clone");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-q"]);
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["config", "user.name", "Test"]);
    git(&clone, &["config", "user.email", "test@example.com"]);
    git(&clone, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(clone.join("greeting.txt"), "hello").unwrap();
    git(&clone, &["add", "-A"]);
    git(&clone, &["commit", "-q", "-m", "add greeting"]);
    git(&clone, &["push", "-q", "-u", "origin", "feature"]);
    let before = git(&origin, &["rev-parse", "feature"]);

    // The pull request has auto-merge on: the daemon's own push must turn it off first.
    fake.add_pr("owner/repo", 7, "Add the greeting", "feature", &before);
    github
        .set_auto_merge(
            "owner/repo",
            7,
            &AutoMerge::Enable {
                method: MergeMethod::Squash,
                expected_head_oid: before.clone(),
            },
        )
        .await
        .unwrap();
    // GitHub's reply to the next request (the failed check's log) is an error that echoes the
    // token it was sent.
    fake.fail_next(500);

    let prompts = Arc::new(Mutex::new(Vec::new()));
    let model = fake_model(Arc::clone(&prompts)).await;
    let home = dir.path().join("home");
    let project = clone.display().to_string().replace('\\', "/");
    let config = format!(
        "[models]\ndefault = \"fake:model\"\n[endpoints.fake]\nbase_url = \"{model}\"\n\
         [memory]\nenabled = false\n[[github.watch]]\nrepo = \"owner/repo\"\nproject = \"{project}\"\n\
         [github.autofix]\nenabled = true\npush = \"ask\"\n"
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
    let watch = settings.github.watch[0].clone();
    let daemon =
        imperium_core::DaemonState::new(imperium_config::Paths::under(&home), settings, None);
    let mut announcements = daemon.announcements();

    let pr = FailingPr {
        number: 7,
        branch: "feature".into(),
        head_sha: before.clone(),
        checks: vec!["lint".into()],
        details_url: "https://github.com/owner/repo/actions/runs/1/job/55".into(),
    };
    let fixing = {
        let daemon = Arc::clone(&daemon);
        tokio::spawn(async move { imperium_core::fix_ci(&daemon, &github, &watch, &pr).await })
    };

    let mut notifications = Vec::new();
    let mut approval = String::new();
    loop {
        let notification = tokio::time::timeout(Duration::from_secs(30), announcements.recv())
            .await
            .expect("an announcement within 30 s")
            .unwrap();
        notifications.push(format!("{} {}", notification.method, notification.params));
        match notification.method.as_str() {
            "approval.requested" => {
                approval = notification.params.to_string();
                let id = notification.params["id"].as_u64().unwrap();
                assert!(daemon.approvals().respond(id, true));
            }
            "autofix.event" => {
                let status = notification.params["status"].as_str().unwrap().to_string();
                assert_ne!(status, "failed", "{}", notification.params);
                if status == "pushed" {
                    break;
                }
            }
            _ => {}
        }
    }
    fixing.await.unwrap();

    // The push turned auto-merge off, and nothing turned it back on.
    assert!(fake.pr("owner/repo", 7).auto_merge.is_none());
    let mutations: Vec<String> = fake
        .mutations()
        .into_iter()
        .map(|(operation, _)| operation)
        .collect();
    assert_eq!(mutations, ["EnableAutoMerge", "DisableAutoMerge"]);

    // The run really used the token (the fake checked it), and its error text reached the prompt.
    let requests = fake.requests();
    assert!(requests.iter().any(|r| r.authorized), "{requests:?}");
    let prompts = prompts.lock().unwrap().clone();
    assert!(!prompts.is_empty());
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("the log could not be read")),
        "the scrubbed GitHub error should be in the prompt"
    );
    assert!(!approval.is_empty());

    for (what, texts) in [
        ("a notification", &notifications),
        ("a prompt", &prompts),
        ("the approval", &vec![approval.clone()]),
    ] {
        for text in texts {
            assert!(!text.contains(SENTINEL), "the token is in {what}: {text}");
        }
    }
    // The log: the run logged (so the capture works) and the token is not in it.
    let logged = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(
        logged.contains("auto-fix"),
        "nothing was logged: {logged:?}"
    );
    assert!(
        !logged.contains(SENTINEL),
        "the token is in the log: {logged}"
    );
    // The journal and everything else the daemon wrote under its home.
    let mut written = Vec::new();
    files(&home, &mut written);
    assert!(
        !written.is_empty(),
        "the daemon wrote nothing under its home"
    );
    for (path, bytes) in written {
        let found = bytes
            .windows(SENTINEL.len())
            .any(|window| window == SENTINEL.as_bytes());
        assert!(!found, "the token is in {}", path.display());
    }
}
