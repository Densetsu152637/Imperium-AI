//! A pull request to wake sessions for, shared by the stage 8 tests (`review_replies`,
//! `injection_corpus`): fake GitHub, real Git with a local bare repository as `origin` (the
//! checkout's `origin` names the GitHub repository and `insteadOf` reaches the bare one), and a
//! scripted model whose every answer reports 600 prompt and 400 completion tokens.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_engine::Session;
use imperium_github::{GitHub, HttpGitHub, StaticToken};
use imperium_github_fake::FakeGithub;
use imperium_protocol::jsonrpc::{Notification, Outcome};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

pub const TOKEN: &str = "ghp_STAGE8SENTINEL0123456789abcdefABCDEF";
pub const REPO: &str = "owner/repo";
pub const URL: &str = "https://github.com/owner/repo/pull/7";
pub const REPLY_MARKER: &str = "<!-- imperium-auto-fix -->";

pub fn git(dir: &Path, args: &[&str]) -> String {
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

/// What the model does when its prompt is a wake (the daemon's untrusted-data envelope): the
/// tool calls one after the other, then `reply` as its final text. Any other prompt is answered
/// with "Done.".
#[derive(Clone)]
pub struct Script {
    pub calls: Vec<(&'static str, Value)>,
    pub reply: String,
}

impl Script {
    pub fn new(calls: Vec<(&'static str, Value)>, reply: &str) -> Self {
        Script {
            calls,
            reply: reply.to_string(),
        }
    }
}

pub struct Model {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Value>>>,
}

fn sse(chunks: &[Value]) -> String {
    let mut text: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    text.push_str("data: [DONE]\n\n");
    text
}

async fn fake_model(script: Script) -> Model {
    let script = Arc::new(script);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let (script, seen) = (Arc::clone(&script), Arc::clone(&seen));
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let last_user = messages.iter().rposition(|m| m["role"] == "user");
                let (is_wake, done) = last_user.map_or((false, 0), |at| {
                    (
                        messages[at]["content"].to_string().contains("CI-DATA-"),
                        messages[at + 1..]
                            .iter()
                            .filter(|m| m["role"] == "tool")
                            .count(),
                    )
                });
                seen.lock().unwrap().push(body);
                let usage = json!({"choices": [],
                    "usage": {"prompt_tokens": 600, "completion_tokens": 400}});
                let chunk = match script.calls.get(done).filter(|_| is_wake) {
                    Some((name, arguments)) => json!({"choices":[{"delta":{"tool_calls":[{
                        "index": 0, "id": format!("c{done}"),
                        "function": {"name": name, "arguments": arguments.to_string()}
                    }]}}]}),
                    None if is_wake => json!({"choices":[{"delta":{"content": script.reply}}]}),
                    None => json!({"choices":[{"delta":{"content":"Done."}}]}),
                };
                (
                    axum::http::StatusCode::OK,
                    [("content-type", "text/event-stream")],
                    sse(&[chunk, usage]),
                )
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    Model { url, requests }
}

/// A bare `origin` with a pushed `feature` branch and a clone on it.
fn setup_repo(root: &Path) -> (PathBuf, PathBuf) {
    let origin = root.join("origin.git");
    let clone = root.join("clone");
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "--bare", "-q"]);
    git(
        root,
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
    let bare = origin.display().to_string().replace('\\', "/");
    let github = "https://github.com/owner/repo.git";
    git(&clone, &["config", "remote.origin.url", github]);
    git(
        &clone,
        &["config", &format!("url.{bare}.insteadOf"), github],
    );
    (origin, clone)
}

fn daemon(home: &Path, model: &str, extra: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{model}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n{extra}"
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

async fn exchange(
    connection: &mut Connection,
    id: i64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response = match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(20), deferred)
            .await
            .expect("an answer within 20 s"),
        (None, None) => panic!("no reply"),
    };
    match response.outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => Err(error.message),
    }
}

/// What a run announced and asked. `approvals` are the `approval.requested` params.
#[derive(Debug, Default)]
pub struct Seen {
    pub statuses: Vec<String>,
    pub events: Vec<Value>,
    pub approvals: Vec<Value>,
    pub notices: Vec<Notification>,
}

/// What starts the work: the card's poller (review and CI wakes of bound pull requests), or a
/// pass of Fix CI over `[[github.watch]]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poll {
    Monitor,
    FixCi,
}

pub struct World {
    pub fake: FakeGithub,
    pub daemon: Arc<DaemonState>,
    pub notices: broadcast::Receiver<Notification>,
    pub model: Model,
    pub origin: PathBuf,
    pub clone: PathBuf,
    pub extra: String,
    pub root: tempfile::TempDir,
}

impl World {
    /// `extra` is more user config; `{clone}` in it is the checkout's path.
    pub async fn new(script: Script, extra: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (origin, clone) = setup_repo(root.path());
        let extra = extra.replace("{clone}", &clone.display().to_string().replace('\\', "/"));
        let model = fake_model(script).await;
        let fake = FakeGithub::start(TOKEN).await;
        fake.add_repo(REPO);
        fake.add_pr(REPO, 7, "Add the thing", "feature", "abc123");
        for (login, permission) in [("me", "admin"), ("alice", "write"), ("bob", "read")] {
            fake.set_collaborator(REPO, login, permission);
        }
        let daemon = daemon(&root.path().join("home"), &model.url, &extra);
        let world = Self {
            notices: daemon.announcements(),
            fake,
            daemon,
            model,
            origin,
            clone,
            extra,
            root,
        };
        world.daemon.pr_monitor().attach(world.github());
        world
    }

    pub fn github(&self) -> Arc<dyn GitHub> {
        Arc::new(HttpGitHub::new(&self.fake.url(), Arc::new(StaticToken::new(TOKEN))).unwrap())
    }

    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut connection = Connection::new(Arc::clone(&self.daemon));
        let shake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": {"major": 0, "minor": 64}});
        exchange(&mut connection, 1, "system.handshake", shake).await?;
        exchange(&mut connection, 2, method, params).await
    }

    pub async fn status(&self, session: &str) -> Value {
        self.rpc("pull_requests.status", json!({"session_id": session}))
            .await
            .unwrap()
    }

    pub fn new_session(&self) -> Arc<Session> {
        self.daemon
            .create_session(&self.clone.display().to_string(), None)
            .unwrap()
            .0
    }

    pub async fn poll(&self, poll: Poll) {
        match poll {
            Poll::Monitor => {
                self.daemon.pr_monitor().poll(&self.daemon, true).await;
            }
            Poll::FixCi => {
                imperium_core::review_testing::poll_fix_ci(&self.daemon, self.github()).await;
            }
        }
    }

    /// Binds the session, turns auto-fix on, and reads the comments that are there.
    pub async fn arm(&self, session: &Session) {
        let id = session.id();
        self.rpc("pull_requests.bind", json!({"session_id": id, "url": URL}))
            .await
            .unwrap();
        let status = self
            .rpc(
                "pull_requests.set_monitor",
                json!({"session_id": id, "url": URL, "auto_fix": true}),
            )
            .await
            .unwrap();
        assert_eq!(status["auto_fix"], true);
        self.poll(Poll::Monitor).await;
    }

    pub fn origin_head(&self) -> String {
        git(&self.origin, &["rev-parse", "feature"])
    }

    pub async fn next(&mut self, wait: Duration) -> Option<Notification> {
        loop {
            match tokio::time::timeout(wait, self.notices.recv()).await {
                Err(_) => return None,
                Ok(Ok(notice)) => return Some(notice),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(error)) => panic!("{error}"),
            }
        }
    }

    /// Polls until work starts, then answers every approval with `answer(params)` (approve,
    /// and the text for a request that has some) until an `autofix.event` whose status is in
    /// `until` arrives, and a moment more for what follows it.
    pub async fn run(
        &mut self,
        poll: Poll,
        answer: impl FnMut(&Value) -> (bool, Option<String>),
        until: &[&str],
    ) -> Seen {
        self.run_until(poll, answer, |seen| {
            seen.statuses
                .last()
                .is_some_and(|status| until.contains(&status.as_str()))
        })
        .await
    }

    /// Like [`World::run`], until `done(seen)` after an `autofix.event`.
    pub async fn run_until(
        &mut self,
        poll: Poll,
        mut answer: impl FnMut(&Value) -> (bool, Option<String>),
        done: impl Fn(&Seen) -> bool,
    ) -> Seen {
        let mut seen = Seen::default();
        let mut polls = 0;
        while self.notices.try_recv().is_ok() {}
        loop {
            if seen.events.is_empty() {
                assert!(polls < 6, "nothing started: {seen:?}");
                polls += 1;
                self.poll(poll).await;
            }
            let wait = Duration::from_secs(if seen.events.is_empty() { 2 } else { 60 });
            let Some(notice) = self.next(wait).await else {
                assert!(seen.events.is_empty(), "the run stalled: {seen:?}");
                continue;
            };
            seen.notices.push(notice.clone());
            let params = notice.params;
            match notice.method.as_str() {
                "approval.requested" => {
                    let (approve, text) = answer(&params);
                    seen.approvals.push(params.clone());
                    let found = self
                        .daemon
                        .approvals()
                        .respond_with(params["id"].as_u64().unwrap(), approve, text)
                        .expect("the answer was taken");
                    assert!(found);
                }
                "autofix.event" => {
                    let status = params["status"].as_str().unwrap().to_string();
                    seen.statuses.push(status);
                    seen.events.push(params);
                    if done(&seen) {
                        // What follows at once (the wake task's last announcements).
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        while let Ok(notice) = self.notices.try_recv() {
                            if notice.method == "autofix.event" {
                                seen.statuses
                                    .push(notice.params["status"].as_str().unwrap().to_string());
                                seen.events.push(notice.params.clone());
                            }
                            seen.notices.push(notice);
                        }
                        return seen;
                    }
                }
                _ => {}
            }
        }
    }

    /// Polls and checks that nothing starts.
    pub async fn quiet(&mut self, poll: Poll) {
        self.poll(poll).await;
        let mut noticed = Vec::new();
        while let Some(notice) = self.next(Duration::from_millis(700)).await {
            if notice.method == "autofix.event" {
                noticed.push(notice.params);
            }
        }
        assert!(noticed.is_empty(), "something started: {noticed:?}");
    }

    /// The bodies of the replies posted to GitHub, with their thread.
    pub fn replies(&self) -> Vec<(String, String)> {
        self.fake
            .mutations()
            .into_iter()
            .filter(|(operation, _)| operation == "ReplyToThread")
            .map(|(_, vars)| {
                (
                    vars["thread"].as_str().unwrap_or_default().to_string(),
                    vars["body"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    /// The daemon and GitHub again, as after a restart, on the same data folder.
    pub async fn restart(self) -> World {
        let World {
            fake,
            daemon: old,
            notices,
            model,
            origin,
            clone,
            extra,
            root,
        } = self;
        drop((notices, old));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let daemon = daemon(&root.path().join("home"), &model.url, &extra);
        let world = World {
            notices: daemon.announcements(),
            fake,
            daemon,
            model,
            origin,
            clone,
            extra,
            root,
        };
        world.daemon.pr_monitor().attach(world.github());
        world
    }
}

/// Every file under `dir`, read as text where it is text.
pub fn files_under(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            found.push((path, String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
    found
}
