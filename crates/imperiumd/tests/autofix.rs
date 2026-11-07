//! Phase 6 exit criterion (second half): a failing pull request check is fixed in its own
//! worktree and pushed to the pull request's branch only after approval. GitHub is faked; Git
//! is real, with a local bare repository as `origin`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_github::{FailingPr, GhFuture, GitHub};
use serde_json::{Value, json};
use tokio::net::TcpListener;

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

#[derive(Debug)]
struct FakeGitHub;

impl GitHub for FakeGitHub {
    fn failing_prs<'a>(&'a self, _repo: &'a str) -> GhFuture<'a, Vec<FailingPr>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn failure_log<'a>(&'a self, _repo: &'a str, _url: &'a str) -> GhFuture<'a, String> {
        Box::pin(async { Ok("lint: greeting.txt must end with a newline".to_string()) })
    }

    fn pr_state<'a>(&'a self, _repo: &'a str, _number: u64) -> GhFuture<'a, String> {
        Box::pin(async { Ok("OPEN".to_string()) })
    }
}

/// Writes the fix with write_file, then summarises.
async fn fake_model() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|axum::Json(body): axum::Json<Value>| async move {
            let fixed = body["messages"]
                .as_array()
                .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"));
            let chunk = if fixed {
                json!({"choices":[{"delta":{"content":"Added the missing trailing newline."}}]})
            } else {
                let arguments = json!({"path": "greeting.txt", "content": "hello\n"}).to_string();
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"w",
                    "function":{"name":"write_file","arguments":arguments}}]}}]})
            };
            (
                [("content-type", "text/event-stream")],
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

#[tokio::test]
async fn a_failing_check_is_fixed_and_pushed_after_approval() {
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

    let home = dir.path().join("home");
    let project = clone.display().to_string().replace('\\', "/");
    let config = format!(
        r#"
        [models]
        default = "fake:model"
        [endpoints.fake]
        base_url = "{model}"
        [memory]
        enabled = false
        [[github.watch]]
        repo = "owner/repo"
        project = "{project}"
        [github.autofix]
        enabled = true
        "#,
        model = fake_model().await
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
    let daemon = imperiumd::DaemonState::new(imperium_config::Paths::under(&home), settings, None);
    let mut announcements = daemon.announcements();

    let pr = FailingPr {
        number: 7,
        branch: "feature".into(),
        head_sha: before.clone(),
        checks: vec!["lint".into()],
        details_url: String::new(),
    };
    let fixing = {
        let daemon = Arc::clone(&daemon);
        tokio::spawn(async move { imperiumd::fix_ci(&daemon, &FakeGitHub, &watch, &pr).await })
    };

    let mut statuses = Vec::new();
    loop {
        let notification = tokio::time::timeout(Duration::from_secs(30), announcements.recv())
            .await
            .expect("an announcement within 30 s")
            .unwrap();
        let params = notification.params;
        match notification.method.as_str() {
            "approval.requested" => {
                // Nothing reaches GitHub before approval.
                assert_eq!(git(&origin, &["rev-parse", "feature"]), before);
                assert!(params["summary"].as_str().unwrap().contains("owner/repo#7"));
                let id = params["id"].as_u64().unwrap();
                assert!(daemon.approvals().respond(id, true));
            }
            "autofix.event" => {
                let status = params["status"].as_str().unwrap().to_string();
                assert_ne!(status, "failed", "{params}");
                statuses.push(status.clone());
                if status == "pushed" {
                    break;
                }
            }
            _ => {}
        }
    }
    fixing.await.unwrap();
    assert_eq!(statuses, ["started", "awaiting_approval", "pushed"]);
    assert_eq!(git(&origin, &["show", "feature:greeting.txt"]), "hello");
    let message = git(&origin, &["log", "-1", "--format=%B", "feature"]);
    assert!(message.starts_with("fix: lint on #7"), "{message}");
    assert!(message.contains("Imperium-Auto-Fix: session"), "{message}");
    // The fix is a new commit on top, not a rewrite.
    assert_eq!(git(&origin, &["rev-parse", "feature~1"]), before);
    // The user's own checkout is untouched.
    assert_eq!(
        std::fs::read_to_string(clone.join("greeting.txt")).unwrap(),
        "hello"
    );
}
