use super::*;

// ---- Pull requests bound to sessions ----

/// Notification sent to every client when a session's pull-request status changes, and on bind
/// and unbind. The payload is a [`PullRequestsChangedParams`].
pub const PULL_REQUESTS_CHANGED: &str = "pull_requests.changed";

/// `pull_requests.bind`: binds a session to a pull request by its URL
/// (`https://github.com/<owner>/<repo>/pull/<n>`) and answers with its status. The repository
/// must be the session's `origin` or one of the repositories the user's sessions already have a
/// pull request bound from. A session has at most one pull request; binding replaces it.
#[derive(Debug)]
pub struct PullRequestsBind;

impl Method for PullRequestsBind {
    const NAME: &'static str = "pull_requests.bind";
    type Params = PullRequestsBindParams;
    type Result = PrStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsBindParams {
    pub session_id: String,
    pub url: String,
}

/// `pull_requests.unbind`: ends a session's binding.
#[derive(Debug)]
pub struct PullRequestsUnbind;

impl Method for PullRequestsUnbind {
    const NAME: &'static str = "pull_requests.unbind";
    type Params = PullRequestsUnbindParams;
    type Result = PullRequestsUnbindResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsUnbindParams {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsUnbindResult {
    // False when the session had no pull request bound.
    pub was_bound: bool,
}

/// `pull_requests.status`: the daemon's cached status of a session's pull request. A paired
/// phone may call it.
#[derive(Debug)]
pub struct PullRequestsStatus;

impl Method for PullRequestsStatus {
    const NAME: &'static str = "pull_requests.status";
    type Params = PullRequestsStatusParams;
    type Result = PrStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsStatusParams {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsChangedParams {
    pub session_id: String,
    // None after an unbind.
    pub status: Option<PrStatus>,
}

/// A pull request as the card shows it. Every text field came from GitHub, was written by
/// someone there, is length-capped and free of control characters, and renders as plain text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PrStatus {
    // Always `https://github.com/<repo>/pull/<number>`, built by the daemon.
    pub url: String,
    pub repo: String,
    #[ts(type = "number")]
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub draft: bool,
    pub head_sha: String,
    pub base: String,
    pub branch: String,
    pub checks: PrChecks,
    pub review: PrReview,
    pub mergeable: PrConflictState,
    // GitHub's `mergeStateStatus` in lower case (`clean`, `dirty`, `blocked`, `behind`,
    // `unstable`, `has_hooks`, `draft`, `unknown`), or `queued` in a merge queue.
    pub merge_state: String,
    // The merge methods the repository allows, in the order `squash`, `merge`, `rebase`; empty
    // when they could not be read.
    pub allowed_methods: Vec<PrMergeMethod>,
    // Inert until the review-comment stage; always false now.
    pub auto_fix: bool,
    pub auto_merge: PrAutoMerge,
    pub auto_archive_on_close: bool,
    // Inert until the review-comment stage; always false now.
    pub untrusted_handled: bool,
    // When the status last changed, in milliseconds since the Unix epoch.
    #[ts(type = "number")]
    pub updated_at: u64,
    // Why the last poll failed; the rest is the last good status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// Where a pull request is in its life; `draft` is an open pull request not ready for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

/// The check counts of a pull request's latest commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PrChecks {
    pub passed: u32,
    pub failed: u32,
    pub pending: u32,
    // Up to 20 names, each capped in length.
    pub failing_names: Vec<String>,
}

/// GitHub's review decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PrReview {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

/// Whether the branch merges cleanly into its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PrConflictState {
    Mergeable,
    Conflicting,
    Unknown,
}

/// A way GitHub merges a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PrMergeMethod {
    Squash,
    Merge,
    Rebase,
}

/// A pull request's GitHub auto-merge request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PrAutoMerge {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub method: Option<PrMergeMethod>,
    // Why it cannot be turned on for this pull request now (a draft, a repository that does not
    // allow it, a merge queue, a closed pull request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason_unavailable: Option<String>,
}
