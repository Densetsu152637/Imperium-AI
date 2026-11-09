//! Self-update. The app checks its release channel on a schedule the user picks (every 1, 7, or
//! 28 days, measured from the last successful check, which is saved so it holds across
//! restarts), downloads a newer signed build in the background, and installs it when the app
//! quits, or at once when the user asks to restart.
//!
//! With "Check for updates when the computer starts" on, the app is registered to start at login
//! with [`BACKGROUND_FLAG`]. Started that way it opens no window and does not start the daemon:
//! it checks once, installs a newer build if there is one (without starting it), and exits.
//!
//! The schedule lives in the interface's `settings` document (see `store.rs`); the app reads it
//! at startup, before any window opens, and the interface passes changes with
//! `update_set_schedule`.
//!
//! Each channel's feed is the `latest.json` asset of its newest GitHub Release: main follows
//! stable releases (`vX.Y.Z`), experimental follows `vX.Y.N-experimental` pre-releases. The
//! signature in the feed is checked against the public key in `tauri.conf.json`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Notify;

/// The repository whose releases carry the update feeds.
const REPOSITORY: &str = "Densetsu152637/Imperium-AI";
/// How long after startup the first check runs, so it does not compete with opening the window.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
/// How long to wait before trying again after a check failed.
const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
/// The longest single wait, so a computer that slept still checks soon after it wakes.
const LONGEST_WAIT: Duration = Duration::from_secs(15 * 60);
/// The intervals the user can choose, in days.
const INTERVALS: [u32; 3] = [1, 7, 28];
const DAY_SECS: u64 = 24 * 60 * 60;
/// The argument the login entry starts the app with: check, install, and exit, with no window.
pub const BACKGROUND_FLAG: &str = "--background-update";
/// The login entry's name; distinct from the `Imperium` entry `imperium daemon autostart` makes.
pub const LOGIN_ENTRY: &str = "Imperium update check";
/// The event that carries every status change to the interface.
const STATUS_EVENT: &str = "update://status";
/// Marks experimental versions and the tags of experimental releases.
const EXPERIMENTAL_MARK: &str = "-experimental";

/// The release line an install follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Main,
    Experimental,
}

impl Channel {
    /// The channel a build was released on, which is the one it follows until the user changes it.
    fn of_version(version: &str) -> Self {
        if version.contains(EXPERIMENTAL_MARK) {
            Self::Experimental
        } else {
            Self::Main
        }
    }

    fn follows(self, release: &Release) -> bool {
        let experimental = release.tag_name.contains(EXPERIMENTAL_MARK);
        match self {
            Self::Main => !release.prerelease && !experimental,
            Self::Experimental => experimental,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Downloading {
        version: String,
    },
    Ready {
        version: String,
        notes: Option<String>,
    },
    Failed {
        message: String,
    },
}

/// When the app checks on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Schedule {
    /// Days between automatic checks: 1, 7, or 28.
    interval_days: u32,
    /// Check (and install) when the user logs in to the computer.
    at_login: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            interval_days: 1,
            at_login: false,
        }
    }
}

impl Schedule {
    fn new(interval_days: u32, at_login: bool) -> Self {
        let interval_days = if INTERVALS.contains(&interval_days) {
            interval_days
        } else {
            Self::default().interval_days
        };
        Self {
            interval_days,
            at_login,
        }
    }

    /// The schedule saved in the interface's settings document, or the default.
    fn saved() -> Self {
        let Some(text) = crate::store::store_read("settings".into()).ok().flatten() else {
            return Self::default();
        };
        let Ok(settings) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Self::default();
        };
        let days = settings["updateIntervalDays"]
            .as_u64()
            .and_then(|days| u32::try_from(days).ok())
            .unwrap_or(Self::default().interval_days);
        let at_login = settings["checkUpdatesAtLogin"].as_bool().unwrap_or(false);
        Self::new(days, at_login)
    }

    fn interval(self) -> Duration {
        Duration::from_secs(u64::from(self.interval_days) * DAY_SECS)
    }
}

/// What the interface shows about updates.
#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatus {
    current_version: String,
    channel: Channel,
    /// Whether the app checks on its own; development builds only check when asked.
    automatic: bool,
    #[serde(flatten)]
    schedule: Schedule,
    /// When the last successful check finished, in milliseconds since the Unix epoch.
    last_checked_ms: Option<u64>,
    /// When the next automatic check is due, in milliseconds since the Unix epoch.
    next_check_ms: Option<u64>,
    #[serde(flatten)]
    state: UpdateState,
}

/// The saved channel choice and when the last successful check ran.
#[derive(Default, Serialize, Deserialize)]
struct Preferences {
    /// Absent until the user picks one: the build's own channel applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<Channel>,
    /// Seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_checked: Option<u64>,
}

pub struct Updater {
    inner: Mutex<Inner>,
    check_requested: Notify,
    /// The schedule changed: the wait for the next check starts over.
    schedule_changed: Notify,
    /// Held for a whole check, so a check the user asks for never overlaps a scheduled one.
    checking: tokio::sync::Mutex<()>,
}

struct Inner {
    channel: Channel,
    schedule: Schedule,
    /// Seconds since the Unix epoch.
    last_checked: Option<u64>,
    /// After a failed check: when to try again (seconds since the Unix epoch).
    retry_at: Option<u64>,
    state: UpdateState,
    /// A downloaded update waiting to be installed.
    ready: Option<(Update, Vec<u8>)>,
}

impl Updater {
    fn status<R: Runtime>(&self, app: &AppHandle<R>) -> UpdateStatus {
        let inner = self.inner.lock().expect("updater state");
        UpdateStatus {
            current_version: app.package_info().version.to_string(),
            channel: inner.channel,
            automatic: automatic(),
            schedule: inner.schedule,
            last_checked_ms: inner.last_checked.map(|secs| secs * 1000),
            next_check_ms: automatic().then(|| inner.next_check() * 1000),
            state: inner.state.clone(),
        }
    }

    fn set_state<R: Runtime>(&self, app: &AppHandle<R>, state: UpdateState) {
        self.inner.lock().expect("updater state").state = state;
        let _ = app.emit(STATUS_EVENT, self.status(app));
    }
}

impl Inner {
    /// When the next automatic check is due (seconds since the Unix epoch): an interval after
    /// the last successful check, or the retry time after a failure; now if it never ran.
    fn next_check(&self) -> u64 {
        let due = self
            .last_checked
            .map_or(0, |last| last + self.schedule.interval().as_secs());
        self.retry_at.unwrap_or(due)
    }
}

fn automatic() -> bool {
    !cfg!(debug_assertions)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Whether this run was started at login to check for updates in the background.
pub fn background() -> bool {
    std::env::args().any(|arg| arg == BACKGROUND_FLAG)
}

fn read_preferences<R: Runtime>(app: &AppHandle<R>) -> Preferences {
    preferences_file(app)
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_preferences<R: Runtime>(
    app: &AppHandle<R>,
    change: impl FnOnce(&mut Preferences),
) -> Result<(), String> {
    let mut preferences = read_preferences(app);
    change(&mut preferences);
    let Some(file) = preferences_file(app) else {
        return Ok(());
    };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec(&preferences).map_err(|error| error.to_string())?;
    std::fs::write(file, bytes).map_err(|error| error.to_string())
}

/// Registers or removes the login entry to match the schedule. Development builds never
/// register themselves.
fn sync_login_entry<R: Runtime>(app: &AppHandle<R>, at_login: bool) -> Result<(), String> {
    if !automatic() {
        return Ok(());
    }
    let launcher = app.autolaunch();
    let enabled = launcher.is_enabled().unwrap_or(false);
    let result = match (at_login, enabled) {
        (true, false) => launcher.enable(),
        (false, true) => launcher.disable(),
        _ => Ok(()),
    };
    result.map_err(|error| format!("Could not change the login entry: {error}"))
}

fn preferences_file<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("update.json"))
}

/// Registers the updater state and starts the scheduled checks.
pub fn setup<R: Runtime>(app: &AppHandle<R>) {
    let version = app.package_info().version.to_string();
    let saved = read_preferences(app);
    let channel = saved
        .channel
        .unwrap_or_else(|| Channel::of_version(&version));
    let schedule = Schedule::saved();
    app.manage(Updater {
        inner: Mutex::new(Inner {
            channel,
            schedule,
            last_checked: saved.last_checked,
            retry_at: None,
            state: UpdateState::Idle,
            ready: None,
        }),
        check_requested: Notify::new(),
        schedule_changed: Notify::new(),
        checking: tokio::sync::Mutex::new(()),
    });
    if let Err(error) = sync_login_entry(app, schedule.at_login) {
        eprintln!("{error}");
    }

    let app = app.clone();
    if background() {
        tauri::async_runtime::spawn(async move {
            check_in_background(&app).await;
            app.exit(0);
        });
        return;
    }
    tauri::async_runtime::spawn(async move {
        let updater = app.state::<Updater>();
        if !automatic() {
            loop {
                updater.check_requested.notified().await;
                check(&app).await;
            }
        }
        let mut first = true;
        loop {
            // Waits in steps, measured against the clock, so time asleep counts.
            loop {
                let next = updater.inner.lock().expect("updater state").next_check();
                let mut wait = Duration::from_secs(next.saturating_sub(now_secs()));
                if first {
                    wait = wait.max(FIRST_CHECK_AFTER);
                    first = false;
                }
                if wait.is_zero() {
                    break;
                }
                tokio::select! {
                    () = tokio::time::sleep(wait.min(LONGEST_WAIT)) => {}
                    () = updater.check_requested.notified() => break,
                    () = updater.schedule_changed.notified() => {}
                }
            }
            check(&app).await;
        }
    });
}

/// The login run: checks once and installs a newer build without starting it. The daemon is
/// stopped first only if it is the bundled one and an update is about to replace it.
async fn check_in_background<R: Runtime>(app: &AppHandle<R>) {
    check(app).await;
    let updater = app.state::<Updater>();
    let ready = updater.inner.lock().expect("updater state").ready.take();
    if let Some((update, bytes)) = ready {
        crate::daemon::stop_bundled(app).await;
        if let Err(error) = update.restart_after_install(false).install(bytes) {
            eprintln!("Installing the update failed: {error}");
        }
    }
}

/// Installs a downloaded update as the app exits, without starting it again.
pub fn install_on_exit<R: Runtime>(app: &AppHandle<R>) {
    let Some(updater) = app.try_state::<Updater>() else {
        return;
    };
    let ready = updater.inner.lock().expect("updater state").ready.take();
    if let Some((update, bytes)) = ready {
        crate::daemon::stop_bundled_blocking(app);
        let _ = update.restart_after_install(false).install(bytes);
    }
}

async fn check<R: Runtime>(app: &AppHandle<R>) {
    let updater = app.state::<Updater>();
    let _checking = updater.checking.lock().await;
    let (channel, downloaded) = {
        let inner = updater.inner.lock().expect("updater state");
        let downloaded = inner
            .ready
            .as_ref()
            .map(|(update, _)| update.version.clone());
        (inner.channel, downloaded)
    };
    updater.set_state(app, UpdateState::Checking);
    let found = find(app, channel).await;
    {
        // A check that reached the feed counts, whatever the download does next.
        let mut inner = updater.inner.lock().expect("updater state");
        if found.is_ok() {
            let now = now_secs();
            inner.last_checked = Some(now);
            inner.retry_at = None;
            drop(inner);
            if let Err(error) = write_preferences(app, |saved| saved.last_checked = Some(now)) {
                eprintln!("Could not save when updates were checked: {error}");
            }
        } else {
            inner.retry_at = Some(now_secs() + RETRY_AFTER.as_secs());
        }
    }
    let state = match found {
        Ok(None) => {
            updater.inner.lock().expect("updater state").ready = None;
            UpdateState::UpToDate
        }
        Ok(Some(update)) if downloaded.as_deref() == Some(&update.version) => UpdateState::Ready {
            version: update.version,
            notes: update.body,
        },
        Ok(Some(update)) => {
            updater.set_state(
                app,
                UpdateState::Downloading {
                    version: update.version.clone(),
                },
            );
            match update.download(|_, _| {}, || {}).await {
                Ok(bytes) => {
                    let state = UpdateState::Ready {
                        version: update.version.clone(),
                        notes: update.body.clone(),
                    };
                    updater.inner.lock().expect("updater state").ready = Some((update, bytes));
                    state
                }
                Err(error) => UpdateState::Failed {
                    message: format!("Downloading {} failed: {error}", update.version),
                },
            }
        }
        Err(message) => UpdateState::Failed { message },
    };
    updater.set_state(app, state);
}

/// A newer build on `channel`, if its feed offers one.
///
/// The feed and the installer are fetched through the GitHub API's asset URLs rather than the
/// `github.com/.../releases/download` links in `latest.json`: the API serves the same files with
/// `Accept: application/octet-stream`, and keeps working when the download links do not (they
/// have returned 504 for hours at a time).
async fn find<R: Runtime>(app: &AppHandle<R>, channel: Channel) -> Result<Option<Update>, String> {
    let Some(release) = newest_release(channel).await? else {
        return Ok(None);
    };
    let Some(feed) = release.asset("latest.json") else {
        return Ok(None);
    };
    let update = app
        .updater_builder()
        .endpoints(vec![feed.url.clone()])
        .and_then(|builder| builder.header("Accept", "application/octet-stream"))
        .and_then(|builder| builder.header("User-Agent", "imperium-desktop"))
        .and_then(|builder| builder.build())
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| format!("Checking for updates failed: {error}"))?;
    Ok(update.map(|mut update| {
        // The installer the feed names, by file name, from the same release.
        let file = update
            .download_url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .map(str::to_owned);
        if let Some(asset) = file.and_then(|file| release.asset(&file)) {
            update.download_url = asset.url.clone();
        }
        update
    }))
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    /// The API URL, which serves the file's bytes to `Accept: application/octet-stream`.
    url: tauri::Url,
}

/// The channel's newest release, or `None` when the channel has none yet.
async fn newest_release(channel: Channel) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30");
    let response = reqwest::Client::new()
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "imperium-desktop")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("Listing releases failed: {error}"))?;
    let releases: Vec<Release> = serde_json::from_slice(
        &response
            .bytes()
            .await
            .map_err(|error| format!("Listing releases failed: {error}"))?,
    )
    .map_err(|error| format!("Unexpected release listing: {error}"))?;
    // Releases are listed newest first.
    Ok(releases
        .into_iter()
        .find(|release| !release.draft && channel.follows(release)))
}

#[tauri::command]
pub fn update_status<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
) -> UpdateStatus {
    updater.status(&app)
}

/// Starts a check now; the result arrives as a status event.
#[tauri::command]
pub fn update_check(updater: tauri::State<'_, Updater>) {
    updater.check_requested.notify_one();
}

/// Applies the schedule the user chose: how often to check, and whether to check when the
/// computer starts (registering or removing the login entry). The interface saves the choice in
/// its settings document, where the app reads it at the next start.
#[tauri::command]
pub fn update_set_schedule<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
    interval_days: u32,
    at_login: bool,
) -> Result<UpdateStatus, String> {
    let schedule = Schedule::new(interval_days, at_login);
    let changed = {
        let mut inner = updater.inner.lock().expect("updater state");
        let changed = inner.schedule != schedule;
        inner.schedule = schedule;
        changed
    };
    if changed {
        updater.schedule_changed.notify_one();
    }
    sync_login_entry(&app, schedule.at_login)?;
    let status = updater.status(&app);
    let _ = app.emit(STATUS_EVENT, status.clone());
    Ok(status)
}

/// Switches channel, saves the choice, and checks the new channel.
#[tauri::command]
pub fn update_set_channel<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
    channel: Channel,
) -> Result<UpdateStatus, String> {
    write_preferences(&app, |saved| saved.channel = Some(channel))?;
    {
        let mut inner = updater.inner.lock().expect("updater state");
        inner.channel = channel;
        inner.ready = None;
        inner.state = UpdateState::Idle;
    }
    updater.check_requested.notify_one();
    Ok(updater.status(&app))
}

/// Installs the downloaded update and starts the new version.
#[tauri::command]
pub async fn update_restart<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    // The new version starts its own daemon.
    crate::daemon::stop_bundled(&app).await;
    install_and_restart(&app, &app.state::<Updater>())
}

/// Checks the channel now and, when it offers a newer build, downloads and installs it and
/// starts the new version. Returns the status when there is nothing to install.
#[tauri::command]
pub async fn update_install_now<R: Runtime>(app: AppHandle<R>) -> Result<UpdateStatus, String> {
    check(&app).await;
    let updater = app.state::<Updater>();
    let status = updater.status(&app);
    match status.state {
        UpdateState::Ready { .. } => {
            crate::daemon::stop_bundled(&app).await;
            install_and_restart(&app, &updater).map(|()| status)
        }
        UpdateState::Failed { message } => Err(message),
        _ => Ok(status),
    }
}

fn install_and_restart<R: Runtime>(app: &AppHandle<R>, updater: &Updater) -> Result<(), String> {
    let ready = updater.inner.lock().expect("updater state").ready.take();
    let Some((update, bytes)) = ready else {
        return Err("No update has been downloaded.".into());
    };
    // On Windows the installer takes over here and starts the new version itself.
    update.install(bytes).map_err(|error| error.to_string())?;
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool) -> Release {
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease,
            assets: Vec::new(),
        }
    }

    #[test]
    fn the_next_check_is_an_interval_after_the_last_success() {
        let inner = |days, last_checked, retry_at| Inner {
            channel: Channel::Main,
            schedule: Schedule::new(days, false),
            last_checked,
            retry_at,
            state: UpdateState::Idle,
            ready: None,
        };
        // Never checked: due at once.
        assert_eq!(inner(1, None, None).next_check(), 0);
        assert_eq!(inner(1, Some(1000), None).next_check(), 1000 + DAY_SECS);
        assert_eq!(inner(7, Some(1000), None).next_check(), 1000 + 7 * DAY_SECS);
        assert_eq!(
            inner(28, Some(1000), None).next_check(),
            1000 + 28 * DAY_SECS
        );
        // A failed check retries sooner than the interval would.
        assert_eq!(inner(7, Some(1000), Some(5000)).next_check(), 5000);
    }

    #[test]
    fn only_offered_intervals_are_kept() {
        assert_eq!(Schedule::new(7, true).interval_days, 7);
        assert_eq!(Schedule::new(3, false).interval_days, 1);
        assert_eq!(Schedule::new(0, false).interval_days, 1);
    }

    #[test]
    fn older_preference_files_still_load() {
        let old: Preferences = serde_json::from_str(r#"{"channel":"experimental"}"#).unwrap();
        assert_eq!(old.channel, Some(Channel::Experimental));
        assert_eq!(old.last_checked, None);
        let empty: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.channel, None);
    }

    #[test]
    fn a_build_follows_the_channel_it_was_released_on() {
        assert_eq!(Channel::of_version("0.1.0"), Channel::Main);
        assert_eq!(Channel::of_version("0.2.0-beta.1"), Channel::Main);
        assert_eq!(
            Channel::of_version("0.1.4-experimental"),
            Channel::Experimental
        );
    }

    #[test]
    fn each_channel_follows_its_own_releases() {
        let stable = release("v0.1.0", false);
        let beta = release("v0.2.0-beta.1", true);
        let experimental = release("v0.1.4-experimental", true);
        assert!(Channel::Main.follows(&stable));
        assert!(!Channel::Main.follows(&beta));
        assert!(!Channel::Main.follows(&experimental));
        assert!(Channel::Experimental.follows(&experimental));
        assert!(!Channel::Experimental.follows(&stable));
    }
}
