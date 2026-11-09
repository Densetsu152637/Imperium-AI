//! Archiving sessions: by hand (kept across restarts, undone by unarchiving), once a watched pull
//! request merges, and after days without activity, never touching sessions that are busy,
//! waiting, pinned, or project agents.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use imperium_core::{DaemonState, SessionOptions};
use imperium_engine::{LaunchFuture, SessionLauncher};
use imperium_github::{FailingPr, GhFuture, GitHub};
use imperium_protocol::session::SessionStatus;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

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

fn daemon(home: &std::path::Path, extra: &str) -> Arc<DaemonState> {
    DaemonState::new(imperium_config::Paths::under(home), settings(extra), None)
}

#[tokio::test]
async fn archived_sessions_stay_archived_across_restarts_until_unarchived() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let first = daemon(home.path(), "");
    let (session, _) = first
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    let id = session.id().to_string();
    drop(session);

    let archived = first.archive(&id).unwrap();
    assert!(archived.archived);
    assert!(archived.archived_at.is_some());
    assert!(
        first.session(&id).is_none(),
        "an archived session stops running"
    );
    assert_eq!(first.archived_sessions().len(), 1);
    // Its journal is kept, so its history can still be read.
    assert!(first.archived_history(&id).is_some());
    // Archiving twice changes nothing.
    assert_eq!(
        first.archive(&id).unwrap().archived_at,
        archived.archived_at
    );
    drop(first);

    // After a restart it is listed as archived and not resumed.
    let second = daemon(home.path(), "");
    assert_eq!(second.resume_sessions(), 0);
    let listed = second.archived_sessions();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, id);
    assert_eq!(listed[0].archived_at, archived.archived_at);

    // Unarchiving resumes it; the next restart continues it as usual.
    let unarchived = second.unarchive(&id).unwrap();
    assert!(!unarchived.archived);
    assert!(second.session(&id).is_some());
    assert!(second.archived_sessions().is_empty());
    drop(second);
    let third = daemon(home.path(), "");
    assert_eq!(third.resume_sessions(), 1);
    assert!(third.archived_sessions().is_empty());

    assert!(third.unarchive("nope").is_err());
    assert!(third.archive("nope").is_err());
}

#[derive(Debug)]
struct NoLauncher;

impl SessionLauncher for NoLauncher {
    fn launch<'a>(&'a self, _brief: &'a str, _model: Option<&'a str>) -> LaunchFuture<'a> {
        Box::pin(async { Err("not in this test".to_string()) })
    }
}

#[tokio::test]
async fn idle_sessions_are_archived_after_the_set_days_but_not_pinned_busy_or_persistent_ones() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().display().to_string();
    let state = daemon(home.path(), "[sessions]\nauto_archive_after_days = 7");
    let (idle, _) = state.create_session(&dir, None).unwrap();
    let (pinned, _) = state.create_session(&dir, None).unwrap();
    state.set_pinned(&pinned, true).unwrap();
    let (agent, _) = state
        .create_session_with(
            &dir,
            None,
            SessionOptions {
                launcher: Some(Arc::new(NoLauncher)),
                ..SessionOptions::default()
            },
        )
        .unwrap();
    // A turn against an endpoint that is not there fails and leaves the session needing
    // attention: it waits on the user.
    let (waiting, _) = state.create_session(&dir, None).unwrap();
    waiting.send("hello".into()).unwrap();
    for _ in 0..200 {
        if waiting.status().0 == SessionStatus::NeedsAttention {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(waiting.status().0, SessionStatus::NeedsAttention);

    let now = SystemTime::now();
    // Not idle long enough yet.
    assert!(state.archive_idle_sessions(now + DAY * 6).is_empty());
    let archived = state.archive_idle_sessions(now + DAY * 8);
    assert_eq!(archived, vec![idle.id().to_string()]);
    for kept in [&pinned, &agent, &waiting] {
        assert!(
            state.session(kept.id()).is_some(),
            "{} was archived",
            kept.id()
        );
    }
    // A project agent is not archived by hand either.
    assert!(state.archive(agent.id()).is_err());
}

#[tokio::test]
async fn inactivity_archiving_is_off_by_default() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), "");
    state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    assert!(
        state
            .archive_idle_sessions(SystemTime::now() + DAY * 365)
            .is_empty()
    );
}

#[derive(Debug, Default)]
struct FakeGitHub {
    merged: AtomicBool,
}

impl GitHub for FakeGitHub {
    fn failing_prs<'a>(&'a self, _repo: &'a str) -> GhFuture<'a, Vec<FailingPr>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn failure_log<'a>(&'a self, _repo: &'a str, _url: &'a str) -> GhFuture<'a, String> {
        Box::pin(async { Ok(String::new()) })
    }

    fn pr_state<'a>(&'a self, _repo: &'a str, _number: u64) -> GhFuture<'a, String> {
        let merged = self.merged.load(Ordering::SeqCst);
        Box::pin(async move { Ok(if merged { "MERGED" } else { "OPEN" }.to_string()) })
    }
}

async fn watch_until_merged(extra: &str) -> (Arc<DaemonState>, String, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let state = daemon(home.path(), extra);
    let (session, _) = state
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    state.watch(&session, Some(("owner/repo".into(), 7)));
    let github = Arc::new(FakeGitHub::default());
    imperium_core::watch_pull_requests(&state, github.clone(), Duration::from_millis(30));
    github.merged.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        if session.status().0 == SessionStatus::Merged {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(session.status().0, SessionStatus::Merged);
    // Give the watcher a moment past the merge.
    tokio::time::sleep(Duration::from_millis(100)).await;
    (state, session.id().to_string(), home)
}

#[tokio::test]
async fn a_merged_pull_request_archives_its_session() {
    let (state, id, _home) = watch_until_merged("").await;
    assert!(state.session(&id).is_none());
    let archived = state.archived_sessions();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].session_id, id);
    assert_eq!(archived[0].status, SessionStatus::Merged);
}

#[tokio::test]
async fn archiving_on_merge_can_be_turned_off() {
    let (state, id, _home) =
        watch_until_merged("[sessions]\nauto_archive_on_pr_close = false").await;
    assert!(state.session(&id).is_some());
    assert!(state.archived_sessions().is_empty());
}
