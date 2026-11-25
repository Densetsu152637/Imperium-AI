//! A session watching a pull request shows `watching`, then `merged` once it merges.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use imperium_github::{FailingPr, GhFuture, GitHub, PrStatus};
use imperium_protocol::session::SessionStatus;

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

    fn pr_status<'a>(&'a self, _repo: &'a str, number: u64) -> GhFuture<'a, PrStatus> {
        let merged = self.merged.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(PrStatus {
                node_id: String::new(),
                number,
                title: "A change".into(),
                url: String::new(),
                state: if merged { "MERGED" } else { "OPEN" }.into(),
                is_draft: false,
                head_ref: "feature".into(),
                head_sha: "abc".into(),
                base_ref: "main".into(),
                is_in_merge_queue: false,
                review_decision: None,
                mergeable: "MERGEABLE".into(),
                merge_state: "CLEAN".into(),
                auto_merge: None,
                checks: Vec::new(),
            })
        })
    }
}

#[tokio::test]
async fn watched_pull_requests_turn_into_merged_sessions() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n[workflow]\nenabled = false"
                .parse()
                .unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    let daemon =
        imperium_core::DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
    let (session, _) = daemon
        .create_session(&project.path().display().to_string(), None)
        .unwrap();
    assert_eq!(session.status().0, SessionStatus::Idle);
    daemon.watch(&session, Some(("owner/repo".into(), 12)));
    assert_eq!(
        session.status(),
        (SessionStatus::Watching, Some("owner/repo#12".into()))
    );
    let github = Arc::new(FakeGitHub::default());
    daemon
        .pr_monitor()
        .start(&daemon, github.clone(), Duration::from_millis(50));
    github.merged.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        if session.status().0 == SessionStatus::Merged {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        session.status(),
        (SessionStatus::Merged, Some("owner/repo#12".into()))
    );
}
