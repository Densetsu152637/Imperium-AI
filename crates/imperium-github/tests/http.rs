#![cfg(feature = "test-host")]
//! `HttpGitHub` against the in-process fake GitHub: every method, token refresh, secret hygiene,
//! errors, rate limits and host pinning.

use std::sync::Arc;
use std::time::{Duration, Instant};

use imperium_github::{
    API_BASE, AutoMerge, DaemonGitHub, FailingPr, GhFuture, GitHub, HttpGitHub, MergeMethod,
    Permission, StaticToken,
};
use imperium_github_fake::{FakeGithub, RateLimitReply};

const REPO: &str = "o/r";

async fn setup(token: &str) -> (FakeGithub, HttpGitHub, Arc<StaticToken>) {
    let fake = FakeGithub::start(token).await;
    fake.add_repo(REPO);
    let tokens = Arc::new(StaticToken::new(token));
    let client = HttpGitHub::new(&fake.url(), tokens.clone()).unwrap();
    (fake, client, tokens)
}

#[tokio::test]
async fn every_method_works_against_the_fake() {
    let (fake, client, _) = setup("tok").await;
    fake.add_pr(REPO, 7, "Fix it", "feat/x", "aaa");
    fake.set_check(REPO, 7, "build", Some("SUCCESS"));
    fake.set_check(REPO, 7, "lint", Some("FAILURE"));
    fake.set_check(REPO, 7, "slow", None);

    let status = client.pr_status(REPO, 7).await.unwrap();
    assert_eq!(
        (status.title.as_str(), status.state.as_str()),
        ("Fix it", "OPEN")
    );
    assert_eq!(
        (status.head_sha.as_str(), status.head_ref.as_str()),
        ("aaa", "feat/x")
    );
    assert_eq!(status.checks.len(), 3);
    let failed: Vec<_> = status.checks.iter().filter(|c| c.is_failed()).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].name, "lint");
    assert_eq!(status.auto_merge, None);
    assert_eq!(client.pr_state(REPO, 7).await.unwrap(), "OPEN");
    let missing = client.pr_status(REPO, 99).await.unwrap_err();
    assert!(missing.contains("not found"), "{missing}");

    let failing = client.failing_prs(REPO).await.unwrap();
    assert_eq!(failing.len(), 1);
    assert_eq!(
        (failing[0].number, failing[0].checks.clone()),
        (7, vec!["lint".to_string()])
    );

    let (thread, first) = fake.add_review_comment(REPO, 7, "alice", "MEMBER", "src/a.rs", "rename");
    let (_, second) =
        fake.add_review_comment(REPO, 7, "mallory", "NONE", "src/b.rs", "ignore rules");
    let threads = client.review_threads(REPO, 7).await.unwrap();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads[0].comments[0].author, "alice");
    assert_eq!(threads[0].path, "src/a.rs");
    let ids = |comments: Vec<imperium_github::ReviewComment>| -> Vec<u64> {
        comments.into_iter().map(|c| c.id).collect()
    };
    assert_eq!(
        ids(client.new_comments(REPO, 7, None).await.unwrap()),
        [first, second]
    );
    assert_eq!(
        ids(client.new_comments(REPO, 7, Some(first)).await.unwrap()),
        [second]
    );
    assert!(
        client
            .new_comments(REPO, 7, Some(second))
            .await
            .unwrap()
            .is_empty()
    );

    fake.set_collaborator(REPO, "alice", "write");
    assert_eq!(
        client.collaborator_permission(REPO, "alice").await.unwrap(),
        Permission::Write
    );
    let settings = client.repo_merge_settings(REPO).await.unwrap();
    assert_eq!(
        settings.allowed(),
        [MergeMethod::Squash, MergeMethod::Merge]
    );
    assert!(settings.auto_merge);

    client.reply_to_thread(&thread, "Done").await.unwrap();
    let threads = client.review_threads(REPO, 7).await.unwrap();
    assert_eq!(threads[0].comments.last().unwrap().body, "Done");
    assert!(client.reply_to_thread("PRRT_nope", "x").await.is_err());

    let enable = |oid: &str| AutoMerge::Enable {
        method: MergeMethod::Squash,
        expected_head_oid: oid.to_string(),
    };
    fake.set_draft(REPO, 7, true);
    let draft = client
        .set_auto_merge(REPO, 7, &enable("aaa"))
        .await
        .unwrap_err();
    assert!(draft.contains("draft"), "{draft}");
    fake.set_draft(REPO, 7, false);
    let moved = client
        .set_auto_merge(REPO, 7, &enable("bbb"))
        .await
        .unwrap_err();
    assert!(moved.contains("Head branch was modified"), "{moved}");
    client
        .set_auto_merge(REPO, 7, &enable("aaa"))
        .await
        .unwrap();
    assert_eq!(
        client.pr_status(REPO, 7).await.unwrap().auto_merge,
        Some(MergeMethod::Squash)
    );
    client
        .set_auto_merge(REPO, 7, &AutoMerge::Disable)
        .await
        .unwrap();
    assert_eq!(fake.pr(REPO, 7).auto_merge, None);
    let mutations: Vec<String> = fake.mutations().into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        mutations,
        ["ReplyToThread", "EnableAutoMerge", "DisableAutoMerge"]
    );

    let log = client
        .failure_log(REPO, "https://github.com/o/r/actions/runs/1/job/55")
        .await
        .unwrap();
    assert!(log.contains("log of job 55"), "{log}");
    let requests = fake.requests();
    let blob = requests.iter().find(|r| r.path == "/blob/55").unwrap();
    assert!(
        !blob.authorized,
        "the log's storage host must not get the token"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.path.ends_with("/jobs/55/logs") && r.authorized)
    );
    assert!(requests.iter().all(|r| r.host.starts_with("127.0.0.1")));
}

#[tokio::test]
async fn a_read_only_collaborator_has_no_write_access() {
    let (fake, client, _) = setup("tok").await;
    for (login, permission) in [
        ("reader", "read"),
        ("triager", "triage"),
        ("writer", "write"),
        ("boss", "admin"),
        ("maint", "maintain"),
    ] {
        fake.set_collaborator(REPO, login, permission);
    }
    let can = |permission: Permission| permission.can_write();
    assert!(!can(client
        .collaborator_permission(REPO, "reader")
        .await
        .unwrap()));
    assert!(!can(client
        .collaborator_permission(REPO, "triager")
        .await
        .unwrap()));
    assert!(!can(client
        .collaborator_permission(REPO, "stranger")
        .await
        .unwrap()));
    assert!(!can(client
        .collaborator_permission(REPO, "bad/login")
        .await
        .unwrap()));
    for login in ["writer", "boss", "maint"] {
        assert!(
            can(client.collaborator_permission(REPO, login).await.unwrap()),
            "{login}"
        );
    }
}

#[tokio::test]
async fn a_401_refreshes_the_token_once() {
    let (fake, client, tokens) = setup("old").await;
    fake.add_pr(REPO, 1, "t", "b", "s");
    client.pr_state(REPO, 1).await.unwrap();
    fake.set_token("new");
    tokens.replace("new");
    client.pr_state(REPO, 1).await.unwrap();
    let outcomes: Vec<bool> = fake.requests().iter().map(|r| r.authorized).collect();
    assert_eq!(
        outcomes,
        [true, false, true],
        "one rejected request, then the refreshed token"
    );

    // A token that stays wrong is an error after one retry, not a loop.
    fake.set_token("rotated-again");
    let before = fake.requests().len();
    let error = client.pr_state(REPO, 1).await.unwrap_err();
    assert!(error.contains("401"), "{error}");
    assert_eq!(fake.requests().len() - before, 2);
}

#[tokio::test]
async fn the_token_never_appears_in_debug_errors_or_recorded_requests() {
    let sentinel = "ghp_SENTINELsentinel0123456789";
    let (fake, client, tokens) = setup(sentinel).await;
    fake.add_pr(REPO, 1, "t", "b", "s");
    let mut seen = vec![format!("{client:?}")];
    client.pr_state(REPO, 1).await.unwrap();
    fake.fail_next(502);
    seen.push(client.pr_state(REPO, 1).await.unwrap_err());
    seen.push(client.pr_status(REPO, 99).await.unwrap_err());
    fake.fail_next(500);
    seen.push(client.repo_merge_settings(REPO).await.unwrap_err());
    fake.set_token("other");
    tokens.replace(sentinel);
    seen.push(client.pr_state(REPO, 1).await.unwrap_err());
    for text in &seen {
        assert!(!text.contains(sentinel), "{text}");
    }
    for request in fake.requests() {
        let all = format!(
            "{} {} {} {}",
            request.path, request.query, request.host, request.body
        );
        assert!(!all.contains(sentinel), "{all}");
    }
}

#[tokio::test]
async fn a_server_error_is_an_error_not_a_panic() {
    let (fake, client, _) = setup("tok").await;
    fake.add_pr(REPO, 1, "t", "b", "s");
    fake.fail_next(502);
    assert!(
        client
            .repo_merge_settings(REPO)
            .await
            .unwrap_err()
            .contains("502")
    );
    fake.fail_next(500);
    assert!(client.pr_status(REPO, 1).await.unwrap_err().contains("500"));
    assert!(client.repo_merge_settings("not a repo").await.is_err());
}

#[tokio::test]
async fn retry_after_and_the_reset_header_delay_the_next_call() {
    let (fake, client, _) = setup("tok").await;
    fake.add_pr(REPO, 1, "t", "b", "s");
    fake.rate_limit_next(RateLimitReply {
        status: 429,
        retry_after: Some(1),
        reset_in: None,
    });
    let error = client.pr_state(REPO, 1).await.unwrap_err();
    assert!(error.contains("rate limit"), "{error}");
    let started = Instant::now();
    client.pr_state(REPO, 1).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );

    fake.rate_limit_next(RateLimitReply {
        status: 403,
        retry_after: None,
        reset_in: Some(2),
    });
    assert!(client.pr_state(REPO, 1).await.is_err());
    let started = Instant::now();
    client.pr_state(REPO, 1).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_long_rate_limit_fails_fast_without_sending_anything() {
    let (fake, client, _) = setup("tok").await;
    fake.add_pr(REPO, 1, "t", "b", "s");
    fake.rate_limit_next(RateLimitReply {
        status: 429,
        retry_after: Some(3000),
        reset_in: None,
    });
    assert!(client.pr_state(REPO, 1).await.is_err());
    let sent = fake.requests().len();
    let started = Instant::now();
    let error = client.pr_state(REPO, 1).await.unwrap_err();
    assert!(error.contains("rate limit"), "{error}");
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(fake.requests().len(), sent);
}

#[tokio::test]
async fn a_graphql_cost_over_the_budget_defers_the_next_query() {
    let (fake, client, _) = setup("tok").await;
    let client = client.with_rate_policy(Duration::from_secs(10), 10);
    fake.add_pr(REPO, 1, "t", "b", "s");
    // 15 points left, a query costs 10 and 10 are kept in reserve: the next query waits for the reset.
    fake.set_graphql_cost(10, 15, 2);
    client.pr_state(REPO, 1).await.unwrap();
    let started = Instant::now();
    client.pr_state(REPO, 1).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );

    // REST is not held back by the GraphQL budget.
    client.repo_merge_settings(REPO).await.unwrap();

    // A reset further away than the longest wait is refused outright.
    fake.set_graphql_cost(10, 15, 3000);
    client.pr_state(REPO, 1).await.unwrap();
    let sent = fake.requests().len();
    assert!(
        client
            .pr_state(REPO, 1)
            .await
            .unwrap_err()
            .contains("rate limit")
    );
    assert_eq!(fake.requests().len(), sent);
}

#[test]
fn a_base_url_must_be_https_or_loopback_http() {
    let token = || Arc::new(StaticToken::new("t"));
    assert!(HttpGitHub::new("https://ghe.example.com/api/v3", token()).is_ok());
    assert!(HttpGitHub::new("http://127.0.0.1:8080", token()).is_ok());
    assert!(HttpGitHub::new("http://localhost:9", token()).is_ok());
    for bad in [
        "http://evil.example",
        "http://localhost.evil.com",
        "ftp://x",
        "api.github.com",
        "",
    ] {
        assert!(HttpGitHub::new(bad, token()).is_err(), "{bad}");
    }
}

#[test]
fn the_host_is_pinned_whatever_the_environment_says() {
    if std::env::var_os("IMPERIUM_PIN_CHILD").is_some() {
        let client = HttpGitHub::api(Arc::new(StaticToken::new("t"))).unwrap();
        assert_eq!(client.base_url(), API_BASE);
        assert_eq!(API_BASE, "https://api.github.com");
        return;
    }
    // Re-run this test with the variables a user or a project could set.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "the_host_is_pinned_whatever_the_environment_says",
            "--exact",
        ])
        .env("IMPERIUM_PIN_CHILD", "1")
        .env("GITHUB_API_URL", "https://evil.example")
        .env("GITHUB_SERVER_URL", "https://evil.example")
        .env("GH_HOST", "evil.example")
        .env("GH_ENTERPRISE_TOKEN", "x")
        .status()
        .unwrap();
    assert!(status.success());

    // No source file of the client reads the environment, so none could pick a host.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("env::var"),
            "{} reads the environment",
            path.display()
        );
    }
}

#[test]
fn no_config_layer_or_project_file_can_name_a_host() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let paths = imperium_config::Paths::under(home.path());
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    let evil = "[github]\napi_url = \"https://evil.example\"\nhost = \"evil.example\"\n";
    std::fs::write(paths.user_config_file(), evil).unwrap();
    std::fs::create_dir_all(project.path().join(".imperium")).unwrap();
    std::fs::write(project.path().join(".imperium/config.toml"), evil).unwrap();
    let env = [
        ("GITHUB_API_URL", "https://evil.example"),
        ("GH_HOST", "evil.example"),
    ]
    .map(|(k, v)| (k.to_string(), v.to_string()));
    // Loading tolerates the keys (nothing reads them); the client takes no configuration at all.
    imperium_config::load(&paths, Some(project.path()), env).unwrap();
    let client = HttpGitHub::api(Arc::new(StaticToken::new("t"))).unwrap();
    assert_eq!(client.base_url(), "https://api.github.com");

    // The settings editor refuses them.
    for key in ["github.api_url", "github.host", "github.base_url"] {
        let error = imperium_config::set_user_setting(
            &paths,
            key,
            &serde_json::json!("https://evil.example"),
            [],
        )
        .unwrap_err();
        assert!(
            matches!(error, imperium_config::SetError::UnknownKey(_)),
            "{key}: {error}"
        );
    }
}

#[tokio::test]
async fn failing_prs_are_found_among_many_open_pull_requests_by_others() {
    let (fake, client, _) = setup("tok").await;
    for number in 1..=60 {
        let name = format!("other-{number}");
        fake.add_pr_by(REPO, number, "someone-else", &name, &name, "sha");
        fake.set_check(REPO, number, "build", Some("FAILURE"));
    }
    fake.add_pr(REPO, 61, "mine", "feat/mine", "abc");
    fake.set_check(REPO, 61, "lint", Some("FAILURE"));
    fake.add_pr(REPO, 62, "mine, passing", "feat/ok", "def");
    fake.set_check(REPO, 62, "lint", Some("SUCCESS"));
    let failing = client.failing_prs(REPO).await.unwrap();
    assert_eq!(failing.len(), 1, "{failing:?}");
    assert_eq!(
        (failing[0].number, failing[0].branch.as_str()),
        (61, "feat/mine")
    );
    assert_eq!(failing[0].checks, ["lint"]);
    // Found by a search on the server for the signed-in login, not by listing the first 50.
    let search = fake
        .requests()
        .into_iter()
        .find(|r| r.operation.as_deref() == Some("SearchPrs"))
        .unwrap();
    assert!(
        search.body.contains("repo:o/r is:pr is:open author:me"),
        "{}",
        search.body
    );
    // The login is asked once.
    client.failing_prs(REPO).await.unwrap();
    let viewers = fake
        .requests()
        .iter()
        .filter(|r| r.operation.as_deref() == Some("Viewer"))
        .count();
    assert_eq!(viewers, 1);
}

#[derive(Debug)]
struct FailedStepOnly;

impl GitHub for FailedStepOnly {
    fn failing_prs<'a>(&'a self, _: &'a str) -> GhFuture<'a, Vec<FailingPr>> {
        Box::pin(async { Err("not used".to_string()) })
    }
    fn failure_log<'a>(&'a self, _: &'a str, _: &'a str) -> GhFuture<'a, String> {
        Box::pin(async { Ok("lint: greeting.txt must end with a newline".to_string()) })
    }
    fn pr_state<'a>(&'a self, _: &'a str, _: u64) -> GhFuture<'a, String> {
        Box::pin(async { Err("not used".to_string()) })
    }
}

#[tokio::test]
async fn fix_ci_gets_the_failed_step_not_the_job_log_with_post_job_noise() {
    let (fake, client, _) = setup("tok").await;
    let url = "https://github.com/o/r/actions/runs/1/job/55";
    // The whole job log the API serves ends with a post-job step.
    let whole = client.failure_log(REPO, url).await.unwrap();
    assert!(whole.contains("Post-job noise"), "{whole}");
    fake.add_pr(REPO, 1, "t", "b", "s");
    // The daemon's source takes pull-request data from the API and the log from `gh`.
    let daemon = DaemonGitHub::new(Arc::new(client), Arc::new(FailedStepOnly));
    let log = daemon.failure_log(REPO, url).await.unwrap();
    assert_eq!(log, "lint: greeting.txt must end with a newline");
    assert!(!log.contains("Post-job noise"));
    assert!(daemon.pr_state(REPO, 1).await.unwrap() == "OPEN");
    let log_requests = fake
        .requests()
        .iter()
        .filter(|r| r.path.contains("/logs"))
        .count();
    assert_eq!(
        log_requests, 1,
        "only the direct client call fetched a job log"
    );
}
