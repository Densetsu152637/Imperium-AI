use super::*;

// ---- Reviewing a session's changes ----

/// One hunk of a file's unified diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReviewHunk {
    /// Names this hunk as it is now, for `session.review_hunk`; a hunk that changed since it was
    /// listed no longer matches.
    pub id: String,
    /// `@@ -old_start,old_lines +new_start,new_lines @@`.
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Each line with its sign first: ` ` (context), `-` (removed), or `+` (added), without its
    /// line ending.
    pub lines: Vec<String>,
}

/// A file the session changed, against the review's base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReviewFile {
    /// Names the file as it is now (both sides), for a whole-file `session.review_hunk`
    /// (`hunk: "file:<id>"`); a file that changed since it was listed no longer matches.
    pub id: String,
    /// The repository's top folder.
    pub repo: String,
    /// Relative to `repo`, with `/`.
    pub path: String,
    #[ts(type = "\"added\" | \"modified\" | \"deleted\"")]
    pub status: String,
    /// Binary (or over 2 MiB): no hunks; accepted or rejected whole.
    pub binary: bool,
    pub hunks: Vec<ReviewHunk>,
}

/// `session.review`: the files a session changed since one of its checkpoints (its first, the
/// session's start, by default), as unified diff hunks. Hunks already accepted are not shown.
#[derive(Debug)]
pub struct SessionReview;

impl Method for SessionReview {
    const NAME: &'static str = "session.review";
    type Params = SessionReviewParams;
    type Result = SessionReviewResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewParams {
    pub session_id: String,
    /// The checkpoint to compare against; the session's first when left out.
    #[serde(default)]
    pub since: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewResult {
    /// Whether the session has checkpoints to review against (see `session.checkpoints`).
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// The checkpoint compared against; none before the session's first.
    #[serde(default)]
    pub since: Option<u32>,
    /// The session's checkpoints, oldest first, to pick another.
    pub checkpoints: Vec<SessionCheckpoint>,
    pub files: Vec<ReviewFile>,
    /// A commit message for the changes: the session's title and the files.
    pub suggested_message: String,
    /// The session's repositories (top folders), for commits and pull requests.
    pub repos: Vec<String>,
    /// Whether `gh` was found, so a pull request can be opened from the app.
    pub gh: bool,
}

/// What `session.review_hunk` does with a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewAction {
    /// Keep it: it joins the review's base and is no longer shown.
    Accept,
    /// Revert it in the working tree.
    Reject,
}

/// `session.review_hunk`: accepts or rejects one hunk, or a whole file with `hunk: "file:<id>"`.
/// Refused while a turn is running.
#[derive(Debug)]
pub struct SessionReviewHunk;

impl Method for SessionReviewHunk {
    const NAME: &'static str = "session.review_hunk";
    type Params = SessionReviewHunkParams;
    type Result = SessionReviewHunkResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewHunkParams {
    pub session_id: String,
    /// The review's checkpoint, as `session.review` returned it.
    #[serde(default)]
    pub since: Option<u32>,
    pub repo: String,
    pub path: String,
    /// A hunk's `id`, or `file:<id>` with the file's `id` for the whole file.
    pub hunk: String,
    pub action: ReviewAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewHunkResult {
    /// The file as it is now; none when it has nothing left to review.
    #[serde(default)]
    pub file: Option<ReviewFile>,
}

/// `session.review_commit`: commits the files the session changed since the review's
/// checkpoint in one repository, as they now are, on its current branch (hooks off). Gated by
/// the `git_commit` permission rule: denied, it fails; without an allow rule it returns
/// `needs_confirmation` until called again with `confirmed`. The committed changes are then
/// accepted.
#[derive(Debug)]
pub struct SessionReviewCommit;

impl Method for SessionReviewCommit {
    const NAME: &'static str = "session.review_commit";
    type Params = SessionReviewCommitParams;
    type Result = SessionReviewCommitResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewCommitParams {
    pub session_id: String,
    #[serde(default)]
    pub since: Option<u32>,
    pub repo: String,
    pub message: String,
    /// The user confirmed it (asked by default).
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewCommitResult {
    pub committed: bool,
    /// Nothing was done: ask the user, then call again with `confirmed`.
    pub needs_confirmation: bool,
    /// The new commit.
    #[serde(default)]
    pub commit: Option<String>,
    /// The files committed (or that would be).
    pub files: Vec<String>,
}

/// `session.review_pull_request`: pushes the repository's current branch to `origin` and opens
/// a pull request with `gh`. Without `gh` it opens nothing and returns the commands to run.
/// Gated like `session.review_commit`, by the `open_pull_request` permission rule.
#[derive(Debug)]
pub struct SessionReviewPullRequest;

impl Method for SessionReviewPullRequest {
    const NAME: &'static str = "session.review_pull_request";
    type Params = SessionReviewPullRequestParams;
    type Result = SessionReviewPullRequestResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewPullRequestParams {
    pub session_id: String,
    pub repo: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// The branch to merge into; the repository's default when left out.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReviewPullRequestResult {
    pub opened: bool,
    pub needs_confirmation: bool,
    /// The pull request, when `gh` printed it.
    #[serde(default)]
    pub url: Option<String>,
    /// The branch pushed.
    pub branch: String,
    /// The commands that do it, to run by hand when `gh` is not installed.
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSubscribeParams {
    pub session_id: String,
}

/// Every event the session has emitted so far; later ones arrive as notifications.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSubscribeResult {
    pub history: Vec<crate::session::AgentEvent>,
}
