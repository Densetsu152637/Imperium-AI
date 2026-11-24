//! A fake GitHub for tests: the REST and GraphQL calls `imperium-github` makes, over in-memory
//! pull requests, checks, review threads, collaborators and auto-merge requests. It records every
//! request, rejects one without the expected token, and can script a failing check, a review, a
//! comment, a force-push, a merge, a rate-limit response or a server error.
//!
//! Dev-only: only `[dev-dependencies]` may name this crate, and it is never in a release build.
//! Tests inject it in process by passing [`FakeGithub::url`] to the client's constructor; no
//! setting, environment variable or feature selects a host.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// One request the fake received. The `Authorization` value is never kept, only whether it matched.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: String,
    pub host: String,
    pub body: String,
    /// The GraphQL operation name, when this was a GraphQL call.
    pub operation: Option<String>,
    pub authorized: bool,
}

/// A scripted rate-limit reply for the next request.
#[derive(Debug, Clone, Default)]
pub struct RateLimitReply {
    pub status: u16,
    pub retry_after: Option<u64>,
    /// Seconds from now sent as `x-ratelimit-reset` with `x-ratelimit-remaining: 0`.
    pub reset_in: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub id: u64,
    pub login: String,
    pub association: String,
    pub body: String,
    pub reply_to: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: String,
    pub path: String,
    pub resolved: bool,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone)]
pub struct Pr {
    pub title: String,
    pub author: String,
    pub state: String,
    pub draft: bool,
    pub head_ref: String,
    pub head_sha: String,
    pub review_decision: Option<String>,
    pub mergeable: String,
    /// `(name, status, conclusion)` as the check-run API reports them.
    pub checks: Vec<(String, String, Option<String>)>,
    pub threads: Vec<Thread>,
    /// The method and head OID of the last accepted `enablePullRequestAutoMerge`.
    pub auto_merge: Option<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Repo {
    /// `allow_squash_merge`, `allow_merge_commit`, `allow_rebase_merge`, `allow_auto_merge`.
    pub merge_settings: [bool; 4],
    pub collaborators: HashMap<String, String>,
    pub prs: BTreeMap<u64, Pr>,
}

#[derive(Debug, Default)]
struct World {
    token: String,
    repos: HashMap<String, Repo>,
    requests: Vec<Recorded>,
    mutations: Vec<(String, Value)>,
    rate_limit: Option<RateLimitReply>,
    fail_status: Option<u16>,
    /// `(cost, remaining, seconds until reset)` reported with every GraphQL reply.
    graphql_cost: (u32, u32, u64),
    next_id: u64,
}

type Shared = Arc<Mutex<World>>;

/// A running fake; the server stops when this is dropped.
#[derive(Debug)]
pub struct FakeGithub {
    world: Shared,
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for FakeGithub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeGithub {
    /// Starts a fake on an ephemeral loopback port that accepts only `Bearer <token>`.
    pub async fn start(token: &str) -> Self {
        let world = Arc::new(Mutex::new(World {
            token: token.to_string(),
            graphql_cost: (1, 5000, 3600),
            next_id: 1000,
            ..World::default()
        }));
        let app = Router::new()
            .route("/graphql", post(graphql))
            .route("/blob/{id}", get(blob))
            .fallback(rest)
            .with_state(world.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local address");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { world, addr, task }
    }

    /// The base URL to give the client's constructor.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn w(&self) -> MutexGuard<'_, World> {
        self.world.lock().expect("fake world lock")
    }

    fn with_pr<R>(&self, repo: &str, number: u64, change: impl FnOnce(&mut Pr) -> R) -> R {
        let mut world = self.w();
        let pr = world
            .repos
            .get_mut(repo)
            .and_then(|r| r.prs.get_mut(&number))
            .expect("pull request exists in the fake");
        change(pr)
    }

    /// Accepts a different token from now on, so the old one gets a 401.
    pub fn set_token(&self, token: &str) {
        self.w().token = token.to_string();
    }

    /// Adds a repository (`owner/name`) that allows squash, merge commits and auto-merge.
    pub fn add_repo(&self, repo: &str) {
        self.w().repos.insert(
            repo.to_string(),
            Repo {
                merge_settings: [true, true, false, true],
                collaborators: HashMap::new(),
                prs: BTreeMap::new(),
            },
        );
    }

    pub fn set_merge_settings(
        &self,
        repo: &str,
        squash: bool,
        merge: bool,
        rebase: bool,
        auto: bool,
    ) {
        self.w().repos.get_mut(repo).expect("repo").merge_settings = [squash, merge, rebase, auto];
    }

    /// `permission` is `admin`, `maintain`, `write`, `triage`, `read` or `none`.
    pub fn set_collaborator(&self, repo: &str, login: &str, permission: &str) {
        self.w()
            .repos
            .get_mut(repo)
            .expect("repo")
            .collaborators
            .insert(login.to_string(), permission.to_string());
    }

    pub fn add_pr(&self, repo: &str, number: u64, title: &str, head_ref: &str, head_sha: &str) {
        self.add_pr_by(repo, number, "me", title, head_ref, head_sha);
    }

    /// Like [`FakeGithub::add_pr`], opened by `author` (the fake's signed-in user is `me`).
    pub fn add_pr_by(
        &self,
        repo: &str,
        number: u64,
        author: &str,
        title: &str,
        head_ref: &str,
        head_sha: &str,
    ) {
        let pr = Pr {
            title: title.to_string(),
            author: author.to_string(),
            state: "OPEN".to_string(),
            draft: false,
            head_ref: head_ref.to_string(),
            head_sha: head_sha.to_string(),
            review_decision: None,
            mergeable: "MERGEABLE".to_string(),
            checks: Vec::new(),
            threads: Vec::new(),
            auto_merge: None,
        };
        self.w()
            .repos
            .get_mut(repo)
            .expect("repo")
            .prs
            .insert(number, pr);
    }

    pub fn set_draft(&self, repo: &str, number: u64, draft: bool) {
        self.with_pr(repo, number, |pr| pr.draft = draft);
    }

    pub fn set_review_decision(&self, repo: &str, number: u64, decision: Option<&str>) {
        self.with_pr(repo, number, |pr| {
            pr.review_decision = decision.map(str::to_string);
        });
    }

    /// Adds or replaces a check run; `conclusion` `None` leaves it `IN_PROGRESS`.
    pub fn set_check(&self, repo: &str, number: u64, name: &str, conclusion: Option<&str>) {
        self.with_pr(repo, number, |pr| {
            pr.checks.retain(|(n, ..)| n != name);
            let status = if conclusion.is_some() {
                "COMPLETED"
            } else {
                "IN_PROGRESS"
            };
            pr.checks.push((
                name.to_string(),
                status.to_string(),
                conclusion.map(str::to_string),
            ));
        });
    }

    /// Starts a review thread with one comment; returns the thread id and the comment id.
    pub fn add_review_comment(
        &self,
        repo: &str,
        number: u64,
        login: &str,
        association: &str,
        path: &str,
        body: &str,
    ) -> (String, u64) {
        let id = self.next_id();
        let thread = format!("PRRT_{repo}#{number}-{id}");
        let comment = Comment {
            id,
            login: login.to_string(),
            association: association.to_string(),
            body: body.to_string(),
            reply_to: None,
        };
        self.with_pr(repo, number, |pr| {
            pr.threads.push(Thread {
                id: thread.clone(),
                path: path.to_string(),
                resolved: false,
                comments: vec![comment],
            });
        });
        (thread, id)
    }

    fn next_id(&self) -> u64 {
        let mut world = self.w();
        world.next_id += 1;
        world.next_id
    }

    /// Moves the head to `sha` and drops its checks, as a push does.
    pub fn force_push(&self, repo: &str, number: u64, sha: &str) {
        self.with_pr(repo, number, |pr| {
            pr.head_sha = sha.to_string();
            pr.checks.clear();
        });
    }

    pub fn merge(&self, repo: &str, number: u64) {
        self.with_pr(repo, number, |pr| pr.state = "MERGED".to_string());
    }

    /// Answers the next request with a rate-limit response.
    pub fn rate_limit_next(&self, reply: RateLimitReply) {
        self.w().rate_limit = Some(reply);
    }

    /// Answers the next request with this status (for example 502) and a short error body.
    pub fn fail_next(&self, status: u16) {
        self.w().fail_status = Some(status);
    }

    /// The cost, remaining points and seconds-to-reset every GraphQL reply reports.
    pub fn set_graphql_cost(&self, cost: u32, remaining: u32, reset_in: u64) {
        self.w().graphql_cost = (cost, remaining, reset_in);
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.w().requests.clone()
    }

    /// Every GraphQL mutation the fake accepted, as `(operation, variables)`.
    pub fn mutations(&self) -> Vec<(String, Value)> {
        self.w().mutations.clone()
    }

    pub fn pr(&self, repo: &str, number: u64) -> Pr {
        self.with_pr(repo, number, |pr| pr.clone())
    }
}

fn reply(status: u16, body: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        axum::Json(body),
    )
        .into_response()
}

fn epoch_in(seconds: u64) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + seconds
}

/// Records the request and decides whether it is answered before routing: a scripted failure,
/// a rate limit, or a missing or wrong token.
fn admit(
    world: &mut World,
    method: &str,
    uri: &Uri,
    headers: &HeaderMap,
    body: &str,
    operation: Option<String>,
) -> Option<Response> {
    let expected = format!("Bearer {}", world.token);
    let authorized =
        headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(expected.as_str());
    world.requests.push(Recorded {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().unwrap_or_default().to_string(),
        host: headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string(),
        body: body.to_string(),
        operation,
        authorized,
    });
    if let Some(status) = world.fail_status.take() {
        // Like a server that echoes the request it could not serve: the client must scrub it.
        let message = format!("server error (Authorization: Bearer {})", world.token);
        return Some(reply(status, json!({"message": message})));
    }
    if let Some(limit) = world.rate_limit.take() {
        let mut response = reply(limit.status, json!({"message": "API rate limit exceeded"}));
        let headers = response.headers_mut();
        if let Some(seconds) = limit.retry_after {
            headers.insert("retry-after", HeaderValue::from(seconds));
        }
        if let Some(seconds) = limit.reset_in {
            headers.insert("x-ratelimit-remaining", HeaderValue::from(0u64));
            headers.insert("x-ratelimit-reset", HeaderValue::from(epoch_in(seconds)));
        }
        return Some(response);
    }
    (!authorized).then(|| reply(401, json!({"message": "Bad credentials"})))
}

/// The log's storage host: it must never be sent the GitHub token.
async fn blob(
    State(world): State<Shared>,
    headers: HeaderMap,
    uri: Uri,
    Path(id): Path<String>,
) -> Response {
    world
        .lock()
        .expect("fake world lock")
        .requests
        .push(Recorded {
            method: "GET".to_string(),
            path: uri.path().to_string(),
            query: String::new(),
            host: headers
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            body: String::new(),
            operation: None,
            authorized: headers.contains_key("authorization"),
        });
    // The whole job log: the failed step, then a post-job step.
    let log = [
        format!("log of job {id}: step failed"),
        "##[group]Post job cleanup".to_string(),
        "Post-job noise".to_string(),
        "##[endgroup]".to_string(),
    ];
    format!("{}\n", log.join("\n")).into_response()
}

fn repo_json(repo: &Repo) -> Value {
    let [squash, merge, rebase, auto] = repo.merge_settings;
    json!({"allow_squash_merge": squash, "allow_merge_commit": merge,
           "allow_rebase_merge": rebase, "allow_auto_merge": auto})
}

fn comment_json(pr: u64, thread: &Thread, comment: &Comment) -> Value {
    json!({"id": comment.id, "user": {"login": comment.login},
           "author_association": comment.association, "body": comment.body,
           "created_at": "2026-01-01T00:00:00Z", "path": thread.path,
           "html_url": format!("https://github.com/x/y/pull/{pr}#discussion_r{}", comment.id),
           "in_reply_to_id": comment.reply_to})
}

async fn rest(State(world): State<Shared>, uri: Uri, headers: HeaderMap) -> Response {
    let mut world = world.lock().expect("fake world lock");
    if let Some(early) = admit(&mut world, "GET", &uri, &headers, "", None) {
        return early;
    }
    let query: HashMap<String, String> = uri
        .query()
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let path = uri.path().trim_matches('/').to_string();
    let parts: Vec<&str> = path.split('/').collect();
    let repo = (parts.len() >= 3)
        .then(|| {
            world
                .repos
                .get(&format!("{}/{}", parts[1], parts[2]))
                .cloned()
        })
        .flatten();
    let not_found = || reply(404, json!({"message": "Not Found"}));
    match parts.as_slice() {
        ["repos", _, _] => repo.map_or_else(not_found, |repo| reply(200, repo_json(&repo))),
        ["repos", _, _, "collaborators", login, "permission"] => {
            repo.map_or_else(not_found, |repo| {
                let permission = repo
                    .collaborators
                    .get(*login)
                    .map_or("none", String::as_str);
                reply(
                    200,
                    json!({"permission": permission, "role_name": permission}),
                )
            })
        }
        ["repos", _, _, "pulls", number, "comments"] => {
            let (Some(repo), Ok(number)) = (repo, number.parse::<u64>()) else {
                return not_found();
            };
            let page: usize = query.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
            let per_page: usize = query
                .get("per_page")
                .and_then(|p| p.parse().ok())
                .unwrap_or(30);
            let all: Vec<Value> = repo.prs.get(&number).map_or_else(Vec::new, |pr| {
                pr.threads
                    .iter()
                    .flat_map(|t| t.comments.iter().map(move |c| comment_json(number, t, c)))
                    .collect()
            });
            let slice: Vec<Value> = all
                .into_iter()
                .skip(page.saturating_sub(1) * per_page)
                .take(per_page)
                .collect();
            reply(200, Value::Array(slice))
        }
        ["repos", _, _, "actions", "jobs", job, "logs"] => {
            let mut response = StatusCode::FOUND.into_response();
            let location = format!("/blob/{job}");
            response.headers_mut().insert(
                "location",
                HeaderValue::from_str(&location).expect("header"),
            );
            response
        }
        _ => not_found(),
    }
}

fn rollup(pr: &Pr) -> Value {
    let nodes: Vec<Value> = pr
        .checks
        .iter()
        .map(|(name, status, conclusion)| {
            json!({"__typename": "CheckRun", "name": name, "status": status,
                   "conclusion": conclusion,
                   "detailsUrl": format!("https://github.com/x/y/actions/runs/1/job/{}", name.len())})
        })
        .collect();
    json!({"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": nodes}}}}]})
}

fn parse_node_id(id: &str) -> Option<(String, u64)> {
    let (repo, number) = id.strip_prefix("PR_")?.rsplit_once('#')?;
    Some((repo.to_string(), number.parse().ok()?))
}

async fn graphql(
    State(world): State<Shared>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let text = String::from_utf8_lossy(&body).into_owned();
    let request: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let operation = request["operationName"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut world = world.lock().expect("fake world lock");
    if let Some(early) = admit(
        &mut world,
        "POST",
        &uri,
        &headers,
        &text,
        Some(operation.clone()),
    ) {
        return early;
    }
    let vars = request["variables"].clone();
    let (cost, remaining, reset_in) = world.graphql_cost;
    let key = format!(
        "{}/{}",
        vars["owner"].as_str().unwrap_or_default(),
        vars["name"].as_str().unwrap_or_default()
    );
    let number = vars["number"].as_u64().unwrap_or_default();
    let ok = |mut data: Value| {
        data["rateLimit"] = json!({"cost": cost, "remaining": remaining});
        let mut response = reply(200, json!({"data": data}));
        let headers = response.headers_mut();
        headers.insert(
            "x-ratelimit-remaining",
            HeaderValue::from(u64::from(remaining)),
        );
        headers.insert("x-ratelimit-reset", HeaderValue::from(epoch_in(reset_in)));
        response
    };
    let error = |message: &str| reply(200, json!({"errors": [{"message": message}]}));
    match operation.as_str() {
        "PrStatus" => {
            let Some(pr) = world.repos.get(&key).and_then(|r| r.prs.get(&number)) else {
                return ok(json!({"repository": {"pullRequest": null}}));
            };
            ok(json!({"repository": {"pullRequest": {
                "id": format!("PR_{key}#{number}"), "number": number, "title": pr.title,
                "url": format!("https://github.com/{key}/pull/{number}"), "state": pr.state,
                "isDraft": pr.draft, "headRefName": pr.head_ref, "headRefOid": pr.head_sha,
                "reviewDecision": pr.review_decision, "mergeable": pr.mergeable,
                "mergeStateStatus": "CLEAN",
                "autoMergeRequest": pr.auto_merge.as_ref().map(|(m, _)| json!({"mergeMethod": m})),
                "commits": rollup(pr)}}}))
        }
        "Viewer" => ok(json!({"viewer": {"login": "me"}})),
        "SearchPrs" => {
            // `repo:o/r is:pr is:open author:login`, matched on the server like GitHub's search.
            let query = vars["q"].as_str().unwrap_or_default();
            let term = |prefix: &str| {
                query
                    .split_whitespace()
                    .find_map(|word| word.strip_prefix(prefix))
                    .unwrap_or_default()
                    .to_string()
            };
            let (repo, author) = (term("repo:"), term("author:"));
            let nodes: Vec<Value> = world.repos.get(&repo).map_or_else(Vec::new, |repo| {
                repo.prs
                    .iter()
                    .filter(|(_, pr)| pr.state == "OPEN" && pr.author == author)
                    .take(100)
                    .map(|(n, pr)| {
                        json!({"number": n, "headRefName": pr.head_ref,
                               "headRefOid": pr.head_sha, "commits": rollup(pr)})
                    })
                    .collect()
            });
            ok(json!({"search": {"nodes": nodes}}))
        }
        "ReviewThreads" => {
            let Some(pr) = world.repos.get(&key).and_then(|r| r.prs.get(&number)) else {
                return ok(json!({"repository": {"pullRequest": null}}));
            };
            let threads: Vec<Value> = pr
                .threads
                .iter()
                .map(|t| {
                    let comments: Vec<Value> = t
                        .comments
                        .iter()
                        .map(|c| {
                            json!({"databaseId": c.id, "author": {"login": c.login},
                                   "authorAssociation": c.association, "body": c.body,
                                   "createdAt": "2026-01-01T00:00:00Z",
                                   "url": format!("https://github.com/{key}/pull/{number}#discussion_r{}", c.id),
                                   "replyTo": c.reply_to.map(|id| json!({"databaseId": id}))})
                        })
                        .collect();
                    json!({"id": t.id, "isResolved": t.resolved, "isOutdated": false,
                           "path": t.path, "comments": {"nodes": comments}})
                })
                .collect();
            ok(json!({"repository": {"pullRequest": {"reviewThreads": {"nodes": threads}}}}))
        }
        "ReplyToThread" => {
            let thread_id = vars["thread"].as_str().unwrap_or_default().to_string();
            world.next_id += 1;
            let id = world.next_id;
            let comment = Comment {
                id,
                login: "me".to_string(),
                association: "OWNER".to_string(),
                body: vars["body"].as_str().unwrap_or_default().to_string(),
                reply_to: None,
            };
            let found = world
                .repos
                .values_mut()
                .flat_map(|r| r.prs.values_mut())
                .flat_map(|pr| pr.threads.iter_mut())
                .find(|t| t.id == thread_id);
            let Some(thread) = found else {
                return error("Could not resolve to a node");
            };
            thread.comments.push(comment);
            world.mutations.push((operation.clone(), vars));
            ok(
                json!({"addPullRequestReviewThreadReply": {"comment": {"databaseId": id,
                "url": format!("https://github.com/x/y/pull/1#discussion_r{id}")}}}),
            )
        }
        "EnableAutoMerge" | "DisableAutoMerge" => {
            let Some((repo, number)) = vars["pr"].as_str().and_then(parse_node_id) else {
                return error("Could not resolve to a node");
            };
            let enable = operation == "EnableAutoMerge";
            let method = vars["method"].as_str().unwrap_or_default().to_string();
            let oid = vars["oid"].as_str().unwrap_or_default().to_string();
            let Some(pr) = world
                .repos
                .get_mut(&repo)
                .and_then(|r| r.prs.get_mut(&number))
            else {
                return error("Could not resolve to a node");
            };
            if enable {
                if pr.draft {
                    return error("Pull request is in draft state");
                }
                if pr.head_sha != oid {
                    return error("Head branch was modified. Review and try the merge again.");
                }
                pr.auto_merge = Some((method, oid));
            } else {
                pr.auto_merge = None;
            }
            world.mutations.push((operation.clone(), vars));
            ok(json!({"pullRequest": {"number": number}}))
        }
        _ => error("unknown operation"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The fake is dev-only: a manifest may name it only under `[dev-dependencies]`, so no
    /// shipped crate or bundle can link it.
    #[test]
    fn only_dev_dependencies_name_the_fake() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut manifests: Vec<_> = std::fs::read_dir(root.join("crates"))
            .unwrap()
            .map(|entry| entry.unwrap().path().join("Cargo.toml"))
            .collect();
        manifests.push(root.join("apps/desktop/src-tauri/Cargo.toml"));
        manifests.push(root.join("Cargo.toml"));
        for manifest in manifests.iter().filter(|path| path.exists()) {
            let text = std::fs::read_to_string(manifest).unwrap();
            let mut section = String::new();
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    section = line.to_string();
                } else if line.contains("test-host") && !line.starts_with('#') {
                    let client = manifest.ends_with("crates/imperium-github/Cargo.toml");
                    let core = manifest.ends_with("crates/imperium-core/Cargo.toml");
                    let allowed = (client
                        && ((line == "test-host = []" && section == "[features]")
                            || (section == "[dev-dependencies]"
                                && line.starts_with("imperium-github ="))))
                        // The daemon's tests run against the fake: this one dev-dependency line.
                        || (core
                            && section == "[dev-dependencies]"
                            && line
                                == "imperium-github = { path = \"../imperium-github\", features = [\"test-host\"] }");
                    assert!(
                        allowed,
                        "{} names test-host in `{line}` under {section}",
                        manifest.display()
                    );
                } else if line.contains("imperium-github-fake") && !line.starts_with('#') {
                    let own = text.contains("name = \"imperium-github-fake\"");
                    assert!(
                        own || section == "[dev-dependencies]",
                        "{} names the fake under {section}",
                        manifest.display()
                    );
                }
            }
        }
    }
}
