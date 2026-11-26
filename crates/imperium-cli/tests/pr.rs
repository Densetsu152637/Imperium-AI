//! `imperium pr status` against an in-process daemon and the fake GitHub: it prints the status
//! the daemon holds (the data the desktop's card shows), says so when nothing is bound, and
//! prints GitHub-supplied text with control and direction characters escaped.

use std::process::Command;
use std::sync::Arc;

use imperium_cli::client::Client;
use imperium_cli::pr;
use imperium_config::{Config, Layer, Paths, Source};
use imperium_core::{AccessToken, DaemonState};
use imperium_github::{GitHub, HttpGitHub, StaticToken};
use imperium_github_fake::FakeGithub;
use imperium_protocol::PROTOCOL_VERSION;
use imperium_protocol::discovery::DaemonEndpoint;
use imperium_protocol::methods::{
    Peer, PrAutoMerge, PrChecks, PrConflictState, PrMergeMethod, PrReview, PrState, PrStatus,
    PullRequestsBind, PullRequestsBindParams, PullRequestsStatus, PullRequestsStatusParams,
};
use tokio::net::TcpListener;

const TOKEN: &str = "ghp_PRCLISENTINEL0123456789abcdefgh";
const REPO: &str = "owner/repo";
const URL: &str = "https://github.com/owner/repo/pull/7";

fn me() -> Peer {
    Peer {
        name: "test".into(),
        version: "0".into(),
    }
}

async fn start_daemon(daemon: Arc<DaemonState>) -> DaemonEndpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token = AccessToken::generate().unwrap();
    let endpoint = DaemonEndpoint {
        url: format!("ws://{}/rpc", listener.local_addr().unwrap()),
        token: token.as_str().to_string(),
        pid: std::process::id(),
        socket: None,
        protocol: PROTOCOL_VERSION,
    };
    tokio::spawn(imperium_core::serve(
        listener,
        token,
        daemon,
        std::future::pending(),
    ));
    endpoint
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
    // Held so the fake, the data folder and the checkout outlive the test body.
    _fake: FakeGithub,
    _home: tempfile::TempDir,
    _folder: tempfile::TempDir,
    client: Client,
    session: String,
}

async fn world() -> World {
    let fake = FakeGithub::start(TOKEN).await;
    fake.add_repo(REPO);
    fake.add_pr(REPO, 7, "Add the thing", "feature", "abc123");
    let home = tempfile::tempdir().unwrap();
    let settings = Config::from_layers([
        Layer::defaults(),
        Layer::new(
            Source::User("test".into()),
            "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n[workflow]\nenabled = false\n"
                .parse()
                .unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    let daemon = DaemonState::new(Paths::under(home.path()), settings, None);
    let github = HttpGitHub::new(&fake.url(), Arc::new(StaticToken::new(TOKEN))).unwrap();
    daemon.pr_monitor().attach(Arc::new(github));
    let folder = repository("https://github.com/owner/repo.git");
    let (session, _) = daemon
        .create_session(&folder.path().display().to_string(), None)
        .unwrap();
    let session = session.id().to_string();
    let endpoint = start_daemon(daemon).await;
    let client = Client::connect(&endpoint, me()).await.unwrap();
    World {
        _fake: fake,
        _home: home,
        _folder: folder,
        client,
        session,
    }
}

#[tokio::test]
async fn nothing_is_bound_until_a_pull_request_is() {
    let mut world = world().await;
    let listing = pr::status(&mut world.client, None).await.unwrap();
    assert_eq!(listing, "No session has a pull request bound.\n");
    let error = pr::status(&mut world.client, Some(&world.session))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no pull request is bound"),
        "{error}"
    );
}

#[tokio::test]
async fn prints_the_status_the_daemon_holds() {
    let mut world = world().await;
    let bound = world
        .client
        .call::<PullRequestsBind>(PullRequestsBindParams {
            session_id: world.session.clone(),
            url: URL.to_string(),
        })
        .await
        .unwrap();
    assert_eq!(bound.url, URL);

    let cached = world
        .client
        .call::<PullRequestsStatus>(PullRequestsStatusParams {
            session_id: world.session.clone(),
        })
        .await
        .unwrap();
    let one = pr::status(&mut world.client, Some(&world.session))
        .await
        .unwrap();
    assert_eq!(one, pr::render(&cached));
    assert!(one.starts_with("owner/repo#7  open  https://github.com/owner/repo/pull/7\n"));
    assert!(one.contains("  title:      Add the thing\n"), "{one}");
    assert!(one.contains("  branch:     feature into "), "{one}");
    assert!(one.contains("  checks:     "), "{one}");
    assert!(
        one.contains("  auto-fix:   off (not available yet)\n"),
        "{one}"
    );
    assert!(one.contains("  auto-merge: off"), "{one}");

    // With no session named, every running session that has a pull request bound.
    let all = pr::status(&mut world.client, None).await.unwrap();
    assert_eq!(all, format!("session {}\n{one}", world.session));
}

#[tokio::test]
async fn automerge_shows_its_state_and_turns_off_and_the_command_line_cannot_turn_it_on() {
    let mut world = world().await;
    world
        .client
        .call::<PullRequestsBind>(PullRequestsBindParams {
            session_id: world.session.clone(),
            url: URL.to_string(),
        })
        .await
        .unwrap();
    let shown = pr::automerge_status(&mut world.client, &world.session)
        .await
        .unwrap();
    assert_eq!(shown, "owner/repo#7: auto-merge off\n");

    // Turning off what is already off sends nothing to GitHub.
    let off = pr::automerge_off(&mut world.client, &world.session)
        .await
        .unwrap();
    assert_eq!(off, "owner/repo#7: auto-merge off\n");
    assert!(world._fake.mutations().is_empty());

    // Turned on elsewhere (here GitHub itself), the command line shows it and turns it off.
    let github = HttpGitHub::new(&world._fake.url(), Arc::new(StaticToken::new(TOKEN))).unwrap();
    github
        .set_auto_merge(
            REPO,
            7,
            &imperium_github::AutoMerge::Enable {
                method: imperium_github::MergeMethod::Squash,
                expected_head_oid: "abc123".into(),
            },
        )
        .await
        .unwrap();
    let off = pr::automerge_off(&mut world.client, &world.session)
        .await
        .unwrap();
    assert_eq!(off, "owner/repo#7: auto-merge off\n");
    assert!(world._fake.pr(REPO, 7).auto_merge.is_none());

    // A command that is not the desktop app cannot turn it on: the daemon refuses the call
    // the command line has no code to make.
    let error = world
        .client
        .call::<imperium_protocol::methods::PullRequestsSetAutoMerge>(
            imperium_protocol::methods::PullRequestsSetAutoMergeParams {
                session_id: world.session.clone(),
                url: URL.to_string(),
                head_sha: "abc123".into(),
                enabled: true,
                merge_method: Some(PrMergeMethod::Squash),
                challenge: Some("made-up".into()),
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("permission denied"), "{error}");
    assert!(world._fake.pr(REPO, 7).auto_merge.is_none());
}

fn hostile() -> PrStatus {
    PrStatus {
        url: "https://github.com/owner/repo/pull/7".into(),
        repo: "owner/repo".into(),
        number: 7,
        title: "fix \u{1b}[2J\u{1b}]0;owned\u{7} \u{202e}gnp.exe".into(),
        state: PrState::Open,
        draft: true,
        head_sha: "abc123".into(),
        base: "main".into(),
        branch: "feat\rure".into(),
        checks: PrChecks {
            passed: 1,
            failed: 1,
            pending: 0,
            failing_names: vec!["lint\u{2066}\n\u{9b}31m".into()],
        },
        review: PrReview::ChangesRequested,
        mergeable: PrConflictState::Conflicting,
        merge_state: "dirty".into(),
        allowed_methods: vec![PrMergeMethod::Squash, PrMergeMethod::Rebase],
        auto_fix: false,
        auto_merge: PrAutoMerge {
            enabled: false,
            method: None,
            reason_unavailable: Some("a draft\u{200b}".into()),
        },
        auto_archive_on_close: false,
        untrusted_handled: false,
        ignored_comments: 0,
        updated_at: 0,
        error: Some("rate\u{feff}limited".into()),
    }
}

#[test]
fn escapes_control_and_direction_characters_from_github() {
    let text = pr::render(&hostile());
    for c in text.chars() {
        assert!(
            c == '\n' || !pr::escape(&c.to_string()).starts_with("\\u"),
            "an unescaped {c:?} reached the terminal"
        );
    }
    assert!(text.contains("title:      fix \\u{1b}[2J\\u{1b}]0;owned\\u{7} \\u{202e}gnp.exe\n"));
    assert!(text.contains("branch:     feat\\u{d}ure into main\n"));
    assert!(text.contains("(failing: lint\\u{2066}\\u{a}\\u{9b}31m)"));
    assert!(text.contains("  checks:     1 passed, 1 failed, 0 running"));
    assert!(text.contains("review:     changes requested"));
    assert!(text.contains("conflicts:  has conflicts (merge state: dirty)"));
    assert!(text.contains("methods:    squash, rebase\n"));
    assert!(text.contains("auto-merge: off: a draft\\u{200b}\n"));
    assert!(text.contains("error:      rate\\u{feff}limited"));
    assert!(text.starts_with("owner/repo#7  draft  "));
}
