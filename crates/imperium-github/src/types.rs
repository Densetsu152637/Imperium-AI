//! What the client reports about a pull request, as plain data. Every string here was written by
//! someone on GitHub and is untrusted; the daemon frames it before a model sees it.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// `QUEUED`, `IN_PROGRESS` or `COMPLETED` (a commit status maps to these).
    pub status: String,
    pub conclusion: Option<String>,
    pub details_url: String,
}

impl Check {
    pub fn is_failed(&self) -> bool {
        matches!(
            self.conclusion.as_deref(),
            Some("FAILURE" | "TIMED_OUT" | "ERROR" | "STARTUP_FAILURE")
        )
    }
}

/// How GitHub merges a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    /// The GraphQL `PullRequestMergeMethod` value.
    pub fn graphql(self) -> &'static str {
        match self {
            Self::Squash => "SQUASH",
            Self::Merge => "MERGE",
            Self::Rebase => "REBASE",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_uppercase().as_str() {
            "SQUASH" => Some(Self::Squash),
            "MERGE" => Some(Self::Merge),
            "REBASE" => Some(Self::Rebase),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    /// The GraphQL node id, needed by the auto-merge mutations.
    pub node_id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub is_draft: bool,
    pub head_ref: String,
    pub head_sha: String,
    /// The branch the pull request merges into.
    pub base_ref: String,
    /// Waiting in the base branch's merge queue.
    pub is_in_merge_queue: bool,
    pub review_decision: Option<String>,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: String,
    pub merge_state: String,
    /// The method of a pending auto-merge request.
    pub auto_merge: Option<MergeMethod>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub id: u64,
    pub author: String,
    /// GitHub's `authorAssociation`; not enough alone to trust the author (plan F5).
    pub association: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
    pub reply_to: Option<u64>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    pub id: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub path: String,
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Admin,
    Maintain,
    Write,
    Triage,
    Read,
    None,
}

impl Permission {
    pub fn parse(value: &str) -> Self {
        match value {
            "admin" => Self::Admin,
            "maintain" => Self::Maintain,
            "write" => Self::Write,
            "triage" => Self::Triage,
            "read" => Self::Read,
            _ => Self::None,
        }
    }

    /// Only `write`, `maintain` and `admin` may wake a session (plan F5, D3).
    pub fn can_write(self) -> bool {
        matches!(self, Self::Admin | Self::Maintain | Self::Write)
    }
}

/// Which merge methods a repository allows (decision D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeSettings {
    pub squash: bool,
    pub merge: bool,
    pub rebase: bool,
    pub auto_merge: bool,
}

impl MergeSettings {
    /// The allowed methods in the order `squash`, `merge`, `rebase`.
    pub fn allowed(&self) -> Vec<MergeMethod> {
        [
            (self.squash, MergeMethod::Squash),
            (self.merge, MergeMethod::Merge),
            (self.rebase, MergeMethod::Rebase),
        ]
        .into_iter()
        .filter_map(|(on, method)| on.then_some(method))
        .collect()
    }
}

/// A change to a pull request's auto-merge request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoMerge {
    /// GitHub refuses it if the branch is no longer at `expected_head_oid`.
    Enable {
        method: MergeMethod,
        expected_head_oid: String,
    },
    Disable,
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

/// A check run or commit status node of `statusCheckRollup.contexts`.
pub(crate) fn parse_check(node: &Value) -> Check {
    if node["__typename"] == "StatusContext" {
        let (status, conclusion) = match node["state"].as_str().unwrap_or_default() {
            "SUCCESS" => ("COMPLETED", Some("SUCCESS")),
            "FAILURE" | "ERROR" => ("COMPLETED", Some("FAILURE")),
            _ => ("IN_PROGRESS", None),
        };
        return Check {
            name: text(node, "context"),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
            details_url: text(node, "targetUrl"),
        };
    }
    Check {
        name: text(node, "name"),
        status: text(node, "status"),
        conclusion: node["conclusion"].as_str().map(str::to_string),
        details_url: text(node, "detailsUrl"),
    }
}

/// The checks of a pull request's last commit.
pub(crate) fn parse_checks(pull_request: &Value) -> Vec<Check> {
    pull_request["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"]
        .as_array()
        .map(|nodes| nodes.iter().map(parse_check).collect())
        .unwrap_or_default()
}

pub(crate) fn parse_status(pr: &Value) -> PrStatus {
    PrStatus {
        node_id: text(pr, "id"),
        number: pr["number"].as_u64().unwrap_or_default(),
        title: text(pr, "title"),
        url: text(pr, "url"),
        state: text(pr, "state"),
        is_draft: pr["isDraft"].as_bool().unwrap_or_default(),
        head_ref: text(pr, "headRefName"),
        head_sha: text(pr, "headRefOid"),
        base_ref: text(pr, "baseRefName"),
        is_in_merge_queue: pr["isInMergeQueue"].as_bool().unwrap_or_default(),
        review_decision: pr["reviewDecision"].as_str().map(str::to_string),
        mergeable: text(pr, "mergeable"),
        merge_state: text(pr, "mergeStateStatus"),
        auto_merge: pr["autoMergeRequest"]["mergeMethod"]
            .as_str()
            .and_then(MergeMethod::parse),
        checks: parse_checks(pr),
    }
}

/// A comment from GraphQL (`databaseId`, `author.login`) or REST (`id`, `user.login`).
pub(crate) fn parse_comment(node: &Value, path: Option<&str>) -> Option<ReviewComment> {
    let id = node["databaseId"]
        .as_u64()
        .or_else(|| node["id"].as_u64())?;
    let author = node["author"]["login"]
        .as_str()
        .or_else(|| node["user"]["login"].as_str())
        .unwrap_or("ghost");
    Some(ReviewComment {
        id,
        author: author.to_string(),
        association: node["authorAssociation"]
            .as_str()
            .or_else(|| node["author_association"].as_str())
            .unwrap_or("NONE")
            .to_string(),
        body: text(node, "body"),
        created_at: node["createdAt"]
            .as_str()
            .or_else(|| node["created_at"].as_str())
            .unwrap_or_default()
            .to_string(),
        url: node["url"]
            .as_str()
            .or_else(|| node["html_url"].as_str())
            .unwrap_or_default()
            .to_string(),
        reply_to: node["replyTo"]["databaseId"]
            .as_u64()
            .or_else(|| node["in_reply_to_id"].as_u64()),
        path: path
            .map(str::to_string)
            .or_else(|| node["path"].as_str().map(str::to_string)),
    })
}
