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
/// The repository's merge flags as of 2026-11-24: squash, merge commit and rebase on, auto-merge off.
const FLAGS: (bool, bool, bool, bool) = (true, true, true, false);

fn token() -> String {
    std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN must be set to run the live check")
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
async fn review_threads_parse_on_a_pull_request_that_has_some() {
    // Find a pull request with review threads through `gh`; skip with a note when there is none.
    let query = "query{repository(owner:\"Densetsu152637\",name:\"Imperium-AI\"){pullRequests(first:100,states:[MERGED,CLOSED,OPEN],orderBy:{field:UPDATED_AT,direction:DESC}){nodes{number reviewThreads(first:1){totalCount}}}}}";
    let output = std::process::Command::new("gh")
        .args(["api", "graphql", "-f", &format!("query={query}")])
        .output();
    let found = output.ok().filter(|o| o.status.success()).and_then(|o| {
        let value: serde_json::Value = serde_json::from_slice(&o.stdout).ok()?;
        value["data"]["repository"]["pullRequests"]["nodes"]
            .as_array()?
            .iter()
            .find(|pr| pr["reviewThreads"]["totalCount"].as_u64().unwrap_or(0) > 0)
            .and_then(|pr| pr["number"].as_u64())
    });
    let Some(number) = found else {
        eprintln!(
            "SKIPPED: no pull request with review threads in {REPO} (or `gh` is unavailable)"
        );
        return;
    };
    let threads = client().review_threads(REPO, number).await.unwrap();
    assert!(!threads.is_empty());
    assert!(
        threads
            .iter()
            .all(|t| !t.id.is_empty() && !t.comments.is_empty())
    );
}

#[tokio::test]
#[ignore = "needs GITHUB_TOKEN and the network"]
async fn merge_settings_equal_the_repository_flags() {
    // The workflow's read-only token is not told the flags: that is an error, never "all off".
    let settings = match client().repo_merge_settings(REPO).await {
        Ok(settings) => settings,
        Err(error) => {
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
        Ok(permission) => assert!(permission.can_write(), "{permission:?}"),
        Err(error) => {
            assert!(error.contains("403"), "{error}");
            assert!(!error.contains(&token()), "the token leaked into an error");
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
