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
use imperium_github::{AutoMerge, Check, GitHub, MergeSettings};
use imperium_protocol::methods::{
    PULL_REQUESTS_CHANGED, PrAutoMerge, PrChecks, PrConflictState, PrMergeMethod, PrReview,
    PrState, PrStatus, PullRequestsChangedParams, SESSION_UPDATED,
};
use imperium_protocol::session::SessionStatus;
use serde_json::Value;
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
const MOVED_NOTICE: &str =
    "auto-merge was turned off because new commits were pushed after you enabled it";

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
    /// When the status was last read successfully.
    read_at: Option<Instant>,
    /// The head commit the user confirmed when the desktop app turned auto-merge on through
    /// Imperium. Kept with the binding across restarts. While auto-merge is on and the head
    /// differs, the poll turns auto-merge off. `None` when Imperium did not enable it (it was
    /// enabled elsewhere, or before this was recorded): then the poll leaves it alone.
    confirmed_head: Option<String>,
}

/// A session's binding: its pull request and its own switches (stage 7 acts on them).
#[derive(Debug, Clone)]
struct Binding {
    key: Key,
    auto_fix: bool,
    auto_archive_on_close: bool,
}

#[derive(Debug, Default)]
struct Inner {
    sessions: HashMap<String, Binding>,
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
            .filter_map(|(id, binding)| {
                let tracked = inner.tracked.get(&binding.key)?;
                let over = tracked
                    .status
                    .as_ref()
                    .is_some_and(|s| matches!(s.state, PrState::Merged | PrState::Closed));
                (!over).then(|| (id.clone(), tracked.repo.clone(), tracked.number))
            })
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
        let archive = state.settings().sessions.auto_archive_on_pr_close;
        let (cached, moved, orphan) = {
            let mut inner = self.lock();
            let previous = inner.sessions.get(session.id()).cloned();
            let moved = previous.as_ref().is_none_or(|old| old.key != key);
            // The same pull request again keeps the switches the session has.
            let binding = match previous.as_ref().filter(|old| old.key == key) {
                Some(old) => old.clone(),
                None => Binding {
                    key: key.clone(),
                    auto_fix: false,
                    auto_archive_on_close: archive,
                },
            };
            inner.sessions.insert(session.id().to_string(), binding);
            Self::track(&mut inner, repo, number, None, None);
            let orphan = previous
                .filter(|old| old.key != key)
                .and_then(|old| Self::drop_if_unused(&mut inner, &old.key));
            (
                inner.tracked.get(&key).and_then(|t| t.status.clone()),
                moved,
                orphan,
            )
        };
        // The session left a pull request it had turned auto-merge on for.
        if let Some((old_repo, old_number)) = orphan {
            self.disown(&old_repo, old_number);
        }
        self.save(state, session.id());
        match cached {
            Some(status) => {
                apply_overlay(session, &status);
                state.announce(
                    PULL_REQUESTS_CHANGED,
                    PullRequestsChangedParams {
                        session_id: session.id().to_string(),
                        status: Some(self.personal(session.id(), status)),
                    },
                );
            }
            None => session.set_watch(Some((SessionStatus::Watching, format!("{repo}#{number}")))),
        }
        if moved {
            state.announce(SESSION_UPDATED, state.summary(session));
        }
        self.kick.notify_one();
    }

    /// The tracked entry for a pull request, made when nobody follows it yet. `saved` is a
    /// status read before a restart, kept only until the first poll replaces it.
    fn track(
        inner: &mut Inner,
        repo: &str,
        number: u64,
        saved: Option<PrStatus>,
        confirmed_head: Option<String>,
    ) {
        let key = Key {
            repo: repo.to_ascii_lowercase(),
            number,
        };
        let tracked = inner.tracked.entry(key).or_insert_with(|| Tracked {
            repo: repo.to_string(),
            number,
            status: None,
            last_error: None,
            failures: 0,
            next_due: Instant::now(),
            read_at: None,
            confirmed_head: None,
        });
        if tracked.status.is_none() {
            tracked.status = saved;
        }
        if tracked.confirmed_head.is_none() {
            tracked.confirmed_head = confirmed_head;
        }
    }

    /// `status` with this session's own switches.
    fn personal(&self, session_id: &str, mut status: PrStatus) -> PrStatus {
        if let Some(binding) = self.lock().sessions.get(session_id) {
            status.auto_fix = binding.auto_fix;
            status.auto_archive_on_close = binding.auto_archive_on_close;
        }
        status
    }

    /// Sets a session's per-binding switches and records them. They act from stage 7 on; until
    /// then they are only kept and shown. False when the session has no binding.
    pub fn set_switches(
        &self,
        state: &DaemonState,
        session_id: &str,
        auto_fix: Option<bool>,
        auto_archive_on_close: Option<bool>,
    ) -> bool {
        {
            let mut inner = self.lock();
            let Some(binding) = inner.sessions.get_mut(session_id) else {
                return false;
            };
            binding.auto_fix = auto_fix.unwrap_or(binding.auto_fix);
            binding.auto_archive_on_close =
                auto_archive_on_close.unwrap_or(binding.auto_archive_on_close);
        }
        self.save(state, session_id);
        if let Ok(status) = self.status(session_id) {
            state.announce(
                PULL_REQUESTS_CHANGED,
                PullRequestsChangedParams {
                    session_id: session_id.to_string(),
                    status: Some(status),
                },
            );
        }
        true
    }

    /// Writes a session's binding, switches and last status into its record, so a restart
    /// brings them back.
    fn save(&self, state: &DaemonState, session_id: &str) {
        let record = {
            let inner = self.lock();
            let Some(binding) = inner.sessions.get(session_id) else {
                return;
            };
            let tracked = inner.tracked.get(&binding.key);
            serde_json::json!({
                "url": tracked.map(|t| pr_url(&t.repo, t.number)),
                "auto_fix": binding.auto_fix,
                "auto_archive_on_close": binding.auto_archive_on_close,
                "status": tracked.and_then(|t| t.status.as_ref()),
                "auto_merge_head": tracked.and_then(|t| t.confirmed_head.as_ref()),
            })
        };
        state.record_pull_request(session_id, Some(record));
    }

    /// Takes a session's binding back after a restart from what [`Self::save`] wrote. Nothing
    /// is announced (the session is starting). The repository is checked against the session's
    /// `origin` again, so a binding always means the session's own repository even after the
    /// record was edited or the remote changed; a record that fails the check is dropped, with
    /// its `pull_request` key, and logged. A record that does not parse is ignored.
    pub(crate) fn restore(&self, state: &DaemonState, session: &Session, record: &Value) {
        let Some((repo, number)) = record["url"]
            .as_str()
            .and_then(|url| parse_pr_url(url).ok())
        else {
            return;
        };
        if let Err(reason) = check_repo(session, &repo) {
            tracing::warn!(session = %session.id(), %reason, "the saved pull request is not this session's own; it is unbound");
            state.record_pull_request(session.id(), None);
            return;
        }
        let saved = serde_json::from_value::<PrStatus>(record["status"].clone())
            .ok()
            .filter(|s| s.repo.eq_ignore_ascii_case(&repo) && s.number == number);
        {
            let mut inner = self.lock();
            inner.sessions.insert(
                session.id().to_string(),
                Binding {
                    key: Key {
                        repo: repo.to_ascii_lowercase(),
                        number,
                    },
                    auto_fix: record["auto_fix"].as_bool().unwrap_or(false),
                    auto_archive_on_close: record["auto_archive_on_close"]
                        .as_bool()
                        .unwrap_or(state.settings().sessions.auto_archive_on_pr_close),
                },
            );
            let confirmed = record["auto_merge_head"].as_str().map(str::to_string);
            Self::track(&mut inner, &repo, number, saved.clone(), confirmed);
        }
        match saved {
            Some(status) => apply_overlay(session, &status),
            None => session.set_watch(Some((SessionStatus::Watching, format!("{repo}#{number}")))),
        }
        self.kick.notify_one();
    }

    /// Binds the session that `session.review_pull_request` just opened `url` for. The pull
    /// request is the session's own, so it goes through the same origin check as
    /// `pull_requests.bind`.
    pub fn bind_opened(
        &self,
        state: &DaemonState,
        session_id: &str,
        url: &str,
    ) -> Result<(), String> {
        let session = state.session(session_id).ok_or("no such session")?;
        let (repo, number) = parse_pr_url(url)?;
        check_repo(&session, &repo)?;
        self.bind(state, &session, &repo, number);
        Ok(())
    }

    /// The link of the pull request a session is bound to.
    pub(crate) fn url_of(&self, session_id: &str) -> Option<String> {
        let inner = self.lock();
        let tracked = inner.tracked.get(&inner.sessions.get(session_id)?.key)?;
        Some(pr_url(&tracked.repo, tracked.number))
    }

    /// Drops the tracked entry nobody follows any more. When Imperium had turned auto-merge on
    /// for it (a head is recorded), the pull request is returned: nothing would watch its head
    /// from now on, so [`Self::disown`] turns auto-merge off.
    fn drop_if_unused(inner: &mut Inner, key: &Key) -> Option<(String, u64)> {
        if inner.sessions.values().any(|bound| bound.key == *key) {
            return None;
        }
        let tracked = inner.tracked.remove(key)?;
        tracked
            .confirmed_head
            .map(|_| (tracked.repo, tracked.number))
    }

    /// Turns auto-merge off for a pull request that is no longer followed (the session was
    /// deleted or archived, or moved to another pull request), without waiting: best effort,
    /// logged either way.
    fn disown(&self, repo: &str, number: u64) {
        let Some(github) = self.github.get().cloned() else {
            tracing::warn!(%repo, pr = number, "auto-merge stays on: this pull request is no longer followed and GitHub is not available");
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(%repo, pr = number, "auto-merge stays on: this pull request is no longer followed and nothing could turn it off");
            return;
        };
        let repo = repo.to_string();
        runtime.spawn(async move {
            match github.set_auto_merge(&repo, number, &AutoMerge::Disable).await {
                Ok(()) => tracing::info!(%repo, pr = number, "auto-merge was turned off: nothing follows this pull request any more"),
                Err(error) => tracing::warn!(%repo, pr = number, error = %clean(&error, ERROR_CHARS), "auto-merge stays on: this pull request is no longer followed and it could not be turned off"),
            }
        });
    }

    /// Before an unbind: when this session is the last to follow a pull request Imperium turned
    /// auto-merge on for, turns it off. An error means the unbind must not go ahead.
    pub(crate) async fn release(&self, session_id: &str) -> Result<(), String> {
        let key = {
            let inner = self.lock();
            let Some(binding) = inner.sessions.get(session_id) else {
                return Ok(());
            };
            let shared = inner
                .sessions
                .iter()
                .any(|(id, bound)| id != session_id && bound.key == binding.key);
            let confirmed = inner
                .tracked
                .get(&binding.key)
                .is_some_and(|t| t.confirmed_head.is_some());
            if shared || !confirmed {
                return Ok(());
            }
            binding.key.clone()
        };
        let (repo, number) = {
            let inner = self.lock();
            let tracked = inner.tracked.get(&key).ok_or("no pull request is bound")?;
            (tracked.repo.clone(), tracked.number)
        };
        let github = self.github.get().cloned().ok_or(
            "auto-merge is on and GitHub is not available to turn it off, so the pull request was not unbound",
        )?;
        github
            .set_auto_merge(&repo, number, &AutoMerge::Disable)
            .await
            .map_err(|error| {
                format!(
                    "auto-merge is on and could not be turned off, so the pull request was not unbound: {}",
                    clean(&error, ERROR_CHARS)
                )
            })?;
        let mut inner = self.lock();
        if let Some(tracked) = inner.tracked.get_mut(&key) {
            tracked.confirmed_head = None;
        }
        Ok(())
    }

    /// Ends a session's binding, switches its watch overlay off, and tells clients. False when
    /// it had none.
    pub(crate) fn unbind(&self, state: &DaemonState, session: &Session) -> bool {
        let was_bound = self.forget(session.id());
        session.set_watch(None);
        if was_bound {
            state.record_pull_request(session.id(), None);
            state.announce(SESSION_UPDATED, state.summary(session));
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
        let orphan = {
            let mut inner = self.lock();
            let Some(binding) = inner.sessions.remove(session_id) else {
                return false;
            };
            Self::drop_if_unused(&mut inner, &binding.key)
        };
        if let Some((repo, number)) = orphan {
            self.disown(&repo, number);
        }
        true
    }

    /// The cached status of a session's pull request.
    pub(crate) fn status(&self, session_id: &str) -> Result<PrStatus, String> {
        const UNBOUND: &str = "no pull request is bound to this session";
        let status = {
            let inner = self.lock();
            let key = &inner.sessions.get(session_id).ok_or(UNBOUND)?.key;
            let tracked = inner.tracked.get(key).ok_or(UNBOUND)?;
            tracked.status.clone().ok_or_else(|| {
                tracked.last_error.as_ref().map_or_else(
                    || "the pull request has not been read yet".to_string(),
                    |error| format!("the pull request could not be read: {error}"),
                )
            })?
        };
        Ok(self.personal(session_id, status))
    }

    /// The repository and number of the pull request `session_id` is bound to, when `url` is that
    /// pull request's address (the daemon's own form, as the card was shown).
    fn bound_target(&self, session_id: &str, url: &str) -> Result<(String, u64), String> {
        let inner = self.lock();
        let tracked = inner
            .sessions
            .get(session_id)
            .and_then(|binding| inner.tracked.get(&binding.key))
            .ok_or("no pull request is bound to this session")?;
        if pr_url(&tracked.repo, tracked.number) != url {
            return Err("that is not the pull request this session is bound to".into());
        }
        Ok((tracked.repo.clone(), tracked.number))
    }

    /// Asks GitHub to auto-merge the session's pull request with `method`, pinned to `head_sha`
    /// (GitHub refuses if the branch has moved since). The caller has already checked that this
    /// is the desktop app and that its challenge fits. The pull request is read from GitHub
    /// again first, never taken from the status cache or from the caller: the draft state, the
    /// head commit and auto-merge itself. The repository's merge settings and allowed methods
    /// come from their own cache (up to five minutes old). The only calls to GitHub that
    /// change anything are this mutation and [`Self::disable_auto_merge`]; the daemon never calls
    /// a merge endpoint.
    pub(crate) async fn enable_auto_merge(
        &self,
        state: &Arc<DaemonState>,
        session_id: &str,
        url: &str,
        head_sha: &str,
        chosen: PrMergeMethod,
    ) -> Result<PrStatus, String> {
        if !state.settings().github.automerge.allowed {
            return Err(
                "auto-merge is turned off in your settings (github.automerge.allowed)".into(),
            );
        }
        let github = self
            .github
            .get()
            .cloned()
            .ok_or("GitHub is not available")?;
        let (repo, number) = self.bound_target(session_id, url)?;
        let status = self.refresh(state, session_id).await?;
        // The reason the card showed beside the disabled switch (a draft, a repository that does
        // not allow auto-merge, a merge queue, settings this token cannot read).
        if let Some(reason) = &status.auto_merge.reason_unavailable {
            return Err(reason.clone());
        }
        if status.head_sha != head_sha {
            return Err(format!(
                "the branch moved since you looked (it was at {}, it is at {} now): look again and turn auto-merge on again",
                short(head_sha),
                short(&status.head_sha)
            ));
        }
        if !status.allowed_methods.contains(&chosen) {
            return Err(format!(
                "this repository does not allow {} merges",
                method_name(chosen)
            ));
        }
        if status.auto_merge.enabled {
            return match status.auto_merge.method {
                Some(current) if current == chosen => Ok(status),
                _ => Err("auto-merge is already on with another method: turn it off first".into()),
            };
        }
        let change = AutoMerge::Enable {
            method: github_method(chosen),
            expected_head_oid: head_sha.to_string(),
        };
        github
            .set_auto_merge(&repo, number, &change)
            .await
            .map_err(|error| refusal(&error))?;
        // From now on a different head turns it off again (see `guard_head`).
        self.set_confirmed(state, &key_of(&repo, number), Some(head_sha.to_string()));
        self.refresh(state, session_id).await
    }

    /// Turns the session's pull request's auto-merge off. Any local caller may ask; there is no
    /// challenge. Nothing is sent when it is already off.
    pub(crate) async fn disable_auto_merge(
        &self,
        state: &Arc<DaemonState>,
        session_id: &str,
        url: &str,
    ) -> Result<PrStatus, String> {
        let github = self
            .github
            .get()
            .cloned()
            .ok_or("GitHub is not available")?;
        let (repo, number) = self.bound_target(session_id, url)?;
        let status = self.refresh(state, session_id).await?;
        if !status.auto_merge.enabled {
            return Ok(status);
        }
        github
            .set_auto_merge(&repo, number, &AutoMerge::Disable)
            .await
            .map_err(|error| refusal(&error))?;
        self.set_confirmed(state, &key_of(&repo, number), None);
        self.refresh(state, session_id).await
    }

    /// Records (or, with `None`, forgets) the head commit the user confirmed for a pull request,
    /// and saves it with every session bound to it.
    fn set_confirmed(&self, state: &DaemonState, key: &Key, head: Option<String>) {
        let sessions: Vec<String> = {
            let mut inner = self.lock();
            let Some(tracked) = inner.tracked.get_mut(key) else {
                return;
            };
            if tracked.confirmed_head == head {
                return;
            }
            tracked.confirmed_head = head;
            inner
                .sessions
                .iter()
                .filter(|(_, bound)| bound.key == *key)
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in sessions {
            self.save(state, &id);
        }
    }

    /// After a read: when Imperium turned auto-merge on for the head the user confirmed and the
    /// head has moved since (a push by anyone), turns it off, forgets the confirmation, and says
    /// why in the next status. GitHub keeps auto-merge on across pushes, so this is what keeps a
    /// later push from being merged unconfirmed; only a merge GitHub completes inside one poll
    /// interval is outside it. Nothing is done when no confirmed head is recorded (Imperium did
    /// not enable it), or when the head is unchanged. A failed turn-off keeps the record, so the
    /// next poll tries again.
    async fn guard_head(
        &self,
        state: &Arc<DaemonState>,
        github: &Arc<dyn GitHub>,
        key: &Key,
        status: &PrStatus,
    ) {
        let (confirmed, repo, number) = {
            let inner = self.lock();
            let Some(tracked) = inner.tracked.get(key) else {
                return;
            };
            let Some(confirmed) = tracked.confirmed_head.clone() else {
                return;
            };
            (confirmed, tracked.repo.clone(), tracked.number)
        };
        // A read that says auto-merge is off does not erase the record: it can be a read that
        // was in flight before the user turned it on. Only turning it off here clears it, and a
        // record that outlives its auto-merge can only cause one extra, harmless disable.
        if !status.auto_merge.enabled || status.head_sha == confirmed {
            return;
        }
        if let Err(error) = github
            .set_auto_merge(&repo, number, &AutoMerge::Disable)
            .await
        {
            tracing::warn!(%repo, pr = number, error = %clean(&error, ERROR_CHARS), "could not turn auto-merge off after the head moved");
            return;
        }
        self.set_confirmed(state, key, None);
        // The card shows why until the next poll reads it again.
        let (changed, sessions) = {
            let mut inner = self.lock();
            let Some(tracked) = inner.tracked.get_mut(key) else {
                return;
            };
            let Some(shown) = tracked.status.as_mut() else {
                return;
            };
            shown.auto_merge.enabled = false;
            shown.auto_merge.method = None;
            shown.auto_merge.reason_unavailable = Some(MOVED_NOTICE.to_string());
            shown.updated_at = now_ms();
            let shown = shown.clone();
            let sessions: Vec<String> = inner
                .sessions
                .iter()
                .filter(|(_, bound)| bound.key == *key)
                .map(|(id, _)| id.clone())
                .collect();
            (shown, sessions)
        };
        for id in sessions {
            self.save(state, &id);
            state.announce(
                PULL_REQUESTS_CHANGED,
                PullRequestsChangedParams {
                    session_id: id.clone(),
                    status: Some(self.personal(&id, changed.clone())),
                },
            );
        }
    }

    /// Reads a pull request from GitHub now when a session follows it, so the cache and the
    /// clients see what the daemon just changed there.
    pub(crate) async fn refresh_pull_request(
        &self,
        state: &Arc<DaemonState>,
        repo: &str,
        number: u64,
    ) {
        let key = Key {
            repo: repo.to_ascii_lowercase(),
            number,
        };
        if self.lock().tracked.contains_key(&key) {
            self.set_confirmed(state, &key, None);
            let _ = self.read(state, &key).await;
        }
    }

    /// Turns auto-merge off before the daemon pushes `branch` for `session_id` (a push that
    /// opens or updates a pull request), when the session has a pull request bound on that
    /// branch. An error means the push must not happen.
    pub(crate) async fn disable_before_branch_push(
        &self,
        state: &Arc<DaemonState>,
        session_id: &str,
        branch: &str,
    ) -> Result<(), String> {
        let target = {
            let inner = self.lock();
            inner
                .sessions
                .get(session_id)
                .and_then(|binding| inner.tracked.get(&binding.key))
                .map(|t| {
                    (
                        t.repo.clone(),
                        t.number,
                        t.status.as_ref().map(|s| s.branch.clone()),
                    )
                })
        };
        let Some((repo, number, bound_branch)) = target else {
            return Ok(());
        };
        if bound_branch.is_some_and(|bound| bound != clean(branch, NAME_CHARS)) {
            return Ok(());
        }
        let github = self.github.get().cloned().ok_or(
            "GitHub is not available, so whether auto-merge is on cannot be checked: nothing was pushed",
        )?;
        disable_before_push(state, github.as_ref(), &repo, number).await
    }

    /// The session's cached status when it was read without error less than one poll interval
    /// ago, so a bind does not spend GitHub quota on a pull request the poller just read.
    pub(crate) fn fresh_status(&self, session_id: &str) -> Option<PrStatus> {
        let interval = self.interval();
        let status = {
            let inner = self.lock();
            let tracked = inner.tracked.get(&inner.sessions.get(session_id)?.key)?;
            let fresh = tracked.read_at.is_some_and(|at| at.elapsed() < interval);
            tracked
                .status
                .clone()
                .filter(|s| fresh && s.error.is_none())?
        };
        Some(self.personal(session_id, status))
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
            .map(|binding| binding.key.clone())
            .ok_or("no pull request is bound to this session")?;
        let status = self.read(state, &key).await?;
        Ok(self.personal(session_id, status))
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
                .filter(|(_, bound)| bound.key == *key)
                .map(|(id, _)| id.clone())
                .collect();
            let Some(tracked) = inner.tracked.get_mut(key) else {
                return result;
            };
            match result {
                Ok(mut fresh) => {
                    tracked.failures = 0;
                    tracked.last_error = None;
                    tracked.read_at = Some(Instant::now());
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
                    let error = clean(&error, ERROR_CHARS);
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
                self.save(state, id);
                state.announce(
                    PULL_REQUESTS_CHANGED,
                    PullRequestsChangedParams {
                        session_id: id.clone(),
                        status: status.clone().map(|s| self.personal(id, s)),
                    },
                );
            }
        }
        if let Ok(status) = &outcome {
            self.guard_head(state, &github, key, status).await;
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

/// Turns auto-merge off on `repo#number` before the daemon pushes to its branch, and never turns
/// it back on: the user confirms again in the desktop app once they have seen the push. Nothing
/// is sent when it is already off, or when this GitHub source has no pull-request status (it
/// cannot have been asked for auto-merge). An error means the state could not be read or the
/// switch could not be turned off, and the caller must not push.
pub async fn disable_before_push(
    state: &Arc<DaemonState>,
    github: &dyn GitHub,
    repo: &str,
    number: u64,
) -> Result<(), String> {
    let status = match github.pr_status(repo, number).await {
        Ok(status) => status,
        Err(error) if imperium_github::is_unsupported(&error) => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not check whether auto-merge is on: {}",
                clean(&error, ERROR_CHARS)
            ));
        }
    };
    if status.auto_merge.is_none() {
        return Ok(());
    }
    github
        .set_auto_merge(repo, number, &AutoMerge::Disable)
        .await
        .map_err(|error| {
            format!(
                "could not turn auto-merge off before pushing: {}",
                clean(&error, ERROR_CHARS)
            )
        })?;
    state
        .pr_monitor()
        .refresh_pull_request(state, repo, number)
        .await;
    Ok(())
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

/// The link the card shows, always built here and never taken from GitHub.
fn pr_url(repo: &str, number: u64) -> String {
    format!("https://github.com/{repo}/pull/{number}")
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
        .filter(|c| !c.is_control() && !crate::autofix::is_invisible(*c))
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

fn github_method(method: PrMergeMethod) -> imperium_github::MergeMethod {
    match method {
        PrMergeMethod::Squash => imperium_github::MergeMethod::Squash,
        PrMergeMethod::Merge => imperium_github::MergeMethod::Merge,
        PrMergeMethod::Rebase => imperium_github::MergeMethod::Rebase,
    }
}

fn method_name(method: PrMergeMethod) -> &'static str {
    match method {
        PrMergeMethod::Squash => "squash",
        PrMergeMethod::Merge => "merge",
        PrMergeMethod::Rebase => "rebase",
    }
}

fn key_of(repo: &str, number: u64) -> Key {
    Key {
        repo: repo.to_ascii_lowercase(),
        number,
    }
}

/// The first characters of a commit id.
fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// What the user is told when GitHub refuses an auto-merge change. A pull request that can
/// already merge is the usual refusal: GitHub will not queue it, and Imperium never merges.
fn refusal(error: &str) -> String {
    if error.contains("clean status") {
        return "GitHub will not queue auto-merge on a pull request that can already be merged: merge it yourself".into();
    }
    clean(error, ERROR_CHARS)
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
        _ if pr.is_in_merge_queue => Some("this pull request is in the merge queue"),
        (_, Some(Err(_))) => Some("this token cannot read the repository's merge settings"),
        (_, Some(Ok(s))) if !s.auto_merge => Some("the repository does not allow auto-merge"),
        (_, Some(Ok(_))) if allowed.is_empty() => Some("the repository allows no merge method"),
        _ => None,
    };
    PrStatus {
        url: pr_url(repo, number),
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

/// Refuses a repository that is not one of the session's own `origin` remotes (a linked
/// worktree or a subfolder of a checkout resolves to its checkout's `origin`). Another session's
/// binding of a repository does not make it this session's.
pub(crate) fn check_repo(session: &Session, repo: &str) -> Result<(), String> {
    let workspace = session.workspace();
    let mut folders = workspace.folders_display();
    folders.push(workspace.root().display().to_string());
    if folders
        .iter()
        .filter_map(|folder| origin_repo(Path::new(folder)))
        .any(|origin| origin.eq_ignore_ascii_case(repo))
    {
        return Ok(());
    }
    Err(format!("{repo} is not this session's origin repository"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_rewound_fork_of_a_bound_session_is_not_bound() {
        let config = "[models]
default = \"fake:m\"
[endpoints.fake]
base_url = \"http://127.0.0.1:9/v1\"
[workflow]
enabled = false
";
        let settings = imperium_config::Config::from_layers([
            imperium_config::Layer::defaults(),
            imperium_config::Layer::new(
                imperium_config::Source::User("test".into()),
                config.parse().unwrap(),
            ),
        ])
        .settings()
        .unwrap();
        let home = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(folder.path())
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let state = DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
        let (session, _) = state
            .create_session(&folder.path().display().to_string(), None)
            .unwrap();
        state.watch(&session, Some(("owner/repo".into(), 7)));
        let meta = |id: &str| -> Value {
            let path = imperium_config::Paths::under(home.path())
                .data_dir()
                .join("sessions")
                .join(format!("{id}.meta.json"));
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        assert_eq!(
            meta(session.id())["pull_request"]["url"],
            "https://github.com/owner/repo/pull/7"
        );
        let fork = state.fork_session(session.id(), 0, None).unwrap();
        assert!(meta(&fork).get("pull_request").is_none());
        let running = state.session(&fork).expect("the fork runs");
        assert!(state.summary(&running).pull_request_url.is_none());
        assert!(state.pr_monitor().url_of(session.id()).is_some());
    }

    #[test]
    fn settings_a_token_cannot_read_disable_auto_merge_and_are_not_read_as_all_off() {
        let pr = imperium_github::PrStatus {
            node_id: "n".into(),
            number: 7,
            title: "t".into(),
            url: String::new(),
            state: "OPEN".into(),
            is_draft: false,
            head_ref: "feature".into(),
            head_sha: "abc".into(),
            base_ref: "main".into(),
            is_in_merge_queue: false,
            review_decision: None,
            mergeable: "MERGEABLE".into(),
            merge_state: "BLOCKED".into(),
            auto_merge: None,
            checks: Vec::new(),
        };
        // A token without push access gets no `allow_*` flags: the switch says so.
        let unreadable: Result<MergeSettings, String> = Err("no allow_squash_merge".into());
        let status = build("o/r", 7, &pr, Some(&unreadable), false);
        assert!(status.allowed_methods.is_empty());
        assert!(!status.auto_merge.enabled);
        assert_eq!(
            status.auto_merge.reason_unavailable.as_deref(),
            Some("this token cannot read the repository's merge settings")
        );
        // Readable settings list what is allowed, and a repository without auto-merge says so.
        let allowed = MergeSettings {
            squash: true,
            merge: false,
            rebase: true,
            auto_merge: true,
        };
        let readable: Result<MergeSettings, String> = Ok(allowed);
        let status = build("o/r", 7, &pr, Some(&readable), false);
        assert_eq!(
            status.allowed_methods,
            [PrMergeMethod::Squash, PrMergeMethod::Rebase]
        );
        assert_eq!(status.auto_merge.reason_unavailable, None);
        let forbidden: Result<MergeSettings, String> = Ok(MergeSettings {
            auto_merge: false,
            ..allowed
        });
        let status = build("o/r", 7, &pr, Some(&forbidden), false);
        assert_eq!(
            status.auto_merge.reason_unavailable.as_deref(),
            Some("the repository does not allow auto-merge")
        );
    }

    #[tokio::test]
    async fn a_push_for_a_bound_branch_is_refused_when_github_is_not_available() {
        let config = "[models]
default = \"fake:m\"
[endpoints.fake]
base_url = \"http://127.0.0.1:9/v1\"
[workflow]
enabled = false
";
        let settings = imperium_config::Config::from_layers([
            imperium_config::Layer::defaults(),
            imperium_config::Layer::new(
                imperium_config::Source::User("test".into()),
                config.parse().unwrap(),
            ),
        ])
        .settings()
        .unwrap();
        let home = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let state = DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
        let (session, _) = state
            .create_session(&folder.path().display().to_string(), None)
            .unwrap();
        let monitor = state.pr_monitor();
        // Not bound: nothing to turn off, so the push is not held up.
        monitor
            .disable_before_branch_push(&state, session.id(), "feature")
            .await
            .unwrap();
        // Bound, and the daemon has no GitHub to ask: the push is refused.
        state.watch(&session, Some(("owner/repo".into(), 7)));
        let refused = monitor
            .disable_before_branch_push(&state, session.id(), "feature")
            .await
            .unwrap_err();
        assert!(refused.contains("nothing was pushed"), "{refused}");
    }

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
    fn github_text_loses_control_and_invisible_characters() {
        let message = "GitHub returned 500: \u{202E}gnp.exe\u{2066}\n\u{E0041}\u{AD}x\u{2029}y";
        assert_eq!(
            clean(message, ERROR_CHARS),
            "GitHub returned 500: gnp.exexy"
        );
        assert_eq!(clean(&"a".repeat(500), ERROR_CHARS).len(), ERROR_CHARS);
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
