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

/// `pull_requests.auto_merge_challenge`: the single-use challenge that `pull_requests.set_auto_merge`
/// with `enabled: true` needs. Answered only on the local socket, to the desktop app installed
/// beside `imperium-core`, and bound to this connection, the session's pull request `url`, the
/// `head_sha` the user saw and the `merge_method` the user chose. It expires after two minutes.
/// Called by the desktop app's own process, never by its webview, a paired phone, an agent, a
/// hook, a plugin or a mod.
#[derive(Debug)]
pub struct PullRequestsAutoMergeChallenge;

impl Method for PullRequestsAutoMergeChallenge {
    const NAME: &'static str = "pull_requests.auto_merge_challenge";
    type Params = PullRequestsAutoMergeChallengeParams;
    type Result = PullRequestsAutoMergeChallengeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsAutoMergeChallengeParams {
    pub session_id: String,
    pub url: String,
    pub head_sha: String,
    pub merge_method: PrMergeMethod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsAutoMergeChallengeResult {
    pub challenge: String,
    // When the challenge stops being accepted, in Unix milliseconds.
    #[ts(type = "number")]
    pub expires_unix_ms: u64,
}

/// `pull_requests.set_auto_merge`: turns GitHub's auto-merge on or off for a session's pull
/// request and answers with its new status. `enabled: true` needs `merge_method` and the
/// `challenge` of `pull_requests.auto_merge_challenge` for the same session, `url`, `head_sha`
/// and method, and is refused for any caller but the desktop app. GitHub merges the pull request
/// itself once its own rules pass, and refuses if the branch is no longer at `head_sha`.
/// `enabled: false` needs no challenge and any local caller may send it. A paired phone may send
/// neither.
#[derive(Debug)]
pub struct PullRequestsSetAutoMerge;

impl Method for PullRequestsSetAutoMerge {
    const NAME: &'static str = "pull_requests.set_auto_merge";
    type Params = PullRequestsSetAutoMergeParams;
    type Result = PrStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsSetAutoMergeParams {
    pub session_id: String,
    pub url: String,
    pub head_sha: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merge_method: Option<PrMergeMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub challenge: Option<String>,
}

/// `pull_requests.set_monitor`: sets what the daemon does for a session's pull request, and
/// answers with its new status. `auto_fix` (the card's "Fix CI and review comments" switch;
/// `address_comments` is the same switch) lets new review comments, reviews and comments from
/// people with write access wake the session, and is refused for a pull request the signed-in
/// account did not open. `auto_archive_on_close` archives the session when the pull request
/// merges or closes. `ack_untrusted` clears the notice that the session handled untrusted
/// comment text. A paired phone may not call it.
#[derive(Debug)]
pub struct PullRequestsSetMonitor;

impl Method for PullRequestsSetMonitor {
    const NAME: &'static str = "pull_requests.set_monitor";
    type Params = PullRequestsSetMonitorParams;
    type Result = PrStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PullRequestsSetMonitorParams {
    pub session_id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_fix: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_archive_on_close: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ack_untrusted: Option<bool>,
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
    // Whether new review comments from people with write access wake the session.
    pub auto_fix: bool,
    pub auto_merge: PrAutoMerge,
    pub auto_archive_on_close: bool,
    // The session handled review text written by others, until the user acknowledges it
    // (`pull_requests.set_monitor` with `ack_untrusted`).
    pub untrusted_handled: bool,
    // Comments from people without write access that did not wake the session, since the user
    // last acknowledged. Absent from a daemon that predates it (read as 0).
    #[serde(default)]
    pub ignored_comments: u32,
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
