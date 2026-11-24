//! Every GraphQL document the client sends, in one place, so a change in GitHub's schema is a
//! one-file edit. Shapes follow GitHub's documented GraphQL schema; the fake server in
//! `imperium-github-fake` answers the same operation names with the same shapes.

const ROLLUP: &str =
    "commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
  __typename
  ... on CheckRun { name status conclusion detailsUrl }
  ... on StatusContext { context state targetUrl } } } } } } }";

pub(crate) const RATE: &str = "rateLimit { cost remaining }";

/// The operation name sent with each document (the fake dispatches on it).
pub(crate) const PR_STATUS: &str = "PrStatus";
pub(crate) const VIEWER: &str = "Viewer";
pub(crate) const SEARCH_PRS: &str = "SearchPrs";
pub(crate) const REVIEW_THREADS: &str = "ReviewThreads";
pub(crate) const REPLY: &str = "ReplyToThread";
pub(crate) const ENABLE_AUTO_MERGE: &str = "EnableAutoMerge";
pub(crate) const DISABLE_AUTO_MERGE: &str = "DisableAutoMerge";

pub(crate) fn pr_status() -> String {
    format!(
        "query PrStatus($owner: String!, $name: String!, $number: Int!) {{
  repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{
    id number title url state isDraft headRefName headRefOid baseRefName isInMergeQueue reviewDecision mergeable mergeStateStatus
    autoMergeRequest {{ mergeMethod }}
    {ROLLUP} }} }}
  {RATE} }}"
    )
}

pub(crate) fn viewer() -> String {
    format!("query Viewer {{ viewer {{ login }} {RATE} }}")
}

/// The open pull requests a search finds, with their checks. Searched on the server so a
/// repository with many open pull requests does not hide the user's.
pub(crate) fn search_prs() -> String {
    format!(
        "query SearchPrs($q: String!) {{
  search(query: $q, type: ISSUE, first: 100) {{ nodes {{ ... on PullRequest {{
    number headRefName headRefOid {ROLLUP} }} }} }}
  {RATE} }}"
    )
}

pub(crate) fn review_threads() -> String {
    format!(
        "query ReviewThreads($owner: String!, $name: String!, $number: Int!) {{
  repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{
    reviewThreads(first: 100) {{ nodes {{ id isResolved isOutdated path
      comments(first: 100) {{ nodes {{ databaseId author {{ login }} authorAssociation body createdAt url
        replyTo {{ databaseId }} }} }} }} }} }} }}
  {RATE} }}"
    )
}

pub(crate) const REPLY_TO_THREAD: &str = "mutation ReplyToThread($thread: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, body: $body}) {
    comment { databaseId url } } }";

pub(crate) const ENABLE: &str = "mutation EnableAutoMerge($pr: ID!, $method: PullRequestMergeMethod!, $oid: GitObjectID!) {
  enablePullRequestAutoMerge(input: {pullRequestId: $pr, mergeMethod: $method, expectedHeadOid: $oid}) {
    pullRequest { number } } }";

pub(crate) const DISABLE: &str = "mutation DisableAutoMerge($pr: ID!) {
  disablePullRequestAutoMerge(input: {pullRequestId: $pr}) { pullRequest { number } } }";
