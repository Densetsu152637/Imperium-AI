//! Pull requests bound to sessions. The monitor keeps one cached [`PrStatus`] per distinct pull
//! request (sessions on the same pull request share one poll), refreshes it from GitHub, and
//! tells every client when it changes (`pull_requests.changed`). The card, the CLI and the
//! phone read only this cache; no client polls GitHub.
//!
//! A session shows `watching` while its pull request is open and `merged` once it merges, and
//! is archived when the pull request closes (`sessions.auto_archive_on_pr_close`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use imperium_engine::Session;
use imperium_github::{Check, GitHub, MergeSettings};
use imperium_protocol::methods::{
    PULL_REQUESTS_CHANGED, PrAutoMerge, PrChecks, PrConflictState, PrMergeMethod, PrReview,
    PrState, PrStatus, PullRequestsChangedParams,
};
use imperium_protocol::session::SessionStatus;
use tokio::sync::Notify;

use crate::DaemonState;

/// How long a repository's allowed merge methods are kept before they are read again.
const SETTINGS_TTL: Duration = Duration::from_secs(300);
/// The longest wait between two polls of a pull request that keeps failing.
const MAX_BACKOFF: Duration = Duration::from_secs(900);
/// The longest poll interval while a client shows the card.
const SUBSCRIBED_INTERVAL: Duration = Duration::from_secs(60);
const TITLE_CHARS: usize = 200;
const NAME_CHARS: usize = 100;
const NAMES_SHOWN: usize = 20;
const ERROR_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    /// Lower case: GitHub names are case-insensitive.
    repo: String,
    number: u64,
}

#[derive(Debug)]
struct Tracked {
    /// The repository as it was bound (`owner/name`).
    repo: String,
    number: u64,
    /// The last good status, with `error` set while the latest poll fails.
    status: Option<PrStatus>,
    /// The latest poll's error, also when there is no good status yet.
    last_error: Option<String>,
    failures: u32,
    next_due: Instant,
}

#[derive(Debug, Default)]
struct Inner {
    sessions: HashMap<String, Key>,
    tracked: HashMap<Key, Tracked>,
    settings: HashMap<String, (Instant, Result<MergeSettings, String>)>,
}

/// The daemon's pull-request cache and poller.
#[derive(Debug)]
pub struct PrMonitor {
    inner: Mutex<Inner>,
    github: OnceLock<Arc<dyn GitHub>>,
    base: Mutex<Duration>,
    started: AtomicBool,
    subscribers: AtomicUsize,
    kick: Notify,
}

/// Held by a connection that shows pull requests; the poller runs faster meanwhile.
#[derive(Debug)]
pub(crate) struct Watch(Arc<PrMonitor>);

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.subscribers.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Default for PrMonitor {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            github: OnceLock::new(),
            base: Mutex::new(Duration::from_secs(120)),
            started: AtomicBool::new(false),
            subscribers: AtomicUsize::new(0),
            kick: Notify::new(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl PrMonitor {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the poller with `base` as the idle interval. A second call changes only the
    /// interval (the first call's source stays).
    pub fn start(
        self: &Arc<Self>,
        daemon: &Arc<DaemonState>,
        github: Arc<dyn GitHub>,
        base: Duration,
    ) {
        let _ = self.github.set(github);
        *self.base.lock().unwrap_or_else(PoisonError::into_inner) = base;
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let monitor = Arc::clone(self);
        let daemon = Arc::downgrade(daemon);
        tokio::spawn(async move {
            loop {
                let Some(state) = daemon.upgrade() else {
                    return;
                };
                let wait = monitor.poll(&state, false).await;
                drop(state);
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = monitor.kick.notified() => {}
                }
            }
        });
    }

    /// Sets the GitHub source without starting the poller, so a test drives [`Self::poll`]
    /// itself.
    #[cfg(feature = "testing")]
    pub fn attach(&self, github: Arc<dyn GitHub>) {
        let _ = self.github.set(github);
    }

    /// A connection that shows pull requests holds this while it does.
    pub(crate) fn watch(self: &Arc<Self>) -> Watch {
        self.subscribers.fetch_add(1, Ordering::SeqCst);
        Watch(Arc::clone(self))
    }

    fn interval(&self) -> Duration {
        let base = *self.base.lock().unwrap_or_else(PoisonError::into_inner);
        if self.subscribers.load(Ordering::SeqCst) > 0 {
            base.min(SUBSCRIBED_INTERVAL)
        } else {
            base
        }
    }

    /// The pull requests still worth watching, as `(session id, repo, number)`: bound and not
    /// yet seen merged or closed. They keep a daemon that nobody else uses from exiting.
    pub(crate) fn watched(&self) -> Vec<(String, String, u64)> {
        let inner = self.lock();
        inner
            .sessions
            .iter()
            .filter_map(|(id, key)| {
                let tracked = inner.tracked.get(key)?;
                let over = tracked
                    .status
                    .as_ref()
                    .is_some_and(|s| matches!(s.state, PrState::Merged | PrState::Closed));
                (!over).then(|| (id.clone(), tracked.repo.clone(), tracked.number))
            })
            .collect()
    }

    /// The repositories some session has a pull request bound from.
    pub(crate) fn bound_repos(&self) -> Vec<String> {
        self.lock()
            .tracked
            .values()
            .map(|tracked| tracked.repo.clone())
            .collect()
    }

    /// Binds `session` to a pull request, replacing any it had, and polls soon. The status is
    /// filled in by the poller (or [`Self::refresh`]); a pull request another session already
    /// follows is announced to this session at once.
    pub(crate) fn bind(&self, state: &DaemonState, session: &Session, repo: &str, number: u64) {
        let key = Key {
            repo: repo.to_ascii_lowercase(),
            number,
        };
        let cached = {
            let mut inner = self.lock();
            let replaced = inner.sessions.insert(session.id().to_string(), key.clone());
            inner.tracked.entry(key.clone()).or_insert_with(|| Tracked {
                repo: repo.to_string(),
                number,
                status: None,
                last_error: None,
                failures: 0,
                next_due: Instant::now(),
            });
            if let Some(old) = replaced.filter(|old| *old != key) {
                Self::drop_if_unused(&mut inner, &old);
            }
            inner.tracked.get(&key).and_then(|t| t.status.clone())
        };
        match cached {
            Some(status) => {
                apply_overlay(session, &status);
                state.announce(
                    PULL_REQUESTS_CHANGED,
                    PullRequestsChangedParams {
                        session_id: session.id().to_string(),
                        status: Some(status),
                    },
                );
            }
            None => session.set_watch(Some((SessionStatus::Watching, format!("{repo}#{number}")))),
        }
        self.kick.notify_one();
    }

    fn drop_if_unused(inner: &mut Inner, key: &Key) {
        if !inner.sessions.values().any(|bound| bound == key) {
            inner.tracked.remove(key);
        }
    }

    /// Ends a session's binding, switches its watch overlay off, and tells clients. False when
    /// it had none.
    pub(crate) fn unbind(&self, state: &DaemonState, session: &Session) -> bool {
        let was_bound = self.forget(session.id());
        session.set_watch(None);
        if was_bound {
            state.announce(
                PULL_REQUESTS_CHANGED,
                PullRequestsChangedParams {
                    session_id: session.id().to_string(),
                    status: None,
                },
            );
        }
        was_bound
    }

    /// Drops a session's binding without telling anyone (the session itself is gone).
    pub(crate) fn forget(&self, session_id: &str) -> bool {
        let mut inner = self.lock();
        let Some(key) = inner.sessions.remove(session_id) else {
            return false;
        };
        Self::drop_if_unused(&mut inner, &key);
        true
    }

    /// The cached status of a session's pull request.
    pub(crate) fn status(&self, session_id: &str) -> Result<PrStatus, String> {
        const UNBOUND: &str = "no pull request is bound to this session";
        let inner = self.lock();
        let key = inner.sessions.get(session_id).ok_or(UNBOUND)?;
        let tracked = inner.tracked.get(key).ok_or(UNBOUND)?;
        tracked.status.clone().ok_or_else(|| {
            tracked.last_error.as_ref().map_or_else(
                || "the pull request has not been read yet".to_string(),
                |error| format!("the pull request could not be read: {error}"),
            )
        })
    }

    /// Reads a session's pull request from GitHub now and updates the cache.
    pub(crate) async fn refresh(
        &self,
        state: &Arc<DaemonState>,
        session_id: &str,
    ) -> Result<PrStatus, String> {
        let key = self
            .lock()
            .sessions
            .get(session_id)
            .cloned()
            .ok_or("no pull request is bound to this session")?;
        self.read(state, &key).await
    }

    /// One poll pass: reads each pull request that is due (all of them when `force`), and
    /// returns how long until the next one is due.
    pub async fn poll(&self, state: &Arc<DaemonState>, force: bool) -> Duration {
        self.prune(state);
        let now = Instant::now();
        let due: Vec<Key> = self
            .lock()
            .tracked
            .iter()
            .filter(|(_, tracked)| force || tracked.next_due <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in due {
            let _ = self.read(state, &key).await;
        }
        let interval = self.interval();
        let now = Instant::now();
        self.lock()
            .tracked
            .values()
            .map(|tracked| tracked.next_due.saturating_duration_since(now))
            .min()
            .unwrap_or(interval)
            .min(interval)
            .max(Duration::from_millis(10))
    }

    /// Forgets bindings of sessions that no longer exist.
    fn prune(&self, state: &DaemonState) {
        let ids: Vec<String> = self.lock().sessions.keys().cloned().collect();
        for id in ids {
            if state.session(&id).is_none() {
                self.forget(&id);
            }
        }
    }

    /// Reads one pull request, stores the outcome, and tells the bound sessions' clients when
    /// the status (or its error) changed.
    async fn read(&self, state: &Arc<DaemonState>, key: &Key) -> Result<PrStatus, String> {
        let Some(github) = self.github.get().cloned() else {
            return Err("GitHub is not available".to_string());
        };
        let (repo, number) = {
            let inner = self.lock();
            let tracked = inner
                .tracked
                .get(key)
                .ok_or("the pull request is no longer bound")?;
            (tracked.repo.clone(), tracked.number)
        };
        let archive = state.settings().sessions.auto_archive_on_pr_close;
        let result = async {
            let pr = github.pr_status(&repo, number).await?;
            let settings = if pr.state == "OPEN" {
                Some(self.merge_settings(&github, &repo).await)
            } else {
                None
            };
            Ok::<_, String>(build(&repo, number, &pr, settings.as_ref(), archive))
        }
        .await;
        let interval = self.interval();
        let (outcome, changed, sessions) = {
            let mut inner = self.lock();
            let sessions: Vec<String> = inner
                .sessions
                .iter()
                .filter(|(_, bound)| *bound == key)
                .map(|(id, _)| id.clone())
                .collect();
            let Some(tracked) = inner.tracked.get_mut(key) else {
                return result;
            };
            match result {
                Ok(mut fresh) => {
                    tracked.failures = 0;
                    tracked.last_error = None;
                    tracked.next_due = Instant::now() + interval;
                    let changed = match &tracked.status {
                        Some(old) => {
                            fresh.updated_at = old.updated_at;
                            fresh != *old
                        }
                        None => true,
                    };
                    if changed {
                        fresh.updated_at = now_ms();
                        tracked.status = Some(fresh.clone());
                    }
                    (Ok(fresh), changed, sessions)
                }
                Err(error) => {
                    let error: String = error.chars().take(ERROR_CHARS).collect();
                    tracked.failures += 1;
                    tracked.last_error = Some(error.clone());
                    tracked.next_due = Instant::now() + backoff(tracked.failures, interval, &error);
                    let mut changed = false;
                    if let Some(status) = &mut tracked.status
                        && status.error.as_deref() != Some(error.as_str())
                    {
                        status.error = Some(error.clone());
                        status.updated_at = now_ms();
                        changed = true;
                    }
                    (Err(error), changed, sessions)
                }
            }
        };
        if changed {
            let status = self.lock().tracked.get(key).and_then(|t| t.status.clone());
            for id in &sessions {
                if let (Some(session), Some(status)) = (state.session(id), &status) {
                    apply_overlay(&session, status);
                }
                state.announce(
                    PULL_REQUESTS_CHANGED,
                    PullRequestsChangedParams {
                        session_id: id.clone(),
                        status: status.clone(),
                    },
                );
            }
        }
        // A merged or closed pull request archives its session once the session may be
        // archived, which can be some polls after the merge.
        if let Ok(status) = &outcome
            && matches!(status.state, PrState::Merged | PrState::Closed)
        {
            for id in &sessions {
                if let Some(session) = state.session(id) {
                    state.archive_after_pull_request(&session);
                }
            }
        }
        outcome
    }

    /// The repository's allowed merge methods, read at most every five minutes (an error is
    /// kept as long, so a token without push access does not retry on every poll).
    async fn merge_settings(
        &self,
        github: &Arc<dyn GitHub>,
        repo: &str,
    ) -> Result<MergeSettings, String> {
        let key = repo.to_ascii_lowercase();
        if let Some((at, result)) = self.lock().settings.get(&key)
            && at.elapsed() < SETTINGS_TTL
        {
            return result.clone();
        }
        let result = github.repo_merge_settings(repo).await;
        self.lock()
            .settings
            .insert(key, (Instant::now(), result.clone()));
        result
    }
}

/// How long to wait after the `failures`-th failed poll in a row: the interval doubled each
/// time up to 15 minutes, and at least as long as GitHub said a rate limit lasts.
fn backoff(failures: u32, interval: Duration, error: &str) -> Duration {
    let mut delay = interval
        .saturating_mul(1 << failures.min(5))
        .min(MAX_BACKOFF);
    if let Some(wait) = error
        .split_once("allowed in ")
        .and_then(|(_, rest)| rest.split_once('s'))
        .and_then(|(seconds, _)| seconds.trim().parse::<u64>().ok())
    {
        delay = delay.max(Duration::from_secs(wait).min(MAX_BACKOFF));
    }
    delay
}

/// What a session shows for its pull request's state.
fn apply_overlay(session: &Session, status: &PrStatus) {
    let detail = format!("{}#{}", status.repo, status.number);
    session.set_watch(match status.state {
        PrState::Open | PrState::Draft => Some((SessionStatus::Watching, detail)),
        PrState::Merged => Some((SessionStatus::Merged, detail)),
        PrState::Closed => None,
    });
}

/// Text from GitHub, safe to show: no control or direction-changing characters, and capped.
fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200B}'..='\u{200F}'
                        | '\u{202A}'..='\u{202E}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{FEFF}'
                )
        })
        .take(max)
        .collect()
}

fn method(method: imperium_github::MergeMethod) -> PrMergeMethod {
    match method {
        imperium_github::MergeMethod::Squash => PrMergeMethod::Squash,
        imperium_github::MergeMethod::Merge => PrMergeMethod::Merge,
        imperium_github::MergeMethod::Rebase => PrMergeMethod::Rebase,
    }
}

fn count_checks(checks: &[Check]) -> PrChecks {
    let mut counts = PrChecks {
        passed: 0,
        failed: 0,
        pending: 0,
        failing_names: Vec::new(),
    };
    for check in checks {
        let failed = check.is_failed()
            || matches!(
                check.conclusion.as_deref(),
                Some("CANCELLED" | "ACTION_REQUIRED" | "STALE")
            );
        if failed {
            counts.failed += 1;
            if counts.failing_names.len() < NAMES_SHOWN {
                counts.failing_names.push(clean(&check.name, NAME_CHARS));
            }
        } else if check.status == "COMPLETED"
            && matches!(
                check.conclusion.as_deref(),
                Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
            )
        {
            counts.passed += 1;
        } else {
            counts.pending += 1;
        }
    }
    counts
}

/// The wire status for what GitHub reported. `settings` is `None` for a pull request that is
/// not open (its merge methods do not matter).
fn build(
    repo: &str,
    number: u64,
    pr: &imperium_github::PrStatus,
    settings: Option<&Result<MergeSettings, String>>,
    archive_on_close: bool,
) -> PrStatus {
    let state = match (pr.state.as_str(), pr.is_draft) {
        ("MERGED", _) => PrState::Merged,
        ("CLOSED", _) => PrState::Closed,
        (_, true) => PrState::Draft,
        _ => PrState::Open,
    };
    let allowed: Vec<PrMergeMethod> = match settings {
        Some(Ok(settings)) => settings.allowed().into_iter().map(method).collect(),
        _ => Vec::new(),
    };
    let reason = match (state, settings) {
        (PrState::Merged, _) => Some("the pull request is merged"),
        (PrState::Closed, _) => Some("the pull request is closed"),
        (PrState::Draft, _) => {
            Some("GitHub refuses auto-merge on a draft; mark it ready for review")
        }
        _ if pr.is_in_merge_queue => Some("the branch uses a merge queue"),
        (_, Some(Err(_))) => Some("this token cannot read the repository's merge settings"),
        (_, Some(Ok(s))) if !s.auto_merge => Some("the repository does not allow auto-merge"),
        (_, Some(Ok(_))) if allowed.is_empty() => Some("the repository allows no merge method"),
        _ => None,
    };
    PrStatus {
        url: format!("https://github.com/{repo}/pull/{number}"),
        repo: repo.to_string(),
        number,
        title: clean(&pr.title, TITLE_CHARS),
        state,
        draft: pr.is_draft,
        head_sha: clean(&pr.head_sha, 64),
        base: clean(&pr.base_ref, NAME_CHARS),
        branch: clean(&pr.head_ref, NAME_CHARS),
        checks: count_checks(&pr.checks),
        review: match pr.review_decision.as_deref() {
            Some("APPROVED") => PrReview::Approved,
            Some("CHANGES_REQUESTED") => PrReview::ChangesRequested,
            Some("REVIEW_REQUIRED") => PrReview::ReviewRequired,
            _ => PrReview::None,
        },
        mergeable: match pr.mergeable.as_str() {
            "MERGEABLE" => PrConflictState::Mergeable,
            "CONFLICTING" => PrConflictState::Conflicting,
            _ => PrConflictState::Unknown,
        },
        merge_state: if pr.is_in_merge_queue {
            "queued".to_string()
        } else {
            clean(&pr.merge_state.to_ascii_lowercase(), 32)
        },
        allowed_methods: allowed,
        auto_fix: false,
        auto_merge: PrAutoMerge {
            enabled: pr.auto_merge.is_some(),
            method: pr.auto_merge.map(method),
            reason_unavailable: reason.map(str::to_string),
        },
        auto_archive_on_close: archive_on_close,
        untrusted_handled: false,
        updated_at: now_ms(),
        error: None,
    }
}

/// The repository and number in `https://github.com/<owner>/<repo>/pull/<n>`; the page a user
/// copies may continue with `/files`, `/commits` or `/checks`, a query or a fragment.
pub(crate) fn parse_pr_url(url: &str) -> Result<(String, u64), String> {
    let bad = || {
        "a pull request URL looks like https://github.com/<owner>/<repo>/pull/<number>".to_string()
    };
    let rest = url
        .trim()
        .strip_prefix("https://github.com/")
        .ok_or_else(bad)?;
    let rest = rest.split(['#', '?']).next().unwrap_or_default();
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    let (owner, name, number) = match parts.as_slice() {
        [owner, name, "pull", number] => (owner, name, number),
        [owner, name, "pull", number, "files" | "commits" | "checks"] => (owner, name, number),
        _ => return Err(bad()),
    };
    let valid = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let number: u64 = number.parse().map_err(|_| bad())?;
    if !valid(owner) || !valid(name) || number == 0 {
        return Err(bad());
    }
    Ok((format!("{owner}/{name}"), number))
}

/// `owner/name` of a git remote URL on github.com (`https://`, `ssh://` or scp-style).
fn github_repo(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = ["https://", "http://", "ssh://", "git://"]
        .iter()
        .find_map(|scheme| remote.strip_prefix(scheme))
        .and_then(|rest| {
            let (host, path) = rest.split_once('/')?;
            let host = host.rsplit('@').next()?;
            let host = host.split(':').next()?;
            host.eq_ignore_ascii_case("github.com").then_some(path)
        })
        .or_else(|| {
            let (host, path) = remote.split_once(':')?;
            let host = host.rsplit('@').next()?;
            host.eq_ignore_ascii_case("github.com").then_some(path)
        })?;
    let path = path.trim_matches('/').trim_end_matches(".git");
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{owner}/{name}"))
}

/// The GitHub repository a folder's `origin` remote names, read from the folder's git config
/// without running anything its configuration names.
fn origin_repo(folder: &Path) -> Option<String> {
    let mut command = crate::safe_git::command(folder);
    command.args(["config", "--get", "remote.origin.url"]);
    let output = crate::safe_git::run(&mut command).ok()?;
    github_repo(&crate::safe_git::text(&output))
}

/// Refuses a repository that is neither the session's `origin` nor one the user's sessions
/// already have a pull request bound from.
pub(crate) fn check_repo(state: &DaemonState, session: &Session, repo: &str) -> Result<(), String> {
    let workspace = session.workspace();
    let mut folders = workspace.folders_display();
    folders.push(workspace.root().display().to_string());
    if folders
        .iter()
        .filter_map(|folder| origin_repo(Path::new(folder)))
        .any(|origin| origin.eq_ignore_ascii_case(repo))
        || state
            .pr_monitor()
            .bound_repos()
            .iter()
            .any(|bound| bound.eq_ignore_ascii_case(repo))
    {
        return Ok(());
    }
    Err(format!(
        "{repo} is not this session's origin repository, and none of your sessions has a pull \
         request bound from it"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_urls_are_read_strictly() {
        let ok = |url: &str| parse_pr_url(url).unwrap();
        assert_eq!(ok("https://github.com/o/r/pull/12"), ("o/r".into(), 12));
        assert_eq!(
            ok("https://github.com/o/r/pull/12/files#x"),
            ("o/r".into(), 12)
        );
        assert_eq!(
            ok(" https://github.com/o/r.x/pull/3/?a=b "),
            ("o/r.x".into(), 3)
        );
        for bad in [
            "http://github.com/o/r/pull/1",
            "https://github.com.evil.com/o/r/pull/1",
            "https://github.com/o/r/issues/1",
            "https://github.com/o/r/pull/0",
            "https://github.com/o/r/pull/x",
            "https://github.com/o/r/pull/1/../../x",
            "https://github.com/o%2f/r/pull/1",
            "https://github.com//r/pull/1",
            "o/r#1",
        ] {
            assert!(parse_pr_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn remotes_name_a_github_repository() {
        for (remote, repo) in [
            ("https://github.com/o/r.git", Some("o/r")),
            ("https://user@github.com/o/r", Some("o/r")),
            ("git@github.com:o/r.git", Some("o/r")),
            ("ssh://git@github.com/o/r.git", Some("o/r")),
            ("git@gitlab.com:o/r.git", None),
            ("https://github.com.evil.com/o/r", None),
            ("/some/folder", None),
        ] {
            assert_eq!(github_repo(remote).as_deref(), repo, "{remote}");
        }
    }

    #[test]
    fn failed_polls_back_off_and_respect_a_rate_limit() {
        let interval = Duration::from_secs(60);
        assert_eq!(
            backoff(1, interval, "GitHub returned 502"),
            Duration::from_secs(120)
        );
        assert_eq!(backoff(3, interval, "x"), Duration::from_secs(480));
        assert_eq!(backoff(30, interval, "x"), MAX_BACKOFF);
        let limited = "GitHub rate limit: the next call is allowed in 601s";
        assert_eq!(backoff(1, interval, limited), Duration::from_secs(601));
        assert_eq!(
            backoff(1, interval, "the next call is allowed in 99999s"),
            MAX_BACKOFF
        );
    }

    #[test]
    fn checks_are_counted_and_names_are_cleaned() {
        let check = |name: &str, status: &str, conclusion: Option<&str>| Check {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            details_url: String::new(),
        };
        let counts = count_checks(&[
            check("a", "COMPLETED", Some("SUCCESS")),
            check("b\u{202e}\nevil", "COMPLETED", Some("FAILURE")),
            check("c", "IN_PROGRESS", None),
            check("d", "COMPLETED", Some("CANCELLED")),
        ]);
        assert_eq!((counts.passed, counts.failed, counts.pending), (1, 2, 1));
        assert_eq!(counts.failing_names, ["bevil", "d"]);
    }
}
