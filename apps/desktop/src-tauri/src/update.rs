//! Self-update. The app checks its release channel at startup and every few hours, downloads a
//! newer signed build in the background, and installs it when the app quits, or at once when the
//! user asks to restart.
//!
//! Each channel's feed is the `latest.json` asset of its newest GitHub Release: main follows
//! stable releases (`vX.Y.Z`), experimental follows `vX.Y.Z-experimental.N` pre-releases. The
//! signature in the feed is checked against the public key in `tauri.conf.json`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Notify;

/// The repository whose releases carry the update feeds.
const REPOSITORY: &str = "Densetsu152637/Imperium-AI";
/// How long after startup the first check runs, so it does not compete with opening the window.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
/// How often the app checks again while it stays open.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// The event that carries every status change to the interface.
const STATUS_EVENT: &str = "update://status";
/// Marks experimental versions and the tags of experimental releases.
const EXPERIMENTAL_MARK: &str = "-experimental.";

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

/// What the interface shows about updates.
#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatus {
    current_version: String,
    channel: Channel,
    /// Whether the app checks on its own; development builds only check when asked.
    automatic: bool,
    #[serde(flatten)]
    state: UpdateState,
}

/// The saved channel choice.
#[derive(Serialize, Deserialize)]
struct Preferences {
    channel: Channel,
}

pub struct Updater {
    inner: Mutex<Inner>,
    check_requested: Notify,
    /// Held for a whole check, so a check the user asks for never overlaps a scheduled one.
    checking: tokio::sync::Mutex<()>,
}

struct Inner {
    channel: Channel,
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
            state: inner.state.clone(),
        }
    }

    fn set_state<R: Runtime>(&self, app: &AppHandle<R>, state: UpdateState) {
        self.inner.lock().expect("updater state").state = state;
        let _ = app.emit(STATUS_EVENT, self.status(app));
    }
}

fn automatic() -> bool {
    !cfg!(debug_assertions)
}

fn preferences_file<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("update.json"))
}

/// Registers the updater state and starts the periodic check.
pub fn setup<R: Runtime>(app: &AppHandle<R>) {
    let version = app.package_info().version.to_string();
    let saved = preferences_file(app)
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice::<Preferences>(&bytes).ok());
    let channel = saved.map_or_else(|| Channel::of_version(&version), |saved| saved.channel);
    app.manage(Updater {
        inner: Mutex::new(Inner {
            channel,
            state: UpdateState::Idle,
            ready: None,
        }),
        check_requested: Notify::new(),
        checking: tokio::sync::Mutex::new(()),
    });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let updater = app.state::<Updater>();
        if automatic() {
            tokio::time::sleep(FIRST_CHECK_AFTER).await;
        } else {
            updater.check_requested.notified().await;
        }
        loop {
            check(&app).await;
            if automatic() {
                tokio::select! {
                    () = tokio::time::sleep(CHECK_EVERY) => {}
                    () = updater.check_requested.notified() => {}
                }
            } else {
                updater.check_requested.notified().await;
            }
        }
    });
}

/// Installs a downloaded update as the app exits, without starting it again.
pub fn install_on_exit<R: Runtime>(app: &AppHandle<R>) {
    let Some(updater) = app.try_state::<Updater>() else {
        return;
    };
    let ready = updater.inner.lock().expect("updater state").ready.take();
    if let Some((update, bytes)) = ready {
        crate::daemon::stop_bundled();
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
    let state = match find(app, channel).await {
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

/// Switches channel, saves the choice, and checks the new channel.
#[tauri::command]
pub fn update_set_channel<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
    channel: Channel,
) -> Result<UpdateStatus, String> {
    if let Some(file) = preferences_file(&app) {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let bytes =
            serde_json::to_vec(&Preferences { channel }).map_err(|error| error.to_string())?;
        std::fs::write(file, bytes).map_err(|error| error.to_string())?;
    }
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
pub fn update_restart<R: Runtime>(
    app: AppHandle<R>,
    updater: tauri::State<'_, Updater>,
) -> Result<(), String> {
    install_and_restart(&app, &updater)
}

/// Checks the channel now and, when it offers a newer build, downloads and installs it and
/// starts the new version. Returns the status when there is nothing to install.
#[tauri::command]
pub async fn update_install_now<R: Runtime>(app: AppHandle<R>) -> Result<UpdateStatus, String> {
    check(&app).await;
    let updater = app.state::<Updater>();
    let status = updater.status(&app);
    match status.state {
        UpdateState::Ready { .. } => install_and_restart(&app, &updater).map(|()| status),
        UpdateState::Failed { message } => Err(message),
        _ => Ok(status),
    }
}

fn install_and_restart<R: Runtime>(app: &AppHandle<R>, updater: &Updater) -> Result<(), String> {
    let ready = updater.inner.lock().expect("updater state").ready.take();
    let Some((update, bytes)) = ready else {
        return Err("No update has been downloaded.".into());
    };
    // The new version starts its own daemon. On Windows the installer takes over here and
    // starts the new version itself.
    crate::daemon::stop_bundled();
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
    fn a_build_follows_the_channel_it_was_released_on() {
        assert_eq!(Channel::of_version("0.1.0"), Channel::Main);
        assert_eq!(Channel::of_version("0.2.0-beta.1"), Channel::Main);
        assert_eq!(
            Channel::of_version("0.1.0-experimental.4"),
            Channel::Experimental
        );
    }

    #[test]
    fn each_channel_follows_its_own_releases() {
        let stable = release("v0.1.0", false);
        let beta = release("v0.2.0-beta.1", true);
        let experimental = release("v0.1.0-experimental.4", true);
        assert!(Channel::Main.follows(&stable));
        assert!(!Channel::Main.follows(&beta));
        assert!(!Channel::Main.follows(&experimental));
        assert!(Channel::Experimental.follows(&experimental));
        assert!(!Channel::Experimental.follows(&stable));
    }
}
