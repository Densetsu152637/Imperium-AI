#![cfg(feature = "test-host")]
//! A read-only check of `HttpGitHub` against the real GitHub API, for the `github-live` workflow
//! job. It is ignored by default: run it with `GITHUB_TOKEN` set and `-- --ignored`. It validates
//! the GraphQL documents and REST shapes the fake server imitates. Nothing here changes anything
//! on GitHub: the mutations are sent with bogus ids and must be refused as unresolvable.

use std::sync::Arc;

use imperium_github::{AutoMerge, GitHub, HttpGitHub, MergeMethod, StaticToken};

const REPO: &str = "Densetsu152637/Imperium-AI";
const OWNER: &str = "Densetsu152637";
/// A merged pull request, pinned.
const MERGED_PR: u64 = 266;
/// The fixture pull request (closed, never merged) that carries one review thread.
const THREAD_PR: u64 = 271;
/// The repository's merge flags as of 2026-11-24: squash, merge commit and rebase on, auto-merge off.
const FLAGS: (bool, bool, bool, bool) = (true, true, true, false);

fn token() -> String {
    std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN must be set to run the live check")
}

/// Set by the workflow when it runs with the `LIVE_READ_PAT` token: then the owner must be able to
/// write and the merge flags must be readable.
fn expects_write() -> bool {
    std::env::var("LIVE_EXPECT_WRITE").is_ok_and(|v| v == "1")
}

fn client() -> HttpGitHub {
    HttpGitHub::api(Arc::new(StaticToken::new(token()))).unwrap()
}

/// An error must not hold the token, and must not be a parse or validation error.
fn refused_as_unresolvable(error: &str) {
    assert!(!error.contains(&token()), "the token leaked into an error");
    let lower = error.to_ascii_lowercase();
    assert!(
        lower.contains("could not resolve") || lower.contains("not accessible"),
        "expected an unresolvable or inaccessible error, got: {error}"
    );
}

#[tokio::test]
#[ignore = "needs GITHUB_TOKEN and the network"]
async fn pr_status_of_a_merged_pull_request_parses() {
    let client = client();
    let status = client.pr_status(REPO, MERGED_PR).await.unwrap();
    assert_eq!(status.state, "MERGED");
    assert_eq!(status.head_sha.len(), 40, "{}", status.head_sha);
    assert!(status.head_sha.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!status.node_id.is_empty());
    assert!(["MERGEABLE", "CONFLICTING", "UNKNOWN"].contains(&status.mergeable.as_str()));
    assert!(
        status.review_decision.as_deref().is_none_or(|d| {
            ["APPROVED", "CHANGES_REQUESTED", "REVIEW_REQUIRED", ""].contains(&d)
        }),
        "{:?}",
        status.review_decision
    );
    let failed = status.checks.iter().filter(|c| c.is_failed()).count();
    assert!(failed <= status.checks.len());
    assert!(
        status
            .checks
            .iter()
            .all(|c| !c.name.is_empty() && !c.status.is_empty())
    );
    let (cost, remaining) = client.graphql_budget();
    assert!(cost >= 1, "rateLimit.cost was not parsed");
    assert!(remaining.is_some(), "rateLimit.remaining was not parsed");
    assert_eq!(client.pr_state(REPO, MERGED_PR).await.unwrap(), "MERGED");
}

#[tokio::test]
#[ignore = "needs GITHUB_TOKEN and the network"]
async fn review_threads_parse_on_the_fixture_pull_request() {
    // A closed, never-merged fixture pull request with one review thread (docs/11-ci-cd.md).
    let threads = client().review_threads(REPO, THREAD_PR).await.unwrap();
    assert!(
        !threads.is_empty(),
        "the fixture pull request #{THREAD_PR} has no review threads: was it deleted or edited?"
    );
    for thread in &threads {
        assert!(thread.id.starts_with("PRRT_"), "{}", thread.id);
        assert_eq!(thread.path, "docs/plans/fixture-review-thread.md");
        assert!(!thread.comments.is_empty());
        let comment = &thread.comments[0];
        assert!(comment.id > 0 && !comment.author.is_empty() && !comment.body.is_empty());
        assert!(
            comment.url.starts_with("https://github.com/"),
            "{}",
            comment.url
        );
    }
}

#[tokio::test]
#[ignore = "needs GITHUB_TOKEN and the network"]
async fn merge_settings_equal_the_repository_flags() {
    // The workflow's read-only token is not told the flags: that is an error, never "all off".
    let settings = match client().repo_merge_settings(REPO).await {
        Ok(settings) => settings,
        Err(error) => {
            assert!(
                !expects_write(),
                "the PAT could not read the merge flags: {error}"
            );
            assert!(error.contains("push access"), "{error}");
            eprintln!("NOTE: token cannot read the merge flags: {error}");
            return;
        }
    };
    assert_eq!(
        (
            settings.squash,
            settings.merge,
            settings.rebase,
            settings.auto_merge
        ),
        FLAGS
    );
}

#[tokio::test]
#[ignore = "needs GITHUB_TOKEN and the network"]
async fn the_owner_can_write_or_the_refusal_is_clean() {
    match client().collaborator_permission(REPO, OWNER).await {
        Ok(permission) => {
            assert!(permission.can_write(), "{permission:?}");
            eprintln!("BRANCH=ok");
        }
        Err(error) => {
            // A personal token that should be able to write must not take this branch.
            assert!(!expects_write(), "the PAT was refused: {error}");
            assert!(!error.contains(&token()), "the token leaked into an error");
            assert!(
                error.contains("403") && error.contains("Resource not accessible by integration"),
                "an unexpected refusal: {error}"
            );
            eprintln!("BRANCH=403");
        }
    }
}

#[tokio::test]
#[ignore = "needs GITHUB_TOKEN and the network"]
async fn mutations_with_bogus_ids_are_unresolvable_not_invalid() {
    let client = client();
    let enable = AutoMerge::Enable {
        method: MergeMethod::Squash,
        expected_head_oid: "0".repeat(40),
    };
    let error = client
        .set_auto_merge_by_node("PR_bogus", &enable)
        .await
        .unwrap_err();
    refused_as_unresolvable(&error);
    let error = client
        .set_auto_merge_by_node("PR_bogus", &AutoMerge::Disable)
        .await
        .unwrap_err();
    refused_as_unresolvable(&error);
    let error = client
        .reply_to_thread("PRRT_bogus", "never posted")
        .await
        .unwrap_err();
    refused_as_unresolvable(&error);
}

#[tokio::test]
#[ignore = "needs GITHUB_TOKEN and the network"]
async fn failing_prs_resolves_the_login_and_searches_without_errors() {
    // `Viewer` then `SearchPrs`: the login resolves, and a search on this repository parses
    // (there may be no failing pull request by this login, so only the call is asserted).
    let client = client();
    let failing = match client.failing_prs(REPO).await {
        Ok(failing) => failing,
        Err(error) => {
            // The workflow token is an installation token, which has no `viewer`.
            assert!(
                !expects_write(),
                "the PAT could not resolve its login: {error}"
            );
            assert!(!error.contains(&token()), "the token leaked into an error");
            // Or its `viewer` is the app, which the client reports as such (found by this check).
            let lower = error.to_ascii_lowercase();
            assert!(
                lower.contains("not accessible") || lower.contains("app installation token"),
                "an unexpected refusal: {error}"
            );
            eprintln!("VIEWER=refused");
            return;
        }
    };
    eprintln!("VIEWER=ok");
    for pr in &failing {
        assert!(pr.number > 0 && !pr.branch.is_empty());
        assert_eq!(pr.head_sha.len(), 40, "{}", pr.head_sha);
        assert!(!pr.checks.is_empty());
    }
    let (cost, remaining) = client.graphql_budget();
    assert!(cost >= 1 && remaining.is_some(), "rateLimit was not parsed");
}
