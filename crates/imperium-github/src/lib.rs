//! What the auto-fix responders need from GitHub and Git: pull requests with failing checks and
//! their logs (through the `gh` CLI, using its login), and a worktree per pull request where a
//! fix is made, committed, and pushed (never forced). [`HttpGitHub`] is the daemon-side client
//! for the pull-request card and its auto-merge and review-comment work: GraphQL and REST to
//! `https://api.github.com` with a token that cannot be printed.

#![warn(unreachable_pub)]

pub mod git;
mod http;
mod queries;
mod secret;
mod types;

pub use http::{API_BASE, HttpGitHub};
pub use secret::{GhAuthToken, Secret, StaticToken, TokenFuture, TokenSource, scrub};
pub use types::{
    AutoMerge, Check, MergeMethod, MergeSettings, Permission, PrStatus, ReviewComment, ReviewThread,
};

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;
use serde_json::Value;

/// A pull request whose latest commit has at least one failed check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingPr {
    pub number: u64,
    pub branch: String,
    pub head_sha: String,
    /// Names of the failed checks.
    pub checks: Vec<String>,
    /// The first failed check's details URL (an Actions job, when it is one).
    pub details_url: String,
}

pub type GhFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

fn unsupported<'a, T: Send + 'a>(what: &'static str) -> GhFuture<'a, T> {
    Box::pin(async move { Err(format!("{what} is not supported by this GitHub source")) })
}

/// Read access to a repository's pull requests; [`GhCli`] and [`HttpGitHub`] are the real ones.
/// The first three methods are what auto-fix uses; the rest are for the pull-request card and
/// default to "not supported", so a source that implements only the first three still compiles.
pub trait GitHub: Send + Sync + fmt::Debug {
    /// Open pull requests by the signed-in user whose checks failed.
    fn failing_prs<'a>(&'a self, repo: &'a str) -> GhFuture<'a, Vec<FailingPr>>;
    /// The failed steps' log for a check, from its details URL.
    fn failure_log<'a>(&'a self, repo: &'a str, details_url: &'a str) -> GhFuture<'a, String>;
    /// A pull request's state: `OPEN`, `MERGED`, or `CLOSED`.
    fn pr_state<'a>(&'a self, repo: &'a str, number: u64) -> GhFuture<'a, String>;

    /// A pull request's state, checks, review decision, mergeability and auto-merge request.
    fn pr_status<'a>(&'a self, _repo: &'a str, _number: u64) -> GhFuture<'a, PrStatus> {
        unsupported("pr_status")
    }
    /// The pull request's review threads with their comments.
    fn review_threads<'a>(
        &'a self,
        _repo: &'a str,
        _number: u64,
    ) -> GhFuture<'a, Vec<ReviewThread>> {
        unsupported("review_threads")
    }
    /// Review comments with an id above `since` (the cursor; `None` for all), oldest first.
    fn new_comments<'a>(
        &'a self,
        _repo: &'a str,
        _number: u64,
        _since: Option<u64>,
    ) -> GhFuture<'a, Vec<ReviewComment>> {
        unsupported("new_comments")
    }
    /// What `login` may do in the repository; `None` for a non-collaborator.
    fn collaborator_permission<'a>(
        &'a self,
        _repo: &'a str,
        _login: &'a str,
    ) -> GhFuture<'a, Permission> {
        unsupported("collaborator_permission")
    }
    /// Which merge methods the repository allows, and whether it allows auto-merge.
    fn repo_merge_settings<'a>(&'a self, _repo: &'a str) -> GhFuture<'a, MergeSettings> {
        unsupported("repo_merge_settings")
    }
    /// Replies in a review thread (its GraphQL node id); returns the new comment's id.
    fn reply_to_thread<'a>(&'a self, _thread_id: &'a str, _body: &'a str) -> GhFuture<'a, u64> {
        unsupported("reply_to_thread")
    }
    /// Enables auto-merge pinned to a head commit, or disables it.
    fn set_auto_merge<'a>(
        &'a self,
        _repo: &'a str,
        _number: u64,
        _change: &'a AutoMerge,
    ) -> GhFuture<'a, ()> {
        unsupported("set_auto_merge")
    }
}

/// Uses the `gh` CLI and whatever account it is signed in to.
#[derive(Debug, Default)]
pub struct GhCli;

async fn gh(args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("gh")
        .args(args)
        .output()
        .await
        .map_err(|error| format!("could not run gh: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

impl GitHub for GhCli {
    fn pr_state<'a>(&'a self, repo: &'a str, number: u64) -> GhFuture<'a, String> {
        Box::pin(async move {
            let number = number.to_string();
            let json = gh(&["pr", "view", &number, "--repo", repo, "--json", "state"]).await?;
            let value: Value = serde_json::from_str(&json).map_err(|error| error.to_string())?;
            Ok(value["state"].as_str().unwrap_or("OPEN").to_string())
        })
    }

    fn failing_prs<'a>(&'a self, repo: &'a str) -> GhFuture<'a, Vec<FailingPr>> {
        Box::pin(async move {
            let json = gh(&[
                "pr",
                "list",
                "--repo",
                repo,
                "--author",
                "@me",
                "--state",
                "open",
                "--json",
                "number,headRefName,headRefOid,statusCheckRollup",
            ])
            .await?;
            parse_pr_list(&json)
        })
    }

    fn failure_log<'a>(&'a self, repo: &'a str, details_url: &'a str) -> GhFuture<'a, String> {
        Box::pin(async move {
            let Some(job) = job_id(details_url) else {
                return Ok(format!("(no Actions log; see {details_url})"));
            };
            gh(&["run", "view", "--repo", repo, "--job", &job, "--log-failed"]).await
        })
    }
}

/// The job id in an Actions URL such as `.../actions/runs/1/job/2`.
fn job_id(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("/job/")?;
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (!id.is_empty()).then_some(id)
}

/// Parses `gh pr list --json number,headRefName,headRefOid,statusCheckRollup`.
pub fn parse_pr_list(json: &str) -> Result<Vec<FailingPr>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Pr {
        number: u64,
        head_ref_name: String,
        head_ref_oid: String,
        #[serde(default)]
        status_check_rollup: Vec<Value>,
    }
    let prs: Vec<Pr> = serde_json::from_str(json).map_err(|error| error.to_string())?;
    Ok(prs
        .into_iter()
        .filter_map(|pr| {
            let failed: Vec<&Value> = pr
                .status_check_rollup
                .iter()
                .filter(|check| {
                    // Check runs report a conclusion; commit statuses report a state.
                    let outcome = check
                        .get("conclusion")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .or_else(|| check.get("state").and_then(Value::as_str))
                        .unwrap_or_default();
                    matches!(
                        outcome,
                        "FAILURE" | "TIMED_OUT" | "ERROR" | "STARTUP_FAILURE"
                    )
                })
                .collect();
            let first = failed.first()?;
            let name = |check: &Value| {
                check
                    .get("name")
                    .or_else(|| check.get("context"))
                    .and_then(Value::as_str)
                    .unwrap_or("check")
                    .to_string()
            };
            Some(FailingPr {
                number: pr.number,
                branch: pr.head_ref_name,
                head_sha: pr.head_ref_oid,
                checks: failed.iter().map(|check| name(check)).collect(),
                details_url: first
                    .get("detailsUrl")
                    .or_else(|| first.get("targetUrl"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_checks_are_found_in_the_rollup() {
        let json = r#"[
          {"number": 14, "headRefName": "feat/a", "headRefOid": "abc", "statusCheckRollup": [
            {"__typename": "CheckRun", "name": "docs", "conclusion": "SUCCESS", "detailsUrl": "x"},
            {"__typename": "CheckRun", "name": "rust (windows-latest)", "conclusion": "FAILURE",
             "detailsUrl": "https://github.com/o/r/actions/runs/42/job/128476802764"},
            {"__typename": "StatusContext", "context": "ci/legacy", "state": "ERROR", "targetUrl": "y"}
          ]},
          {"number": 15, "headRefName": "feat/b", "headRefOid": "def", "statusCheckRollup": [
            {"name": "rust", "conclusion": "", "status": "IN_PROGRESS"}
          ]}
        ]"#;
        let failing = parse_pr_list(json).unwrap();
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].number, 14);
        assert_eq!(failing[0].checks, ["rust (windows-latest)", "ci/legacy"]);
        assert_eq!(
            job_id(&failing[0].details_url).as_deref(),
            Some("128476802764")
        );
        assert_eq!(job_id("https://example.com/status"), None);
    }
}
