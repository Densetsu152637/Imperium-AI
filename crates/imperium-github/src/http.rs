//! [`HttpGitHub`]: the GitHub client over GraphQL and REST. It sends the token only to its base
//! URL, never follows a redirect with the token, scrubs every error, and slows down when GitHub
//! says it is rate limited.
//!
//! Proxies: the underlying `reqwest` client keeps honouring `HTTPS_PROXY`, `HTTP_PROXY` and
//! `NO_PROXY`, because users behind a corporate proxy need it. The proxy sees only a TLS tunnel
//! (`CONNECT api.github.com:443`); the certificate of `api.github.com` is still verified, so a
//! proxy cannot read the token or change the host. A proxy is therefore part of the trust path
//! only for availability.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;
use reqwest::{Client, Method, StatusCode, redirect};
use serde_json::{Value, json};

use crate::secret::{Secret, TokenSource, scrub};
use crate::types::{
    AutoMerge, MergeSettings, Permission, PrStatus, ReviewComment, ReviewThread, parse_checks,
    parse_comment, parse_status,
};
use crate::{FailingPr, GhFuture, GitHub, job_id, queries};

/// The only host the daemon sends the token to. No setting, environment variable or file changes
/// it; a different base URL exists only as the argument of [`HttpGitHub::new`], for tests.
pub const API_BASE: &str = "https://api.github.com";

const MAX_LOG_BYTES: usize = 64 * 1024;
const MAX_COMMENT_PAGES: usize = 10;

#[derive(Default)]
struct Limits {
    /// No request before this.
    blocked_until: Option<Instant>,
    /// When the rate-limit window resets, from `x-ratelimit-reset`.
    reset_at: Option<Instant>,
    graphql_remaining: Option<u32>,
    last_cost: u32,
}

pub struct HttpGitHub {
    client: Client,
    base: String,
    tokens: Arc<dyn TokenSource>,
    token: Mutex<Option<Secret>>,
    limits: Mutex<Limits>,
    /// Longest a call sleeps for the rate limit; a longer wait is an error instead.
    max_wait: Duration,
    /// GraphQL points kept back: a query is deferred when fewer than `reserve + last cost` remain.
    reserve: u32,
}

impl fmt::Debug for HttpGitHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpGitHub")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

fn split_repo(repo: &str) -> Result<(&str, &str), String> {
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if valid(owner) && valid(name) => Ok((owner, name)),
        _ => Err(format!("not an owner/name repository: {repo:?}")),
    }
}

#[cfg(feature = "test-host")]
fn is_loopback_http(base: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|prefix| {
            base.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
        })
}

impl HttpGitHub {
    /// The client for `https://api.github.com`: what the daemon builds.
    pub fn api(tokens: Arc<dyn TokenSource>) -> Result<Self, String> {
        Self::build(API_BASE.to_string(), tokens)
    }

    /// A client for another base URL, for in-process tests against the fake server. It exists
    /// only with the `test-host` feature, which only this crate's dev-dependencies enable, so no
    /// shipped code can pass a host. Plain `http` is accepted only for a loopback address.
    #[cfg(feature = "test-host")]
    pub fn new(base_url: &str, tokens: Arc<dyn TokenSource>) -> Result<Self, String> {
        let base = base_url.trim_end_matches('/');
        if !(base.starts_with("https://") || is_loopback_http(base)) {
            return Err("the GitHub base URL must be https, or http on loopback".to_string());
        }
        Self::build(base.to_string(), tokens)
    }

    fn build(base: String, tokens: Arc<dyn TokenSource>) -> Result<Self, String> {
        let client = Client::builder()
            // The token must never follow a redirect to another host.
            .redirect(redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .user_agent("imperium-github")
            .build()
            .map_err(|error| error.without_url().to_string())?;
        Ok(Self {
            client,
            base,
            tokens,
            token: Mutex::new(None),
            limits: Mutex::new(Limits::default()),
            max_wait: Duration::from_secs(60),
            reserve: 100,
        })
    }

    /// Sets the longest rate-limit sleep and the GraphQL points kept in reserve.
    pub fn with_rate_policy(mut self, max_wait: Duration, graphql_reserve: u32) -> Self {
        self.max_wait = max_wait;
        self.reserve = graphql_reserve;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn scrub(&self, text: &str) -> String {
        let token = self.token.lock().expect("token lock").clone();
        scrub(text, token.as_ref())
    }

    async fn current_token(&self, refresh: bool) -> Result<Secret, String> {
        let cached = self.token.lock().expect("token lock").clone();
        if let (false, Some(token)) = (refresh, cached) {
            return Ok(token);
        }
        let fresh = self.tokens.token().await?;
        *self.token.lock().expect("token lock") = Some(fresh.clone());
        Ok(fresh)
    }

    /// Waits out a rate limit GitHub reported, or fails when the wait is longer than `max_wait`.
    async fn gate(&self, graphql: bool) -> Result<(), String> {
        let wait = {
            let limits = self.limits.lock().expect("limits lock");
            let now = Instant::now();
            let mut until = limits.blocked_until.filter(|t| *t > now);
            let low = graphql
                && matches!(
                    (limits.graphql_remaining, limits.reset_at),
                    (Some(remaining), Some(reset))
                        if reset > now && remaining < limits.last_cost.saturating_add(self.reserve)
                );
            if low {
                until = until.max(limits.reset_at);
            }
            until.map(|t| t - now)
        };
        match wait {
            None => Ok(()),
            Some(wait) if wait > self.max_wait => Err(format!(
                "GitHub rate limit: the next call is allowed in {}s",
                wait.as_secs() + 1
            )),
            Some(wait) => {
                tokio::time::sleep(wait).await;
                Ok(())
            }
        }
    }

    /// Reads `Retry-After`, `x-ratelimit-remaining` and `x-ratelimit-reset` from a response.
    fn note_headers(&self, headers: &HeaderMap) {
        let number = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        let now = Instant::now();
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut limits = self.limits.lock().expect("limits lock");
        if let Some(seconds) = number("retry-after") {
            let until = now + Duration::from_secs(seconds.min(3600));
            limits.blocked_until = limits.blocked_until.max(Some(until));
        }
        if let Some(reset) = number("x-ratelimit-reset") {
            let delay = Duration::from_secs(reset.saturating_sub(epoch).min(3600));
            limits.reset_at = Some(now + delay);
            if number("x-ratelimit-remaining") == Some(0) && !delay.is_zero() {
                limits.blocked_until = limits.blocked_until.max(Some(now + delay));
            }
        }
    }

    /// One request with the token. A 401 fetches a fresh token from the source and retries once;
    /// a rate-limit answer is recorded and returned as an error. Other statuses come back to
    /// the caller.
    async fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<&Value>,
        graphql: bool,
    ) -> Result<(StatusCode, HeaderMap, Vec<u8>), String> {
        let mut refresh = false;
        for attempt in 0..2 {
            self.gate(graphql).await?;
            let token = self.current_token(refresh).await?;
            let mut request = self
                .client
                .request(method.clone(), url)
                .bearer_auth(token.expose())
                .header("accept", "application/vnd.github+json")
                .header("x-github-api-version", "2022-11-28");
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request
                .send()
                .await
                .map_err(|error| scrub(&error.without_url().to_string(), Some(&token)))?;
            let status = response.status();
            let headers = response.headers().clone();
            self.note_headers(&headers);
            let bytes = response
                .bytes()
                .await
                .map_err(|error| scrub(&error.without_url().to_string(), Some(&token)))?
                .to_vec();
            if status == StatusCode::UNAUTHORIZED {
                if attempt == 0 {
                    refresh = true;
                    continue;
                }
                return Err("GitHub rejected the token (401)".to_string());
            }
            let limited = headers.contains_key("retry-after")
                || headers
                    .get("x-ratelimit-remaining")
                    .is_some_and(|v| v.as_bytes() == b"0");
            if status == StatusCode::TOO_MANY_REQUESTS
                || (status == StatusCode::FORBIDDEN && limited)
            {
                return Err(format!("GitHub rate limit ({status}); backing off"));
            }
            return Ok((status, headers, bytes));
        }
        Err("GitHub rejected the token (401)".to_string())
    }

    /// An error for a status that is neither success nor redirect, with GitHub's message scrubbed.
    fn check(&self, status: StatusCode, bytes: &[u8]) -> Result<(), String> {
        if status.is_success() || status.is_redirection() {
            return Ok(());
        }
        let message = serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_string))
            .unwrap_or_default();
        let message: String = message.chars().take(200).collect();
        Err(format!(
            "GitHub returned {status}: {}",
            self.scrub(&message)
        ))
    }

    async fn rest(&self, path: &str, allow_missing: bool) -> Result<Option<Value>, String> {
        let url = format!("{}{path}", self.base);
        let (status, _, bytes) = self.send(Method::GET, &url, None, false).await?;
        if allow_missing && status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        self.check(status, &bytes)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("GitHub sent unreadable JSON: {error}"))
    }

    async fn graphql(
        &self,
        operation: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value, String> {
        let body = json!({"query": query, "operationName": operation, "variables": variables});
        let url = format!("{}/graphql", self.base);
        let (status, _, bytes) = self.send(Method::POST, &url, Some(&body), true).await?;
        self.check(status, &bytes)?;
        let reply: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("GitHub sent unreadable JSON: {error}"))?;
        if let Some(first) = reply["errors"].as_array().and_then(|e| e.first()) {
            let message: String = first["message"]
                .as_str()
                .unwrap_or("error")
                .chars()
                .take(200)
                .collect();
            return Err(format!("GitHub GraphQL error: {}", self.scrub(&message)));
        }
        let data = reply["data"].clone();
        if let Some(cost) = data["rateLimit"]["cost"].as_u64() {
            let mut limits = self.limits.lock().expect("limits lock");
            limits.last_cost = u32::try_from(cost).unwrap_or(u32::MAX);
            limits.graphql_remaining = data["rateLimit"]["remaining"]
                .as_u64()
                .map(|r| u32::try_from(r).unwrap_or(u32::MAX));
        }
        Ok(data)
    }

    async fn status(&self, repo: &str, number: u64) -> Result<PrStatus, String> {
        let (owner, name) = split_repo(repo)?;
        let data = self
            .graphql(
                queries::PR_STATUS,
                &queries::pr_status(),
                json!({"owner": owner, "name": name, "number": number}),
            )
            .await?;
        let pr = &data["repository"]["pullRequest"];
        if pr.is_null() {
            return Err(format!("pull request {repo}#{number} not found"));
        }
        Ok(parse_status(pr))
    }

    async fn failing(&self, repo: &str) -> Result<Vec<FailingPr>, String> {
        let (owner, name) = split_repo(repo)?;
        let data = self
            .graphql(
                queries::FAILING_PRS,
                &queries::failing_prs(),
                json!({"owner": owner, "name": name}),
            )
            .await?;
        let me = data["viewer"]["login"].as_str().unwrap_or_default();
        let nodes = data["repository"]["pullRequests"]["nodes"].as_array();
        Ok(nodes
            .into_iter()
            .flatten()
            .filter(|pr| pr["author"]["login"].as_str() == Some(me))
            .filter_map(|pr| {
                let failed: Vec<_> = parse_checks(pr)
                    .into_iter()
                    .filter(|c| c.is_failed())
                    .collect();
                let first = failed.first()?;
                Some(FailingPr {
                    number: pr["number"].as_u64()?,
                    branch: pr["headRefName"].as_str()?.to_string(),
                    head_sha: pr["headRefOid"].as_str()?.to_string(),
                    details_url: first.details_url.clone(),
                    checks: failed.iter().map(|c| c.name.clone()).collect(),
                })
            })
            .collect())
    }

    async fn threads(&self, repo: &str, number: u64) -> Result<Vec<ReviewThread>, String> {
        let (owner, name) = split_repo(repo)?;
        let data = self
            .graphql(
                queries::REVIEW_THREADS,
                &queries::review_threads(),
                json!({"owner": owner, "name": name, "number": number}),
            )
            .await?;
        let nodes = data["repository"]["pullRequest"]["reviewThreads"]["nodes"].as_array();
        Ok(nodes
            .into_iter()
            .flatten()
            .map(|thread| {
                let path = thread["path"].as_str().unwrap_or_default();
                ReviewThread {
                    id: thread["id"].as_str().unwrap_or_default().to_string(),
                    is_resolved: thread["isResolved"].as_bool().unwrap_or_default(),
                    is_outdated: thread["isOutdated"].as_bool().unwrap_or_default(),
                    path: path.to_string(),
                    comments: thread["comments"]["nodes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| parse_comment(c, Some(path)))
                        .collect(),
                }
            })
            .collect())
    }

    async fn comments_after(
        &self,
        repo: &str,
        number: u64,
        since: Option<u64>,
    ) -> Result<Vec<ReviewComment>, String> {
        let (owner, name) = split_repo(repo)?;
        let mut found = Vec::new();
        for page in 1..=MAX_COMMENT_PAGES {
            let path =
                format!("/repos/{owner}/{name}/pulls/{number}/comments?per_page=100&page={page}");
            let Some(Value::Array(items)) = self.rest(&path, false).await? else {
                break;
            };
            let count = items.len();
            found.extend(
                items
                    .iter()
                    .filter_map(|c| parse_comment(c, None))
                    .filter(|c| since.is_none_or(|id| c.id > id)),
            );
            if count < 100 {
                break;
            }
        }
        Ok(found)
    }

    async fn permission(&self, repo: &str, login: &str) -> Result<Permission, String> {
        let (owner, name) = split_repo(repo)?;
        let valid = !login.is_empty()
            && login
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !valid {
            return Ok(Permission::None);
        }
        let path = format!("/repos/{owner}/{name}/collaborators/{login}/permission");
        let value = self.rest(&path, true).await?;
        Ok(value.map_or(Permission::None, |v| {
            Permission::parse(
                v["role_name"]
                    .as_str()
                    .or(v["permission"].as_str())
                    .unwrap_or_default(),
            )
        }))
    }

    async fn merge_settings(&self, repo: &str) -> Result<MergeSettings, String> {
        let (owner, name) = split_repo(repo)?;
        let value = self
            .rest(&format!("/repos/{owner}/{name}"), false)
            .await?
            .unwrap_or(Value::Null);
        let flag = |key: &str| value[key].as_bool().unwrap_or_default();
        Ok(MergeSettings {
            squash: flag("allow_squash_merge"),
            merge: flag("allow_merge_commit"),
            rebase: flag("allow_rebase_merge"),
            auto_merge: flag("allow_auto_merge"),
        })
    }

    async fn reply(&self, thread_id: &str, body: &str) -> Result<u64, String> {
        let data = self
            .graphql(
                queries::REPLY,
                queries::REPLY_TO_THREAD,
                json!({"thread": thread_id, "body": body}),
            )
            .await?;
        data["addPullRequestReviewThreadReply"]["comment"]["databaseId"]
            .as_u64()
            .ok_or_else(|| "GitHub did not return the new comment".to_string())
    }

    async fn auto_merge(&self, repo: &str, number: u64, change: &AutoMerge) -> Result<(), String> {
        let node = self.status(repo, number).await?.node_id;
        match change {
            AutoMerge::Enable {
                method,
                expected_head_oid,
            } => {
                let vars =
                    json!({"pr": node, "method": method.graphql(), "oid": expected_head_oid});
                self.graphql(queries::ENABLE_AUTO_MERGE, queries::ENABLE, vars)
                    .await?;
            }
            AutoMerge::Disable => {
                self.graphql(
                    queries::DISABLE_AUTO_MERGE,
                    queries::DISABLE,
                    json!({"pr": node}),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// The tail of a job's log. GitHub answers with a redirect to storage; that second request
    /// carries no token.
    async fn log(&self, repo: &str, details_url: &str) -> Result<String, String> {
        let Some(job) = job_id(details_url) else {
            return Ok(format!("(no Actions log; see {details_url})"));
        };
        let (owner, name) = split_repo(repo)?;
        let url = format!("{}/repos/{owner}/{name}/actions/jobs/{job}/logs", self.base);
        let (status, headers, bytes) = self.send(Method::GET, &url, None, false).await?;
        self.check(status, &bytes)?;
        let bytes = if status.is_redirection() {
            let location = headers
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or("GitHub sent a redirect without a location")?;
            let target = if location.starts_with('/') {
                format!("{}{location}", self.base)
            } else if location.starts_with("https://") {
                location.to_string()
            } else {
                return Err("refusing a log location that is not https".to_string());
            };
            let response = self
                .client
                .get(&target)
                .send()
                .await
                .map_err(|error| self.scrub(&error.without_url().to_string()))?;
            if !response.status().is_success() {
                return Err(format!("log download returned {}", response.status()));
            }
            response
                .bytes()
                .await
                .map_err(|error| self.scrub(&error.without_url().to_string()))?
                .to_vec()
        } else {
            bytes
        };
        let text = String::from_utf8_lossy(&bytes);
        let start = text.len().saturating_sub(MAX_LOG_BYTES);
        let start = (start..=text.len())
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(text.len());
        Ok(self.scrub(&text[start..]))
    }
}

impl GitHub for HttpGitHub {
    fn failing_prs<'a>(&'a self, repo: &'a str) -> GhFuture<'a, Vec<FailingPr>> {
        Box::pin(self.failing(repo))
    }

    fn failure_log<'a>(&'a self, repo: &'a str, details_url: &'a str) -> GhFuture<'a, String> {
        Box::pin(self.log(repo, details_url))
    }

    fn pr_state<'a>(&'a self, repo: &'a str, number: u64) -> GhFuture<'a, String> {
        Box::pin(async move { Ok(self.status(repo, number).await?.state) })
    }

    fn pr_status<'a>(&'a self, repo: &'a str, number: u64) -> GhFuture<'a, PrStatus> {
        Box::pin(self.status(repo, number))
    }

    fn review_threads<'a>(&'a self, repo: &'a str, number: u64) -> GhFuture<'a, Vec<ReviewThread>> {
        Box::pin(self.threads(repo, number))
    }

    fn new_comments<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
        since: Option<u64>,
    ) -> GhFuture<'a, Vec<ReviewComment>> {
        Box::pin(self.comments_after(repo, number, since))
    }

    fn collaborator_permission<'a>(
        &'a self,
        repo: &'a str,
        login: &'a str,
    ) -> GhFuture<'a, Permission> {
        Box::pin(self.permission(repo, login))
    }

    fn repo_merge_settings<'a>(&'a self, repo: &'a str) -> GhFuture<'a, MergeSettings> {
        Box::pin(self.merge_settings(repo))
    }

    fn reply_to_thread<'a>(&'a self, thread_id: &'a str, body: &'a str) -> GhFuture<'a, u64> {
        Box::pin(self.reply(thread_id, body))
    }

    fn set_auto_merge<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
        change: &'a AutoMerge,
    ) -> GhFuture<'a, ()> {
        Box::pin(self.auto_merge(repo, number, change))
    }
}
