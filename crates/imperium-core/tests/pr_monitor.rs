//! Pull requests bound to sessions, against the fake GitHub: binding by URL, one
//! `pull_requests.changed` per change and none without one, errors that keep the last good
//! status, one poll per pull request however many sessions follow it, rate limits, and the
//! live poller.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use imperium_core::{Connection, DaemonState};
use imperium_github::{HttpGitHub, StaticToken};
use imperium_github_fake::{FakeGithub, RateLimitReply};
use imperium_protocol::jsonrpc::{Notification, Outcome, Response, codes};
use imperium_protocol::session::SessionStatus;
use serde_json::{Value, json};
use tokio::sync::broadcast;

const TOKEN: &str = "ghp_PRMONITORSENTINEL0123456789abcdef";
const REPO: &str = "owner/repo";
const URL: &str = "https://github.com/owner/repo/pull/7";

fn settings(extra: &str) -> imperium_config::Settings {
    imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            format!(
                "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n[workflow]\nenabled = false\n{extra}"
            )
            .parse()
            .unwrap(),
        ),
    ])
    .settings()
    .unwrap()
}

/// A folder that is a git repository whose `origin` is `url`.
fn repository(url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", url]] {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    dir
}

struct World {
    fake: FakeGithub,
    daemon: Arc<DaemonState>,
    notices: broadcast::Receiver<Notification>,
    _home: tempfile::TempDir,
    folders: Vec<tempfile::TempDir>,
}

impl World {
    async fn new(extra: &str) -> Self {
        let fake = FakeGithub::start(TOKEN).await;
        fake.add_repo(REPO);
        fake.add_pr(REPO, 7, "Add the thing", "feature", "abc123");
        let home = tempfile::tempdir().unwrap();
        let daemon = DaemonState::new(
            imperium_config::Paths::under(home.path()),
            settings(extra),
            None,
        );
        let notices = daemon.announcements();
        let github = HttpGitHub::new(&fake.url(), Arc::new(StaticToken::new(TOKEN))).unwrap();
        daemon.pr_monitor().attach(Arc::new(github));
        Self {
            fake,
            daemon,
            notices,
            _home: home,
            folders: Vec::new(),
        }
    }

    /// A session in a checkout of `owner/repo`.
    fn session(&mut self) -> Arc<imperium_engine::Session> {
        let folder = repository("https://github.com/owner/repo.git");
        let (session, _) = self
            .daemon
            .create_session(&folder.path().display().to_string(), None)
            .unwrap();
        self.folders.push(folder);
        session
    }

    fn connection(&self) -> Connection {
        Connection::new(Arc::clone(&self.daemon))
    }

    /// The `pull_requests.changed` notifications since the last call, as `(session, status)`.
    fn changes(&mut self) -> Vec<(String, Value)> {
        let mut found = Vec::new();
        while let Ok(notice) = self.notices.try_recv() {
            if notice.method == "pull_requests.changed" {
                found.push((
                    notice.params["session_id"].as_str().unwrap().to_string(),
                    notice.params["status"].clone(),
                ));
            }
        }
        found
    }

    async fn poll(&self) {
        self.daemon.pr_monitor().poll(&self.daemon, true).await;
    }

    fn status_requests(&self) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r.operation.as_deref() == Some("PrStatus"))
            .count()
    }
}

async fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Response {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(20), deferred)
            .await
            .expect("an answer within 20 s"),
        (None, None) => panic!("no reply"),
    }
}

async fn shake(connection: &mut Connection) {
    let handshake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": {"major": 0, "minor": 57}});
    result(call(connection, 1, "system.handshake", handshake).await);
}

fn result(response: Response) -> Value {
    match response.outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{error:?}"),
    }
}

fn error(response: Response) -> (i32, String) {
    match response.outcome {
        Outcome::Error(error) => (error.code, error.message),
        Outcome::Result(value) => panic!("expected an error, got {value}"),
    }
}

async fn bind(connection: &mut Connection, session: &str) -> Value {
    result(
        call(
            connection,
            10,
            "pull_requests.bind",
            json!({"session_id": session, "url": URL}),
        )
        .await,
    )
}

#[tokio::test]
async fn a_session_binds_by_url_and_an_unrelated_repository_is_refused() {
    let mut world = World::new("").await;
    world.fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    world.fake.set_check(REPO, 7, "test", Some("SUCCESS"));
    world.fake.set_check(REPO, 7, "docs", None);
    let session = world.session();
    let mut connection = world.connection();
    shake(&mut connection).await;

    let status = bind(&mut connection, session.id()).await;
    assert_eq!(status["url"], URL);
    assert_eq!(status["repo"], REPO);
    assert_eq!(status["number"], 7);
    assert_eq!(status["title"], "Add the thing");
    assert_eq!(status["state"], "open");
    assert_eq!(status["branch"], "feature");
    assert_eq!(status["base"], "main");
    assert_eq!(status["head_sha"], "abc123");
    assert_eq!(
        status["checks"],
        json!({"passed": 1, "failed": 1, "pending": 1, "failing_names": ["lint"]})
    );
    assert_eq!(status["review"], "none");
    assert_eq!(status["mergeable"], "mergeable");
    assert_eq!(status["allowed_methods"], json!(["squash", "merge"]));
    assert_eq!(status["auto_merge"], json!({"enabled": false}));
    assert_eq!(status["auto_fix"], false);
    assert!(status.get("error").is_none());
    assert_eq!(
        session.status(),
        (SessionStatus::Watching, Some("owner/repo#7".into()))
    );
    // The cache answers `pull_requests.status`, and the bind announced itself once.
    let cached = result(
        call(
            &mut connection,
            11,
            "pull_requests.status",
            json!({"session_id": session.id()}),
        )
        .await,
    );
    assert_eq!(cached, status);
    assert_eq!(world.changes().len(), 1);

    // Another repository, a malformed URL and an unknown session are refused.
    for url in [
        "https://github.com/someone/else/pull/1",
        "https://github.com/owner/repo/issues/7",
        "https://example.com/owner/repo/pull/7",
    ] {
        let refused = call(
            &mut connection,
            12,
            "pull_requests.bind",
            json!({"session_id": session.id(), "url": url}),
        )
        .await;
        assert_eq!(error(refused).0, codes::INVALID_PARAMS, "{url}");
    }
    let unknown = call(
        &mut connection,
        13,
        "pull_requests.bind",
        json!({"session_id": "nope", "url": URL}),
    )
    .await;
    assert_eq!(error(unknown).0, codes::SESSION_NOT_FOUND);
    // A refused bind changed nothing.
    assert!(world.changes().is_empty());

    // A session in some other repository may follow a repository that is already bound.
    let elsewhere = repository("https://github.com/someone/else.git");
    let (other, _) = world
        .daemon
        .create_session(&elsewhere.path().display().to_string(), None)
        .unwrap();
    bind(&mut connection, other.id()).await;
    // And unbinding says so, switches the overlay off and announces it.
    let unbound = result(
        call(
            &mut connection,
            14,
            "pull_requests.unbind",
            json!({"session_id": session.id()}),
        )
        .await,
    );
    assert_eq!(unbound["was_bound"], true);
    assert_eq!(session.status().0, SessionStatus::Idle);
    let changes = world.changes();
    assert!(
        changes
            .iter()
            .any(|(id, status)| id == session.id() && status.is_null())
    );
    let again = result(
        call(
            &mut connection,
            15,
            "pull_requests.unbind",
            json!({"session_id": session.id()}),
        )
        .await,
    );
    assert_eq!(again["was_bound"], false);
}

#[tokio::test]
async fn each_change_on_github_is_announced_once_and_no_change_is_not() {
    let mut world = World::new("").await;
    let session = world.session();
    let mut connection = world.connection();
    shake(&mut connection).await;
    bind(&mut connection, session.id()).await;
    world.changes();

    // Nothing changed: no announcement, however often it is polled.
    world.poll().await;
    world.poll().await;
    assert!(world.changes().is_empty());

    type Step = (&'static str, fn(&FakeGithub), fn(&Value));
    let steps: [Step; 7] = [
        (
            "a check fails",
            |f| f.set_check(REPO, 7, "lint", Some("FAILURE")),
            |s| assert_eq!(s["checks"]["failing_names"], json!(["lint"])),
        ),
        (
            "changes are requested",
            |f| f.set_review_decision(REPO, 7, Some("CHANGES_REQUESTED")),
            |s| assert_eq!(s["review"], "changes_requested"),
        ),
        (
            "a conflict appears",
            |f| f.set_mergeable(REPO, 7, "CONFLICTING", "DIRTY"),
            |s| {
                assert_eq!(s["mergeable"], "conflicting");
                assert_eq!(s["merge_state"], "dirty");
            },
        ),
        (
            "it becomes a draft",
            |f| f.set_draft(REPO, 7, true),
            |s| {
                assert_eq!(s["state"], "draft");
                assert_eq!(s["draft"], true);
                let reason = s["auto_merge"]["reason_unavailable"].as_str().unwrap();
                assert!(reason.contains("draft"), "{reason}");
            },
        ),
        (
            "it is ready for review",
            |f| f.set_draft(REPO, 7, false),
            |s| assert_eq!(s["state"], "open"),
        ),
        (
            "it enters the merge queue",
            |f| f.set_in_merge_queue(REPO, 7, true),
            |s| assert_eq!(s["merge_state"], "queued"),
        ),
        (
            "it is pushed to",
            |f| f.force_push(REPO, 7, "def456"),
            |s| {
                assert_eq!(s["head_sha"], "def456");
                assert_eq!(s["checks"]["failed"], 0);
            },
        ),
    ];
    for (what, change, check) in steps {
        change(&world.fake);
        world.poll().await;
        let seen = world.changes();
        assert_eq!(seen.len(), 1, "{what}: {seen:?}");
        assert_eq!(seen[0].0, session.id());
        check(&seen[0].1);
        // Polling again with nothing new says nothing.
        world.poll().await;
        assert!(world.changes().is_empty(), "{what}: announced twice");
    }

    // Merging marks the session merged (and archives it, which is the default).
    world.fake.merge(REPO, 7);
    world.poll().await;
    let seen = world.changes();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].1["state"], "merged");
    assert_eq!(seen[0].1["auto_merge"]["enabled"], false);
}

#[tokio::test]
async fn a_closed_pull_request_is_announced_and_ends_the_watch() {
    let mut world = World::new("[sessions]\nauto_archive_on_pr_close = false").await;
    let session = world.session();
    let mut connection = world.connection();
    shake(&mut connection).await;
    bind(&mut connection, session.id()).await;
    world.changes();
    world.fake.close(REPO, 7);
    world.poll().await;
    let seen = world.changes();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].1["state"], "closed");
    assert_eq!(session.status().0, SessionStatus::Idle);
}

#[tokio::test]
async fn a_github_error_is_shown_and_the_last_good_status_is_kept() {
    let mut world = World::new("").await;
    let session = world.session();
    let mut connection = world.connection();
    shake(&mut connection).await;
    let good = bind(&mut connection, session.id()).await;
    world.changes();

    world.fake.fail_next(502);
    world.poll().await;
    let seen = world.changes();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let failed = &seen[0].1;
    assert!(
        failed["error"].as_str().unwrap().contains("502"),
        "{failed}"
    );
    assert_eq!(failed["title"], good["title"]);
    assert_eq!(failed["checks"], good["checks"]);
    assert!(!failed.to_string().contains(TOKEN));

    // The same failure again is not news; recovering clears the error, once.
    world.fake.fail_next(502);
    world.poll().await;
    assert!(world.changes().is_empty());
    world.poll().await;
    let seen = world.changes();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].1.get("error").is_none(), "{}", seen[0].1);
}

#[tokio::test]
async fn sessions_on_one_pull_request_share_one_poll() {
    let mut world = World::new("").await;
    let first = world.session();
    let second = world.session();
    let mut connection = world.connection();
    shake(&mut connection).await;
    bind(&mut connection, first.id()).await;
    bind(&mut connection, second.id()).await;
    world.changes();

    let before = world.status_requests();
    world.poll().await;
    world.fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    world.poll().await;
    assert_eq!(world.status_requests() - before, 2, "one request per poll");
    // Both sessions heard about the one change.
    let seen = world.changes();
    let mut ids: Vec<&str> = seen.iter().map(|(id, _)| id.as_str()).collect();
    ids.sort_unstable();
    let mut expected = vec![first.id(), second.id()];
    expected.sort_unstable();
    assert_eq!(ids, expected);
    // The poll stops once the last session lets go.
    for session in [&first, &second] {
        result(
            call(
                &mut connection,
                20,
                "pull_requests.unbind",
                json!({"session_id": session.id()}),
            )
            .await,
        );
    }
    let before = world.status_requests();
    world.poll().await;
    assert_eq!(world.status_requests(), before);
}

#[tokio::test]
async fn a_retry_after_pauses_polling() {
    let mut world = World::new("").await;
    let session = world.session();
    let mut connection = world.connection();
    shake(&mut connection).await;
    bind(&mut connection, session.id()).await;
    world.changes();

    world.fake.rate_limit_next(RateLimitReply {
        status: 429,
        retry_after: Some(3600),
        reset_in: None,
    });
    world.poll().await;
    let seen = world.changes();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].1["error"].as_str().unwrap().contains("rate limit"));
    // GitHub said to wait an hour: later polls do not reach it, and say why.
    let before = world.fake.requests().len();
    world.poll().await;
    world.poll().await;
    assert_eq!(world.fake.requests().len(), before, "a request was sent");
    let status = result(
        call(
            &mut connection,
            30,
            "pull_requests.status",
            json!({"session_id": session.id()}),
        )
        .await,
    );
    assert!(
        status["error"].as_str().unwrap().contains("rate limit"),
        "{status}"
    );
    assert_eq!(status["title"], "Add the thing");
}

#[tokio::test]
async fn the_poller_pushes_changes_without_any_client_polling() {
    let mut world = World::new("").await;
    let session = world.session();
    let github = HttpGitHub::new(&world.fake.url(), Arc::new(StaticToken::new(TOKEN))).unwrap();
    world
        .daemon
        .pr_monitor()
        .start(&world.daemon, Arc::new(github), Duration::from_millis(40));
    world.daemon.watch(&session, Some((REPO.into(), 7)));
    wait_for(&mut world, |s| s["title"] == "Add the thing").await;
    world.fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    let seen = wait_for(&mut world, |s| s["checks"]["failed"] == 1).await;
    assert_eq!(seen["checks"]["failing_names"], json!(["lint"]));
}

/// Waits for a `pull_requests.changed` whose status satisfies `found`.
async fn wait_for(world: &mut World, found: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        for (_, status) in world.changes() {
            if found(&status) {
                return status;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "no matching change");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
