//! A browser agents drive over the Chrome DevTools Protocol: Chrome, Chromium, or Edge,
//! started with its own profile under Imperium's data folder (never the user's), visible or
//! headless. After each action a screenshot is taken so the desktop app can mirror the page.
//!
//! The debugger is reached only through the pipe the browser inherits (`--remote-debugging-pipe`,
//! [`crate::launch`]): the browser opens no TCP port, so no other process on the machine can
//! attach to the signed-in profile and read its cookies. Everything goes over that one
//! browser-level connection: tabs are listed, opened and closed with `Target.*` commands, and the
//! active tab is driven through a session of its own on it (`Target.attachToTarget` with
//! `flatten`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use imperium_protocol::methods::BrowserAction;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cdp::Pipe;
use crate::launch::{self, Process};
use crate::tabs::{self, Listed, TabRec};

/// From starting the browser to having a page to drive. The wait polls the debugger and ends
/// as soon as a page is listed (or the browser exits), so this is only a ceiling. A browser's
/// first start on a CI runner is cold (profile, disk cache, component registration) and took
/// more than 30 seconds once on Ubuntu, where the warm-up alone is allowed 90.
const START_TIMEOUT: Duration = Duration::from_secs(90);
/// Longest one quick request to the browser's debugger (listing or closing tabs) may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest the browser may take to answer its first page load. Chrome holds that request back
/// until its own start-up work (a fresh profile registers dozens of components first) has
/// finished, which took 10 to 45 seconds on a Windows CI runner; it is waited out once, in
/// `launch`, so no later page load pays for it.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(90);
/// Longest page text returned by `read`.
const MAX_TEXT: usize = 20_000;
/// How much of the browser's output an error reports, in bytes and in lines.
const LOG_TAIL: usize = 2_000;
const LOG_LINES: usize = 10;
/// The most tabs open at once, the pages a page opens by itself included; further ones are
/// closed again.
pub const MAX_TABS: usize = 8;
/// How long, after an action, to wait for the browser to report a download the action started.
const DOWNLOAD_POLL: Duration = Duration::from_millis(150);
/// Downloads remembered so the agent can save one without naming its URL.
const MAX_PENDING_DOWNLOADS: usize = 10;
/// The most time, and the most messages, one look for downloads may take.
const DOWNLOAD_READ_LIMIT: Duration = Duration::from_secs(1);
const DOWNLOAD_EVENTS: usize = 200;

/// What the page looks like after an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub url: String,
    pub title: String,
    /// A JPEG screenshot, base64-encoded.
    pub screenshot: String,
    /// The tab this is (the active one), as `browser_tabs` names it.
    pub tab: String,
    /// Every open tab, in the order they were opened.
    pub tabs: Vec<TabInfo>,
}

/// One open tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// A download a page started, which the browser refused (downloads are denied; the agent saves
/// one deliberately with `browser_download`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDownload {
    pub url: String,
    /// The name the page suggested; page-controlled, so not trusted as a file name.
    pub filename: String,
}

/// The browser's one debugger connection, over its pipe.
struct Cdp {
    pipe: Pipe,
    next_id: u64,
    log_path: PathBuf,
    /// `Browser.downloadWillBegin` events read while waiting for an answer, oldest first.
    downloads: Vec<Value>,
    /// The browser closed the pipe: nothing it is asked is answered any more, and it can no
    /// longer be held to refusing downloads.
    closed: bool,
}

impl Cdp {
    fn new(pipe: Pipe, log_path: PathBuf) -> Self {
        Cdp {
            pipe,
            next_id: 1,
            log_path,
            downloads: Vec::new(),
            closed: false,
        }
    }

    /// Sends a CDP command, to the page `session` names or to the browser itself, and waits for
    /// its result; gives up when the browser is silent for `limit`. Messages in between are
    /// events: the downloads among them are kept for [`Browser::notices`].
    async fn call(
        &mut self,
        limit: Duration,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        if let Err(error) = self.pipe.send(&message) {
            self.closed = true;
            return Err(error);
        }
        loop {
            let frame = match tokio::time::timeout(limit, self.pipe.next()).await {
                Err(_) => {
                    return Err(format!(
                        "{method} timed out; browser output: {}",
                        log_tail(&self.log_path)
                    ));
                }
                Ok(None) => {
                    self.closed = true;
                    return Err("the browser closed".into());
                }
                Ok(Some(frame)) => frame,
            };
            let Ok(value) = serde_json::from_slice::<Value>(&frame) else {
                continue;
            };
            if value["id"].as_u64() != Some(id) {
                self.note(&value);
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(format!(
                    "{method}: {}",
                    error["message"].as_str().unwrap_or("failed")
                ));
            }
            return Ok(value["result"].clone());
        }
    }

    /// Keeps a download the browser reported (and refused); other events are not needed.
    fn note(&mut self, message: &Value) {
        if message["method"] == "Browser.downloadWillBegin" {
            self.downloads.push(message["params"].clone());
            if self.downloads.len() > DOWNLOAD_EVENTS {
                self.downloads.remove(0);
            }
        }
    }

    /// Reads what the browser sent unasked, for at most `DOWNLOAD_READ_LIMIT` in all and
    /// `DOWNLOAD_EVENTS` messages, so a page that floods the connection cannot hold a call up;
    /// it stops as soon as the browser is quiet for `DOWNLOAD_POLL`.
    async fn drain(&mut self) {
        let deadline = Instant::now() + DOWNLOAD_READ_LIMIT;
        let mut read = 0;
        while read < DOWNLOAD_EVENTS && !self.closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(DOWNLOAD_POLL.min(left), self.pipe.next()).await {
                Err(_) => break,
                Ok(None) => {
                    tracing::warn!("the browser's debugging pipe closed");
                    self.closed = true;
                }
                Ok(Some(frame)) => {
                    read += 1;
                    if let Ok(value) = serde_json::from_slice::<Value>(&frame) {
                        self.note(&value);
                    }
                }
            }
        }
    }
}

pub struct Browser {
    _process: Option<Process>,
    cdp: Cdp,
    /// The debugger session of the active tab, on the browser's connection.
    session: String,
    /// Whether the start-up warm-up opened its throwaway page. False when the browser would
    /// not open one (the first navigation then pays for the start-up work, as it used to).
    warmed: bool,
    /// Open tabs in the order they were opened; `session` talks to the one labelled `active`.
    tabs: Vec<TabRec>,
    active: u32,
    next_tab: u32,
    /// Tabs the pages opened themselves, not yet reported to the agent.
    opened_by_pages: Vec<String>,
    /// Pages the browser lists that are not tabs: the throwaway page of the start-up warm-up.
    ignored: Vec<String>,
    /// The last URL the browser listed for each open tab (by label), for the approval rules.
    tab_urls: Vec<(u32, String)>,
    pending_downloads: Vec<PendingDownload>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser").finish_non_exhaustive()
    }
}

/// Chrome, Chromium, or Edge in its usual install location, or on `PATH`.
pub fn find_browser() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ]
    } else {
        &[]
    };
    if let Some(found) = candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
    {
        return Some(found);
    }
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
    ];
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// What the browser is started with: the debugger on its pipe, never a port; its own profile;
/// and WebRTC kept off the network except through a proxy, so a page cannot send UDP straight
/// out to probe the local network or learn the machine's local addresses.
fn launch_args(profile_dir: &Path, headless: bool) -> Vec<OsString> {
    let mut user_data_dir = OsString::from("--user-data-dir=");
    user_data_dir.push(profile_dir.as_os_str());
    let mut args: Vec<OsString> = vec![
        "--remote-debugging-pipe".into(),
        user_data_dir,
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--window-size=1280,900".into(),
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into(),
    ];
    if headless {
        args.push("--headless=new".into());
    }
    args.push("about:blank".into());
    args
}

impl Browser {
    /// Starts `program` with a profile in `profile_dir` and connects to its first page.
    pub async fn launch(
        program: &Path,
        profile_dir: &Path,
        headless: bool,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(profile_dir).map_err(|error| error.to_string())?;
        // Defence in depth next to the debugger's refusal: should a download ever get through,
        // it lands in an empty folder of the browser's own profile, after a prompt nobody sees.
        if let Err(error) = set_download_prefs(profile_dir) {
            tracing::warn!(%error, "could not point the browser's downloads at an empty folder");
        }
        let log_path = profile_dir.join("imperium-browser.log");
        let log = std::fs::File::create(&log_path).map_err(|error| error.to_string())?;
        let deadline = Instant::now() + START_TIMEOUT;
        let (mut process, pipe) = launch::spawn(program, &launch_args(profile_dir, headless), log)
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;
        let mut cdp = Cdp::new(pipe, log_path.clone());
        let waited = wait_for_page(&mut cdp, deadline, || process.exited().is_some()).await;
        let target = match waited {
            Ok(target) => target,
            Err(reason) => {
                let status = match process.exited() {
                    Some(status) => format!("the browser exited ({status})"),
                    None => "the browser was still running".to_string(),
                };
                return Err(format!(
                    "{}; {status}; output: {}",
                    redact(&reason),
                    log_tail(&log_path)
                ));
            }
        };
        let mut browser = Browser::assemble(cdp, Some(process), target);
        // The browser may still be starting: its first answers get what is left of the start.
        let left = deadline
            .saturating_duration_since(Instant::now())
            .max(LOAD_TIMEOUT);
        browser.attach_within(1, left, false).await?;
        browser.warm_up().await?;
        // After the warm-up, so the browser has finished starting. Failing to refuse downloads
        // fails the start: a browser that might save files by itself is not offered.
        browser.refuse_downloads().await?;
        Ok(browser)
    }

    /// A browser whose one tab is `target`, not yet attached to.
    fn assemble(cdp: Cdp, process: Option<Process>, target: String) -> Self {
        Browser {
            _process: process,
            cdp,
            session: String::new(),
            warmed: false,
            tabs: vec![TabRec { label: 1, target }],
            active: 1,
            next_tab: 2,
            opened_by_pages: Vec::new(),
            ignored: Vec::new(),
            tab_urls: Vec::new(),
            pending_downloads: Vec::new(),
        }
    }

    /// The browser's process id, for the test that it listens on no port.
    #[cfg(test)]
    fn process_id(&self) -> Option<u32> {
        self._process.as_ref().and_then(Process::id)
    }

    /// Whether the start-up warm-up ran: the browser opened its throwaway page.
    pub fn warmed(&self) -> bool {
        self.warmed
    }

    /// Waits until the browser answers a first page load, for at most `WARM_UP_TIMEOUT` in
    /// all. A throwaway page loads an empty page the daemon serves on loopback for the moment
    /// (nothing leaves the machine); that load is held back by Chrome's start-up work in the
    /// same way as the first real one, so doing it here, with a long limit, keeps that wait out
    /// of the first real `Page.navigate`, whose limit is short. The page being driven is not
    /// touched, so its history never holds the throwaway page.
    async fn warm_up(&mut self) -> Result<(), String> {
        self.warm_up_within(WARM_UP_TIMEOUT).await
    }

    /// `warm_up` with its limit chosen. The limit is on the whole wait; events the browser
    /// sends meanwhile do not extend it. Only a timeout or a closed browser fails the wait: any
    /// other answer (a navigation refused, a page replaced by another) means the request ended.
    async fn warm_up_within(&mut self, limit: Duration) -> Result<(), String> {
        let started = Instant::now();
        let outcome = tokio::time::timeout(limit, async {
            let opened = self.warm_up_navigations(limit).await?;
            self.warmed = opened.is_some();
            if let Some(throwaway) = opened {
                // The browser can keep listing it for a while; it is never a tab of ours.
                self.ignored.push(throwaway);
                // A page that was opened and closed can leave the one being driven in the
                // background, where a screenshot waits for a frame that is never drawn.
                self.bring_to_front(limit).await?;
            }
            Ok::<(), String>(())
        })
        .await;
        let elapsed = started.elapsed();
        match outcome {
            Ok(Ok(())) => {
                tracing::info!(?elapsed, "the browser answered its first request");
                Ok(())
            }
            Ok(Err(error)) => {
                let error = redact(&error);
                tracing::warn!(?elapsed, %error, "the browser failed its first request");
                Err(format!("the browser did not finish starting: {error}"))
            }
            Err(_) => {
                tracing::warn!(
                    ?elapsed,
                    "the browser did not answer its first request in time"
                );
                Err(format!(
                    "the browser did not finish starting within {limit:?}; browser output: {}",
                    log_tail(&self.cdp.log_path)
                ))
            }
        }
    }

    async fn bring_to_front(&mut self, limit: Duration) -> Result<(), String> {
        match self
            .call_within(limit, "Page.bringToFront", json!({}))
            .await
        {
            Err(error) if !error.starts_with("Page.bringToFront: ") => Err(error),
            Err(error) => {
                tracing::debug!(%error, "the browser refused to bring its page to the front");
                Ok(())
            }
            Ok(_) => Ok(()),
        }
    }

    /// Serves an empty page on loopback for the throwaway page, opens, loads and closes that
    /// page, and stops serving; the throwaway's target id when it was opened.
    async fn warm_up_navigations(&mut self, limit: Duration) -> Result<Option<String>, String> {
        let listener = match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await
        {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, "could not serve a page to warm the browser up");
                return Ok(None);
            }
        };
        let url = match listener.local_addr() {
            Ok(address) => format!("http://{address}/"),
            Err(error) => {
                tracing::warn!(%error, "could not serve a page to warm the browser up");
                return Ok(None);
            }
        };
        // Stopped however this ends, a warm-up cut off by its time limit included: the guard
        // goes with this future, and the page is served only for the moment of the warm-up.
        let _server = AbortOnDrop(tokio::spawn(serve_blank(listener)));
        self.throwaway(&url, limit).await
    }

    /// Opens a throwaway page, loads `url` in it and closes it again.
    async fn throwaway(&mut self, url: &str, limit: Duration) -> Result<Option<String>, String> {
        let created = match self
            .cdp
            .call(
                limit,
                None,
                "Target.createTarget",
                json!({"url": "about:blank"}),
            )
            .await
        {
            Ok(created) => created,
            // Without a throwaway page there is nothing to wait on; the first navigation then
            // pays, as it did before.
            Err(error) if error.starts_with("Target.createTarget: ") => {
                tracing::warn!(%error, "could not open a page to warm the browser up");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let Some(id) = created["targetId"]
            .as_str()
            .filter(|id| tabs::is_target_id(id))
            .map(str::to_string)
        else {
            tracing::warn!("the browser opened a page without an id");
            return Ok(None);
        };
        let outcome = self.navigate_throwaway(&id, url, limit).await;
        // Close it whatever happened (failing to is harmless) and wait until it is gone.
        self.close_target(&id).await;
        for _ in 0..30 {
            let present = self
                .cdp
                .call(REQUEST_TIMEOUT, None, "Target.getTargets", json!({}))
                .await
                .is_ok_and(|reply| {
                    tabs::listed(&reply["targetInfos"])
                        .iter()
                        .any(|target| target.id == id)
                });
            if !present {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        outcome.map(|()| Some(id))
    }

    async fn navigate_throwaway(
        &mut self,
        id: &str,
        url: &str,
        limit: Duration,
    ) -> Result<(), String> {
        let attached = match self
            .cdp
            .call(
                limit,
                None,
                "Target.attachToTarget",
                json!({"targetId": id, "flatten": true}),
            )
            .await
        {
            Ok(attached) => attached,
            Err(error) if error.starts_with("Target.attachToTarget: ") => {
                tracing::debug!(%error, "the browser refused to attach to the warm-up page");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let Some(session) = attached["sessionId"].as_str() else {
            return Ok(());
        };
        match self
            .cdp
            .call(limit, Some(session), "Page.navigate", json!({"url": url}))
            .await
        {
            Err(error) if !error.starts_with("Page.navigate: ") => Err(error),
            Err(error) => {
                tracing::debug!(%error, "the browser refused the warm-up navigation");
                Ok(())
            }
            Ok(_) => Ok(()),
        }
    }

    /// Sends a CDP command to the active tab and waits for its result, skipping events in
    /// between.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.call_within(LOAD_TIMEOUT, method, params).await
    }

    /// `call`, giving up when the browser is silent for `limit`.
    async fn call_within(
        &mut self,
        limit: Duration,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.cdp
            .call(limit, Some(self.session.as_str()), method, params)
            .await
    }

    /// Evaluates a JavaScript expression and returns its value.
    async fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        self.evaluate_as(expression, false).await
    }

    /// `evaluate`, where `gesture` makes the script count as something the user did, as a click
    /// does: a link that opens a new window is then allowed to, where a script alone would be
    /// blocked as a pop-up. The number of tabs stays bounded either way.
    async fn evaluate_as(&mut self, expression: &str, gesture: bool) -> Result<Value, String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                    "userGesture": gesture,
                }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(format!(
                "the page script failed: {}",
                exception["exception"]["description"]
                    .as_str()
                    .unwrap_or("error")
            ));
        }
        Ok(result["result"]["value"].clone())
    }

    async fn wait_until_loaded(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        while Instant::now() < deadline {
            if self.evaluate("document.readyState").await?.as_str() == Some("complete") {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Err("the page did not finish loading".into())
    }

    /// The current URL, title, and a screenshot.
    pub async fn view(&mut self) -> Result<View, String> {
        let info = self
            .evaluate("JSON.stringify([location.href, document.title])")
            .await?;
        let [url, title]: [String; 2] =
            serde_json::from_str(info.as_str().unwrap_or("[\"\",\"\"]"))
                .map_err(|error| error.to_string())?;
        let shot = self
            .call(
                "Page.captureScreenshot",
                json!({"format": "jpeg", "quality": 60}),
            )
            .await?;
        // Best effort: the tab list is a courtesy next to the page itself.
        let listed = self.sync_tabs().await.unwrap_or_default();
        let tabs = self.tab_infos(&listed, &url, &title);
        Ok(View {
            url,
            title,
            screenshot: shot["data"].as_str().unwrap_or_default().to_string(),
            tab: self.active.to_string(),
            tabs,
        })
    }

    /// Opens `url` in the active tab, or in a new tab (which becomes the active one) with
    /// `new_tab`; returns the page's text.
    pub async fn open(&mut self, url: &str, new_tab: bool) -> Result<String, String> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!("{url} is not an http or https URL"));
        }
        if new_tab {
            // Pages that opened tabs of their own count towards the limit.
            self.sync_tabs().await?;
            self.create_tab().await?;
        }
        self.navigate(url).await?;
        self.wait_until_loaded().await?;
        self.read().await
    }

    async fn navigate(&mut self, url: &str) -> Result<(), String> {
        let result = self.call("Page.navigate", json!({"url": url})).await?;
        if let Some(error) = result["errorText"].as_str() {
            return Err(format!("could not open {url}: {error}"));
        }
        Ok(())
    }

    // ---- Tabs ----

    /// The page targets the browser lists, in its order.
    async fn list_targets(&mut self) -> Result<Vec<Listed>, String> {
        let reply = self
            .cdp
            .call(REQUEST_TIMEOUT, None, "Target.getTargets", json!({}))
            .await?;
        let mut listed = tabs::listed(&reply["targetInfos"]);
        listed.retain(|tab| !self.ignored.contains(&tab.id));
        Ok(listed)
    }

    /// Brings the tab list in line with what the browser has: tabs a page closed are dropped,
    /// tabs a page opened (a link to a new window) are added up to [`MAX_TABS`] and the rest
    /// closed, and if the active tab is gone another becomes the active one (a blank one if
    /// none is left). Returns what the browser listed.
    async fn sync_tabs(&mut self) -> Result<Vec<Listed>, String> {
        let listed = self.list_targets().await?;
        let changes = tabs::reconcile(&mut self.tabs, &mut self.next_tab, &listed, MAX_TABS);
        self.tab_urls = self
            .tabs
            .iter()
            .filter_map(|rec| {
                let page = listed.iter().find(|page| page.id == rec.target)?;
                Some((rec.label, page.url.clone()))
            })
            .collect();
        for target in &changes.excess {
            self.close_target(target).await;
        }
        for (label, target) in &changes.opened {
            let url = listed
                .iter()
                .find(|tab| tab.id == *target)
                .map(|tab| tab.url.as_str())
                .unwrap_or_default();
            self.opened_by_pages.push(format!(
                "a page opened tab {label} ({}); switch to it with browser_switch to see it",
                tabs::one_line(url, 120)
            ));
            if self.opened_by_pages.len() > 10 {
                self.opened_by_pages.remove(0);
            }
        }
        if !self.tabs.iter().any(|tab| tab.label == self.active) {
            match self.tabs.first().map(|tab| tab.label) {
                Some(label) => self.attach(label).await?,
                None => {
                    self.create_tab().await?;
                }
            }
        }
        Ok(listed)
    }

    fn tab_infos(&self, listed: &[Listed], url: &str, title: &str) -> Vec<TabInfo> {
        self.tabs
            .iter()
            .map(|rec| {
                let active = rec.label == self.active;
                let found = listed.iter().find(|tab| tab.id == rec.target);
                TabInfo {
                    id: rec.label.to_string(),
                    url: if active {
                        url.to_string()
                    } else {
                        found.map(|tab| tab.url.clone()).unwrap_or_default()
                    },
                    title: if active {
                        title.to_string()
                    } else {
                        found.map(|tab| tab.title.clone()).unwrap_or_default()
                    },
                    active,
                }
            })
            .collect()
    }

    /// Every open tab with its URL and title.
    pub async fn tabs(&mut self) -> Result<Vec<TabInfo>, String> {
        let listed = self.sync_tabs().await?;
        let (url, title) = listed
            .iter()
            .find(|tab| {
                self.tabs
                    .iter()
                    .any(|rec| rec.label == self.active && rec.target == tab.id)
            })
            .map(|tab| (tab.url.clone(), tab.title.clone()))
            .unwrap_or_default();
        Ok(self.tab_infos(&listed, &url, &title))
    }

    /// The label a tab id names, if such a tab is open.
    fn label_of(&self, id: &str) -> Result<u32, String> {
        id.trim()
            .parse::<u32>()
            .ok()
            .filter(|label| self.tabs.iter().any(|tab| tab.label == *label))
            .ok_or_else(|| {
                let open: Vec<String> = self.tabs.iter().map(|tab| tab.label.to_string()).collect();
                format!(
                    "there is no tab {id:?}; the open tabs are {}",
                    open.join(", ")
                )
            })
    }

    /// Opens a blank tab and makes it the active one.
    async fn create_tab(&mut self) -> Result<u32, String> {
        if self.tabs.len() >= MAX_TABS {
            return Err(format!(
                "{MAX_TABS} tabs are open, the most allowed; close one first"
            ));
        }
        let created = self
            .cdp
            .call(
                LOAD_TIMEOUT,
                None,
                "Target.createTarget",
                json!({"url": "about:blank"}),
            )
            .await?;
        let id = created["targetId"]
            .as_str()
            .filter(|id| tabs::is_target_id(id))
            .ok_or("the browser opened a tab without a usable id")?
            .to_string();
        let label = self.next_tab;
        self.next_tab += 1;
        self.tabs.push(TabRec { label, target: id });
        self.attach(label).await?;
        Ok(label)
    }

    /// Attaches to a tab and makes it the active one.
    async fn attach(&mut self, label: u32) -> Result<(), String> {
        self.attach_within(label, LOAD_TIMEOUT, true).await
    }

    /// `attach`, with `limit` for each answer, bringing the tab to the front with `front`.
    async fn attach_within(
        &mut self,
        label: u32,
        limit: Duration,
        front: bool,
    ) -> Result<(), String> {
        let target = self
            .tabs
            .iter()
            .find(|tab| tab.label == label)
            .map(|tab| tab.target.clone())
            .ok_or("that tab is not open")?;
        let attached = self
            .cdp
            .call(
                limit,
                None,
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
            )
            .await
            .map_err(|error| format!("could not connect to the tab: {error}"))?;
        let session = attached["sessionId"]
            .as_str()
            .filter(|session| !session.is_empty())
            .ok_or("the browser attached to the tab without a session")?
            .to_string();
        let previous = std::mem::replace(&mut self.session, session);
        self.active = label;
        if !previous.is_empty() && previous != self.session {
            // Its events are of no use any more; a tab already closed has no session to end.
            let _ = self
                .cdp
                .call(
                    REQUEST_TIMEOUT,
                    None,
                    "Target.detachFromTarget",
                    json!({"sessionId": previous}),
                )
                .await;
        }
        self.call_within(limit, "Page.enable", json!({})).await?;
        if front {
            // A tab in the background draws no frames, so a screenshot of it would wait forever.
            self.bring_to_front(Duration::from_secs(5)).await?;
        }
        Ok(())
    }

    /// Makes an open tab the active one.
    pub async fn switch_tab(&mut self, id: &str) -> Result<(), String> {
        self.sync_tabs().await?;
        let label = self.label_of(id)?;
        if label != self.active {
            self.attach(label).await?;
            // A page still loading (a window a page just opened) is waited for, within limits.
            let _ = self.wait_until_loaded().await;
        }
        Ok(())
    }

    /// Closes a tab; the last one cannot be closed. If it was the active one, the tab before it
    /// (or the first) becomes the active one.
    pub async fn close_tab(&mut self, id: &str) -> Result<(), String> {
        self.sync_tabs().await?;
        let label = self.label_of(id)?;
        if self.tabs.len() == 1 {
            return Err("the last tab cannot be closed; open another page in it instead".into());
        }
        let position = self
            .tabs
            .iter()
            .position(|tab| tab.label == label)
            .ok_or("that tab is not open")?;
        let rec = self.tabs.remove(position);
        self.close_target(&rec.target).await;
        if label == self.active {
            let next = self.tabs[position.saturating_sub(1).min(self.tabs.len() - 1)].label;
            self.attach(next).await?;
        }
        // Wait until the browser has let go of it, so a list right after does not show it.
        for _ in 0..30 {
            let gone = self
                .list_targets()
                .await
                .map(|listed| listed.iter().all(|tab| tab.id != rec.target))
                .unwrap_or(true);
            if gone {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// Asks the browser to close a page (failing to is harmless).
    async fn close_target(&mut self, target: &str) {
        if tabs::is_target_id(target) {
            let _ = self
                .cdp
                .call(
                    REQUEST_TIMEOUT,
                    None,
                    "Target.closeTarget",
                    json!({"targetId": target}),
                )
                .await;
        }
    }

    // ---- Downloads ----

    /// Tells the browser to refuse every download and to report each one it refuses. The
    /// setting lasts while the debugging pipe is open, which is the browser's whole life.
    async fn refuse_downloads(&mut self) -> Result<(), String> {
        self.cdp
            .call(
                LOAD_TIMEOUT,
                None,
                "Browser.setDownloadBehavior",
                json!({"behavior": "deny", "eventsEnabled": true}),
            )
            .await
            .map_err(|error| format!("could not make the browser refuse downloads: {error}"))?;
        Ok(())
    }

    /// Reads the downloads the browser refused since the last look, and returns the new ones.
    /// Reading stops after `DOWNLOAD_READ_LIMIT` in all, or `DOWNLOAD_EVENTS` messages, so a
    /// page that floods the connection cannot hold a call up. A pipe that closes is remembered
    /// ([`Browser::refusal_lost`]): the browser is no longer made to refuse downloads.
    async fn collect_downloads(&mut self) -> Vec<PendingDownload> {
        self.cdp.drain().await;
        let mut fresh = Vec::new();
        for params in std::mem::take(&mut self.cdp.downloads) {
            let url = params["url"].as_str().unwrap_or_default().to_string();
            if url.is_empty() {
                continue;
            }
            let download = PendingDownload {
                url,
                filename: params["suggestedFilename"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            };
            fresh.push(download.clone());
            self.pending_downloads.push(download);
            if self.pending_downloads.len() > MAX_PENDING_DOWNLOADS {
                self.pending_downloads.remove(0);
            }
        }
        fresh
    }

    /// Whether the browser may no longer be trusted not to save files by itself: its debugging
    /// pipe, on which it was told to refuse downloads, closed. The owner drops it and starts a
    /// new browser.
    pub fn refusal_lost(&self) -> bool {
        self.cdp.closed
    }

    /// The last URL the browser listed for tab `id`, if that tab is known.
    pub fn tab_url(&self, id: &str) -> Option<String> {
        let label = id.trim().parse::<u32>().ok()?;
        self.tab_urls
            .iter()
            .find(|(known, _)| *known == label)
            .map(|(_, url)| url.clone())
    }

    /// What happened in the browser that the agent did not ask for since the last look, as
    /// lines for the agent: downloads the browser refused and tabs pages opened. Page-controlled
    /// text in them is flattened to one line and capped.
    pub async fn notices(&mut self) -> Vec<String> {
        let fresh = self.collect_downloads().await;
        // Pages may have opened tabs since the last look.
        let _ = self.sync_tabs().await;
        let mut lines = std::mem::take(&mut self.opened_by_pages);
        for download in &fresh {
            lines.push(format!(
                "the page tried to download a file and the browser refused it: {} (suggested name {:?}); \
                 save it with browser_download if the user wants it",
                tabs::one_line(&download.url, 200),
                tabs::one_line(&download.filename, 80)
            ));
        }
        lines
    }

    /// The download the page started most recently, if any is waiting.
    pub fn last_download(&self) -> Option<PendingDownload> {
        self.pending_downloads.last().cloned()
    }

    /// Takes a download off the waiting list once it is saved.
    pub fn forget_download(&mut self, url: &str) {
        self.pending_downloads
            .retain(|download| download.url != url);
    }

    /// The name the page suggested for a download of `url`, if it started one.
    pub fn suggested_name(&self, url: &str) -> Option<String> {
        self.pending_downloads
            .iter()
            .rev()
            .find(|download| download.url == url)
            .map(|download| download.filename.clone())
            .filter(|name| !name.is_empty())
    }

    /// The page's title, URL, and visible text.
    pub async fn read(&mut self) -> Result<String, String> {
        let text = self
            .evaluate("document.title + '\\nURL: ' + location.href + '\\n\\n' + (document.body ? document.body.innerText : '')")
            .await?;
        Ok(text
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(MAX_TEXT)
            .collect())
    }

    /// Clicks the first element matching a CSS selector.
    pub async fn click(&mut self, selector: &str) -> Result<String, String> {
        let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
        let outcome = self
            .evaluate_as(
                &format!(
                    "(() => {{ const el = document.querySelector({selector}); if (!el) return 'no element matches'; \
                     el.scrollIntoView({{block: 'center'}}); el.click(); return 'clicked'; }})()"
                ),
                true,
            )
            .await?;
        let outcome = outcome.as_str().unwrap_or_default().to_string();
        if outcome != "clicked" {
            return Err(format!("{outcome} {selector}"));
        }
        self.wait_until_loaded().await?;
        Ok(outcome)
    }

    /// Types into the first element matching a CSS selector, optionally submitting its form.
    pub async fn type_text(
        &mut self,
        selector: &str,
        text: &str,
        submit: bool,
    ) -> Result<String, String> {
        let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
        let value = serde_json::to_string(text).map_err(|error| error.to_string())?;
        let outcome = self
            .evaluate(&format!(
                "(() => {{ const el = document.querySelector({selector}); if (!el) return 'no element matches'; \
                 el.focus(); el.value = {value}; \
                 el.dispatchEvent(new Event('input', {{bubbles: true}})); \
                 el.dispatchEvent(new Event('change', {{bubbles: true}})); \
                 if ({submit} && el.form) {{ el.form.requestSubmit ? el.form.requestSubmit() : el.form.submit(); }} \
                 return 'typed'; }})()"
            ))
            .await?;
        let outcome = outcome.as_str().unwrap_or_default().to_string();
        if outcome != "typed" {
            return Err(format!("{outcome} {selector}"));
        }
        self.wait_until_loaded().await?;
        Ok(outcome)
    }

    /// Does what the user did (`session.browser_act`): navigates, goes back or forward, reloads,
    /// clicks at a fraction of the visible page, types into the focused element, presses a key,
    /// or scrolls; then waits for the page to settle.
    pub async fn act(&mut self, action: &BrowserAction) -> Result<(), String> {
        match action {
            BrowserAction::Navigate { url } => {
                let url = http_url(url)?;
                self.navigate(&url).await?;
            }
            BrowserAction::NewTab { url } => {
                let url = url
                    .as_deref()
                    .filter(|url| !url.trim().is_empty())
                    .map(http_url)
                    .transpose()?;
                self.sync_tabs().await?;
                self.create_tab().await?;
                if let Some(url) = url {
                    self.navigate(&url).await?;
                }
            }
            BrowserAction::SwitchTab { tab } => self.switch_tab(tab).await?,
            BrowserAction::CloseTab { tab } => self.close_tab(tab).await?,
            BrowserAction::Back => self.go_in_history(-1).await?,
            BrowserAction::Forward => self.go_in_history(1).await?,
            BrowserAction::Reload => {
                self.call("Page.reload", json!({})).await?;
            }
            BrowserAction::Click { x, y } => {
                let size = self
                    .evaluate("JSON.stringify([innerWidth, innerHeight])")
                    .await?;
                let [width, height]: [f64; 2] =
                    serde_json::from_str(size.as_str().unwrap_or("[0,0]"))
                        .map_err(|error| error.to_string())?;
                let (x, y) = (x.clamp(0.0, 1.0) * width, y.clamp(0.0, 1.0) * height);
                for kind in ["mousePressed", "mouseReleased"] {
                    self.call(
                        "Input.dispatchMouseEvent",
                        json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
                    )
                    .await?;
                }
            }
            BrowserAction::Type { text } => {
                self.call("Input.insertText", json!({"text": text})).await?;
            }
            BrowserAction::Key { key } => {
                let (code, virtual_key, text) = key_parts(key)
                    .ok_or_else(|| format!("{key} is not a key the browser can press"))?;
                for kind in ["keyDown", "keyUp"] {
                    let mut params = json!({
                        "type": kind, "key": key, "code": code,
                        "windowsVirtualKeyCode": virtual_key,
                    });
                    if kind == "keyDown" && !text.is_empty() {
                        params["text"] = json!(text);
                    }
                    self.call("Input.dispatchKeyEvent", params).await?;
                }
            }
            BrowserAction::Scroll { dy } => {
                let dy = if dy.is_finite() { *dy } else { 0.0 };
                self.evaluate(&format!("window.scrollBy(0, {dy})")).await?;
            }
        }
        // A navigation the action started may not have replaced the old document yet.
        tokio::time::sleep(Duration::from_millis(250)).await;
        self.wait_until_loaded().await
    }

    /// Moves `step` entries through the page's history; a page at the end of it stays put.
    async fn go_in_history(&mut self, step: i64) -> Result<(), String> {
        let history = self.call("Page.getNavigationHistory", json!({})).await?;
        let current = history["currentIndex"].as_i64().unwrap_or(0);
        let entry = history["entries"]
            .as_array()
            .and_then(|entries| entries.get(usize::try_from(current + step).ok()?))
            .and_then(|entry| entry["id"].as_i64());
        if let Some(id) = entry {
            self.call("Page.navigateToHistoryEntry", json!({"entryId": id}))
                .await?;
        }
        Ok(())
    }
}

/// Sets the profile's download preferences (`Default/Preferences`) so that a download the
/// debugger's refusal missed would go to an empty folder under the profile, and only after a
/// prompt. Existing preferences are kept.
fn set_download_prefs(profile_dir: &Path) -> Result<(), String> {
    let folder = profile_dir.join("blocked-downloads");
    std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    let default = profile_dir.join("Default");
    std::fs::create_dir_all(&default).map_err(|error| error.to_string())?;
    let path = default.join("Preferences");
    let mut prefs: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    for section in ["download", "savefile"] {
        if !prefs[section].is_object() {
            prefs[section] = json!({});
        }
    }
    let folder = folder.display().to_string();
    prefs["download"]["default_directory"] = json!(folder);
    prefs["download"]["prompt_for_download"] = json!(true);
    prefs["download"]["directory_upgrade"] = json!(true);
    prefs["savefile"]["default_directory"] = json!(folder);
    std::fs::write(&path, prefs.to_string()).map_err(|error| error.to_string())
}

/// The browser for a test that needs one: where it is installed; where it is not, a skip with a
/// note, except on CI (the `CI` variable is set), where the runners have one and its absence
/// means the test would otherwise pass without testing anything.
#[doc(hidden)]
pub fn browser_for_tests() -> Option<PathBuf> {
    let found = find_browser();
    if found.is_none() {
        if std::env::var_os("CI").is_some() {
            panic!("no Chrome, Chromium, or Edge found on CI; the browser tests would not run");
        }
        eprintln!("no Chrome, Chromium, or Edge found; skipping");
    }
    found
}

/// Answers every request with an empty page, for the warm-up's throwaway page; ends when the
/// task is aborted. Each request is read up to its blank line, for at most 5 s.
async fn serve_blank(listener: tokio::net::TcpListener) {
    // The connections' tasks are this task's: aborting it ends them with the listener.
    let mut connections = tokio::task::JoinSet::new();
    while let Ok((mut stream, _)) = listener.accept().await {
        // Finished connections are let go of as new ones come.
        while connections.try_join_next().is_some() {}
        connections.spawn(async move {
            let mut head = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 16 * 1024 {
                match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                    Ok(Ok(read)) if read > 0 => head.extend_from_slice(&chunk[..read]),
                    _ => return,
                }
            }
            let _ = stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 0\r\n\
                      Connection: close\r\n\r\n",
                )
                .await;
        });
    }
}

/// A task that is aborted when this is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Waits for the browser to list its first page and returns that page's target id. The
/// debugger can answer after the process starts, and the page after the debugger (slow starts,
/// notably Edge on Windows), so an unanswered request or an empty list is retried until
/// `deadline`, unless `exited` reports the browser is gone or it closes its pipe.
async fn wait_for_page(
    cdp: &mut Cdp,
    deadline: Instant,
    mut exited: impl FnMut() -> bool,
) -> Result<String, String> {
    let mut last_error = "the browser has no page to drive".to_string();
    while Instant::now() < deadline {
        if exited() {
            return Err(format!(
                "the browser exited before it answered on its debugging pipe ({last_error})"
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match cdp
            .call(
                REQUEST_TIMEOUT.min(remaining),
                None,
                "Target.getTargets",
                json!({}),
            )
            .await
        {
            Ok(reply) => {
                if let Some(page) = tabs::listed(&reply["targetInfos"]).into_iter().next() {
                    return Ok(page.id);
                }
            }
            Err(_) if cdp.closed => {
                return Err("the browser closed its debugging pipe while starting".into());
            }
            Err(error) => last_error = error,
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "the browser did not answer on its debugging pipe ({last_error})"
    ))
}

/// `url` when it is an http or https URL the browser can open.
fn http_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    match reqwest::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => Ok(parsed.to_string()),
        _ => Err(format!("{url} is not an http or https URL")),
    }
}

/// The DOM `code`, Windows virtual key code, and typed text of the keys a user can press.
fn key_parts(key: &str) -> Option<(&'static str, u32, &'static str)> {
    Some(match key {
        "Enter" => ("Enter", 13, "\r"),
        "Tab" => ("Tab", 9, ""),
        "Backspace" => ("Backspace", 8, ""),
        "Delete" => ("Delete", 46, ""),
        "Escape" => ("Escape", 27, ""),
        "ArrowUp" => ("ArrowUp", 38, ""),
        "ArrowDown" => ("ArrowDown", 40, ""),
        "ArrowLeft" => ("ArrowLeft", 37, ""),
        "ArrowRight" => ("ArrowRight", 39, ""),
        "Home" => ("Home", 36, ""),
        "End" => ("End", 35, ""),
        "PageUp" => ("PageUp", 33, ""),
        "PageDown" => ("PageDown", 34, ""),
        _ => return None,
    })
}

/// The last part of the browser's output, for error messages: at most `LOG_LINES` lines, with
/// the debugging endpoint hidden (see `redact`).
fn log_tail(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let tail = &bytes[bytes.len().saturating_sub(LOG_TAIL)..];
    let text = String::from_utf8_lossy(tail);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let lines = &lines[lines.len().saturating_sub(LOG_LINES)..];
    if lines.is_empty() {
        "none".into()
    } else {
        redact(&lines.join("\n"))
    }
}

/// `text` without the browser's debugging endpoint. Errors go to the model, and the address the
/// browser prints (`DevTools listening on ws://127.0.0.1:<port>/devtools/browser/<id>`) would let
/// it drive the browser directly, past the approvals and the refusal of `file:` URLs. Every word
/// naming a socket address or a debugger path is replaced.
fn redact(text: &str) -> String {
    text.lines()
        .map(|line| {
            line.split(' ')
                .map(|word| {
                    if names_endpoint(word) {
                        "[redacted]"
                    } else {
                        word
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn names_endpoint(word: &str) -> bool {
    [
        "ws://",
        "wss://",
        "/devtools/",
        "127.0.0.1:",
        "localhost:",
        "[::1]:",
    ]
    .iter()
    .any(|marker| word.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::routing::get;

    /// What a stand-in browser does with one command.
    enum Reply {
        /// Answers with this result.
        Answer(Value),
        /// Sends this event first, then answers with the result.
        EventThen(Value, Value),
        /// Answers with a protocol error.
        Refuse(&'static str),
        /// Answers with the result, then closes the pipe.
        AnswerThenClose(Value),
        /// Says nothing.
        Silent,
    }

    /// Each command a stand-in was sent, as "method session target-or-url", the session being
    /// `browser` for a browser-level command.
    type Seen = Arc<Mutex<Vec<String>>>;

    /// A pipe whose other end is a stand-in browser: `answer` decides each reply, and with
    /// `chatty` an event is sent every 50 ms as well.
    fn mock_pipe(
        chatty: bool,
        mut answer: impl FnMut(&Value) -> Reply + Send + 'static,
    ) -> (Pipe, Seen) {
        let (pipe, mut remote) = Pipe::pair();
        let seen = Seen::default();
        let noted = Arc::clone(&seen);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            let send = |value: Value| value.to_string().into_bytes();
            loop {
                tokio::select! {
                    command = remote.commands.recv() => {
                        let Some(command) = command else { break };
                        let command: Value = serde_json::from_slice(&command).unwrap();
                        let params = &command["params"];
                        noted.lock().unwrap().push(format!(
                            "{} {} {}",
                            command["method"].as_str().unwrap_or_default(),
                            command["sessionId"].as_str().unwrap_or("browser"),
                            params["targetId"].as_str().or(params["url"].as_str()).unwrap_or_default()
                        ).trim_end().to_string());
                        let id = command["id"].clone();
                        let (before, reply, close) = match answer(&command) {
                            Reply::Answer(result) => (None, Some(json!({"id": id, "result": result})), false),
                            Reply::EventThen(event, result) => {
                                (Some(event), Some(json!({"id": id, "result": result})), false)
                            }
                            Reply::Refuse(message) => {
                                (None, Some(json!({"id": id, "error": {"message": message}})), false)
                            }
                            Reply::AnswerThenClose(result) => {
                                (None, Some(json!({"id": id, "result": result})), true)
                            }
                            Reply::Silent => (None, None, false),
                        };
                        for message in before.into_iter().chain(reply) {
                            if remote.replies.send(send(message)).await.is_err() {
                                return;
                            }
                        }
                        if close {
                            return;
                        }
                    }
                    _ = tick.tick(), if chatty => {
                        let event = json!({"method": "Page.frameStartedLoading", "params": {}});
                        if remote.replies.send(send(event)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        (pipe, seen)
    }

    /// A stand-in that answers like a browser with one page, `T0`: new pages are `T1`, `T2`, …,
    /// attaching to target `Tn` gives session `Sn`, and every other command succeeds.
    fn answering(command: &Value) -> Reply {
        let params = &command["params"];
        match command["method"].as_str().unwrap_or_default() {
            "Target.createTarget" => Reply::Answer(json!({"targetId": "T1"})),
            "Target.attachToTarget" => {
                let target = params["targetId"].as_str().unwrap_or_default();
                Reply::Answer(json!({"sessionId": target.replacen('T', "S", 1)}))
            }
            "Target.getTargets" => Reply::Answer(json!({"targetInfos": [
                {"targetId": "T0", "type": "page", "url": "about:blank", "title": ""}
            ]})),
            _ => Reply::Answer(json!({})),
        }
    }

    /// A `Browser` talking to a stand-in instead of a process, driving the page `T0` through
    /// session `S0`, with `log` as the browser's output, and what the stand-in was sent.
    fn mock_browser(
        log: &str,
        chatty: bool,
        answer: impl FnMut(&Value) -> Reply + Send + 'static,
    ) -> (Browser, tempfile::TempDir, Seen) {
        let (pipe, seen) = mock_pipe(chatty, answer);
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("imperium-browser.log");
        std::fs::write(&log_path, log).unwrap();
        let mut browser = Browser::assemble(Cdp::new(pipe, log_path), None, "T0".into());
        browser.session = "S0".into();
        (browser, dir, seen)
    }

    fn lines(seen: &Seen, keep: impl Fn(&str) -> bool) -> Vec<String> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|line| keep(line.as_str()))
            .cloned()
            .collect()
    }

    const LISTENING: &str =
        "DevTools listening on ws://127.0.0.1:9222/devtools/browser/0a1b2c3d-secret";

    #[test]
    fn the_debugger_is_on_the_pipe_never_a_port_and_webrtc_sends_no_direct_udp() {
        let profile = Path::new("/data/browser");
        for headless in [true, false] {
            let args: Vec<String> = launch_args(profile, headless)
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(
                args.iter().any(|arg| arg == "--remote-debugging-pipe"),
                "{args:?}"
            );
            assert!(
                !args
                    .iter()
                    .any(|arg| arg.starts_with("--remote-debugging-port")
                        || arg.starts_with("--remote-debugging-address")),
                "{args:?}"
            );
            assert!(
                args.iter()
                    .any(|arg| arg == "--force-webrtc-ip-handling-policy=disable_non_proxied_udp"),
                "{args:?}"
            );
            assert_eq!(
                args.iter().any(|arg| arg == "--headless=new"),
                headless,
                "{args:?}"
            );
            assert!(args.contains(&format!("--user-data-dir={}", profile.display())));
            assert_eq!(args.last().map(String::as_str), Some("about:blank"));
        }
    }

    #[tokio::test]
    async fn a_call_the_browser_never_answers_times_out_with_its_output_but_not_its_endpoint() {
        let log = format!("{LISTENING}\nboom happened\n");
        let (mut browser, _dir, _) = mock_browser(&log, false, |_| Reply::Silent);
        let started = Instant::now();
        let error = browser
            .call_within(Duration::from_millis(200), "Page.navigate", json!({}))
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(error.starts_with("Page.navigate timed out"), "{error}");
        assert!(error.contains("boom happened"), "{error}");
        assert!(
            !error.contains("ws://") && !error.contains("secret"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn commands_go_to_the_active_tabs_session_and_tab_commands_to_the_browser() {
        let (mut browser, _dir, seen) = mock_browser("", false, answering);
        browser.call("Page.reload", json!({})).await.unwrap();
        browser.create_tab().await.unwrap();
        browser.call("Page.reload", json!({})).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "Page.reload S0",
                "Target.createTarget browser about:blank",
                "Target.attachToTarget browser T1",
                // The old tab's session is let go once the new one is attached.
                "Target.detachFromTarget browser",
                "Page.enable S1",
                "Page.bringToFront S1",
                "Page.reload S1",
            ]
        );
        assert_eq!(browser.active, 2);
    }

    #[tokio::test]
    async fn the_warm_up_limit_is_on_the_whole_wait_not_on_silence() {
        // Events every 50 ms keep a silence timer from ever firing; the limit must still hold.
        let (mut browser, _dir, _) = mock_browser(LISTENING, true, |_| Reply::Silent);
        let started = Instant::now();
        let error = browser
            .warm_up_within(Duration::from_millis(400))
            .await
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(error.contains("did not finish starting"), "{error}");
        assert!(
            !error.contains("ws://") && !error.contains("secret"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_warm_up_that_times_out_stops_serving_its_page() {
        // The throwaway page is opened and attached, then its load never answers; events keep
        // the connection busy, so only the warm-up's own limit ends it.
        let (mut browser, _dir, seen) = mock_browser("", true, |command| {
            if command["method"] == "Page.navigate" {
                Reply::Silent
            } else {
                answering(command)
            }
        });
        let error = browser
            .warm_up_within(Duration::from_millis(400))
            .await
            .unwrap_err();
        assert!(error.contains("did not finish starting"), "{error}");
        let navigations = lines(&seen, |line| line.starts_with("Page.navigate"));
        let address = navigations
            .first()
            .and_then(|line| line.split("http://").nth(1))
            .map(|rest| rest.trim_end_matches('/').to_string())
            .unwrap_or_else(|| panic!("no warm-up navigation in {navigations:?}"));
        // The abort lands at the server task's next turn.
        let mut refused = false;
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(&address).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(refused, "the warm-up page is still served at {address}");
    }

    #[tokio::test]
    async fn a_refused_warm_up_navigation_is_not_a_failure() {
        let (mut browser, _dir, seen) = mock_browser("", false, |command| {
            if command["method"] == "Page.navigate" {
                Reply::Refuse("Cannot navigate")
            } else {
                answering(command)
            }
        });
        browser
            .warm_up_within(Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(
            lines(&seen, |line| line.starts_with("Page.navigate")).len(),
            1
        );
    }

    #[tokio::test]
    async fn the_warm_up_uses_a_throwaway_page_and_leaves_the_driven_one_alone() {
        let (mut browser, _dir, seen) = mock_browser("", false, answering);
        browser
            .warm_up_within(Duration::from_secs(10))
            .await
            .unwrap();
        assert!(browser.warmed());
        // The page being driven was navigated nowhere, so its history holds nothing of the
        // warm-up; it was only brought to the front.
        assert_eq!(
            lines(&seen, |line| line.contains(" S0")),
            ["Page.bringToFront S0"]
        );
        // The throwaway page loaded a page served on loopback for the moment, and was closed.
        let navigations = lines(&seen, |line| line.starts_with("Page.navigate"));
        assert_eq!(navigations.len(), 1, "{navigations:?}");
        assert!(
            navigations[0].starts_with("Page.navigate S1 http://127.0.0.1:"),
            "{navigations:?}"
        );
        assert_eq!(
            lines(&seen, |line| line.starts_with("Target.closeTarget")),
            ["Target.closeTarget browser T1"]
        );
        assert_eq!(browser.ignored, ["T1"]);
    }

    #[tokio::test]
    async fn a_browser_that_cannot_open_a_page_does_not_fail_the_start() {
        let (mut browser, _dir, seen) = mock_browser("", false, |command| {
            if command["method"] == "Target.createTarget" {
                Reply::Refuse("Not supported")
            } else {
                answering(command)
            }
        });
        browser
            .warm_up_within(Duration::from_secs(10))
            .await
            .unwrap();
        assert!(lines(&seen, |line| line.contains(" S0")).is_empty());
        assert!(!browser.warmed());
    }

    #[tokio::test]
    async fn a_browser_that_never_answers_is_given_up_on_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let (pipe, _) = mock_pipe(false, |_| Reply::Silent);
        let mut cdp = Cdp::new(pipe, dir.path().join("log"));
        let started = Instant::now();
        let deadline = started + Duration::from_secs(3);
        let error = wait_for_page(&mut cdp, deadline, || false)
            .await
            .unwrap_err();
        assert!(error.contains("debugging pipe"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(6));
    }

    #[tokio::test]
    async fn the_first_page_the_browser_lists_is_the_one_driven() {
        let dir = tempfile::tempdir().unwrap();
        let mut asked = 0;
        let (pipe, _) = mock_pipe(false, move |_| {
            asked += 1;
            // No page yet the first time; then a worker and the page.
            if asked == 1 {
                Reply::Answer(json!({"targetInfos": []}))
            } else {
                Reply::Answer(json!({"targetInfos": [
                    {"targetId": "W1", "type": "service_worker", "url": "https://a/sw.js"},
                    {"targetId": "P1", "type": "page", "url": "about:blank", "title": ""}
                ]}))
            }
        });
        let mut cdp = Cdp::new(pipe, dir.path().join("log"));
        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(
            wait_for_page(&mut cdp, deadline, || false).await.unwrap(),
            "P1"
        );
    }

    #[tokio::test]
    async fn a_browser_that_closes_its_pipe_while_starting_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (pipe, remote) = Pipe::pair();
        drop(remote);
        let mut cdp = Cdp::new(pipe, dir.path().join("log"));
        let started = Instant::now();
        let deadline = started + Duration::from_secs(30);
        let error = wait_for_page(&mut cdp, deadline, || false)
            .await
            .unwrap_err();
        assert!(error.contains("closed its debugging pipe"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_download_reported_while_another_answer_was_awaited_is_not_lost() {
        let (mut browser, _dir, _) = mock_browser("", false, |command| {
            if command["method"] == "Runtime.evaluate" {
                Reply::EventThen(
                    json!({"method": "Browser.downloadWillBegin", "params": {
                        "url": "https://example.com/report.bin",
                        "suggestedFilename": "report.bin"
                    }}),
                    json!({"result": {"value": "complete"}}),
                )
            } else {
                answering(command)
            }
        });
        browser.wait_until_loaded().await.unwrap();
        let fresh = browser.collect_downloads().await;
        assert_eq!(
            fresh,
            [PendingDownload {
                url: "https://example.com/report.bin".into(),
                filename: "report.bin".into(),
            }]
        );
        assert!(!browser.refusal_lost());
        assert!(browser.collect_downloads().await.is_empty());
    }

    #[tokio::test]
    async fn a_browser_that_will_not_refuse_downloads_does_not_start() {
        let (mut browser, _dir, _) = mock_browser("", false, |_| Reply::Refuse("Not allowed"));
        let error = browser.refuse_downloads().await.unwrap_err();
        assert!(
            error.contains("could not make the browser refuse downloads"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_lost_download_refusal_is_remembered_so_the_browser_is_replaced() {
        let (mut browser, _dir, _) = mock_browser("", false, |_| Reply::AnswerThenClose(json!({})));
        browser.refuse_downloads().await.unwrap();
        assert!(!browser.refusal_lost());
        // The stand-in closed the pipe after its one answer.
        browser.collect_downloads().await;
        assert!(browser.refusal_lost());
    }

    /// The process ids listening on a TCP port, IPv4 or IPv6, as the operating system's own tool
    /// reports them. Panics when the tool cannot be run: the test must not pass unseen.
    fn listening_pids() -> Vec<u32> {
        let run = |program: &str, args: &[&str]| {
            let mut command = std::process::Command::new(program);
            command.args(args);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            let output = command
                .output()
                .unwrap_or_else(|error| panic!("could not run {program}: {error}"));
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        if cfg!(windows) {
            // "  TCP    127.0.0.1:9222    0.0.0.0:0    LISTENING    1234"
            run("netstat", &["-ano", "-p", "TCP"])
                .lines()
                .chain(run("netstat", &["-ano", "-p", "TCPv6"]).lines())
                .filter(|line| line.contains("LISTENING"))
                .filter_map(|line| line.split_whitespace().last()?.parse().ok())
                .collect()
        } else if cfg!(target_os = "macos") {
            // `-F p`: one "p<pid>" line per process.
            run("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "p"])
                .lines()
                .filter_map(|line| line.strip_prefix('p')?.parse().ok())
                .collect()
        } else {
            // users:(("chrome",pid=123,fd=45))
            run("ss", &["-Hltnp"])
                .split("pid=")
                .skip(1)
                .filter_map(|rest| {
                    rest.split(|c: char| !c.is_ascii_digit())
                        .next()?
                        .parse()
                        .ok()
                })
                .collect()
        }
    }

    /// `root` and every process descended from it.
    fn process_tree(root: u32) -> Vec<u32> {
        let listing = if cfg!(windows) {
            let mut command = std::process::Command::new("powershell");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId) $($_.ParentProcessId)\" }",
            ]);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            command.output()
        } else {
            std::process::Command::new("ps")
                .args(["-A", "-o", "pid=", "-o", "ppid="])
                .output()
        }
        .expect("could not list the processes");
        let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
            })
            .collect();
        let mut tree = vec![root];
        let mut index = 0;
        while index < tree.len() {
            let parent = tree[index];
            for (pid, ppid) in &pairs {
                if *ppid == parent && *pid != parent && !tree.contains(pid) {
                    tree.push(*pid);
                }
            }
            index += 1;
        }
        tree
    }

    /// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
    #[tokio::test]
    async fn the_browser_listens_on_no_tcp_port() {
        let Some(program) = browser_for_tests() else {
            return;
        };
        // The listing sees a port this process holds, so an empty answer below means something.
        let own = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(
            listening_pids().contains(&std::process::id()),
            "the listing does not see this test's own port"
        );
        drop(own);

        let profile = tempfile::tempdir().unwrap();
        let mut browser = Browser::launch(&program, profile.path(), true)
            .await
            .unwrap();
        // It answers on its pipe, and has finished starting (the warm-up ran).
        assert!(browser.view().await.unwrap().screenshot.len() > 100);
        let tree = process_tree(browser.process_id().expect("the browser's process id"));
        let listening: Vec<u32> = listening_pids()
            .into_iter()
            .filter(|pid| tree.contains(pid))
            .collect();
        assert!(
            listening.is_empty(),
            "the browser's processes {listening:?} listen on a TCP port (of {tree:?})"
        );
        // A browser with a debugging port writes it here; this one has none to write.
        assert!(!profile.path().join("DevToolsActivePort").exists());
    }

    #[test]
    fn the_debugging_endpoint_is_hidden_and_the_tail_is_short() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut text = format!("{LISTENING}\n");
        for n in 0..30 {
            text.push_str(&format!("line {n}\n"));
        }
        text.push_str("error sending request for url (http://127.0.0.1:9222/json/list)\n");
        std::fs::write(&path, &text).unwrap();
        let tail = log_tail(&path);
        assert_eq!(tail.lines().count(), LOG_LINES, "{tail}");
        assert!(tail.contains("line 29"), "{tail}");
        assert!(!tail.contains("ws://") && !tail.contains("9222"), "{tail}");

        std::fs::write(&path, LISTENING).unwrap();
        let tail = log_tail(&path);
        assert_eq!(tail, "DevTools listening on [redacted]");
        assert!(!tail.contains("secret"));

        for hidden in [
            "ws://localhost:1/devtools/page/x",
            "wss://host/x",
            "http://localhost:5/json/list",
            "http://[::1]:5/json",
            "/devtools/browser/abc",
        ] {
            assert_eq!(
                redact(&format!("a {hidden} b")),
                "a [redacted] b",
                "{hidden}"
            );
        }
        assert_eq!(redact("plain words only"), "plain words only");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(log_tail(&path), "none");
    }

    /// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
    #[tokio::test]
    async fn the_browser_opens_reads_clicks_types_and_screenshots() {
        let Some(program) = browser_for_tests() else {
            return;
        };
        let page = r#"<html><head><title>Counter</title></head><body>
            <p id="count">0 clicks</p>
            <button id="add" onclick="document.getElementById('count').textContent = '1 click'">Add</button>
            <input id="name" oninput="document.title = 'Hello ' + this.value">
            </body></html>"#;
        let app = Router::new().route("/", get(move || async move { axum::response::Html(page) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let profile = tempfile::tempdir().unwrap();
        let mut browser = Browser::launch(&program, profile.path(), true)
            .await
            .unwrap();
        // A browser that stops opening pages with Target.createTarget would quietly skip the
        // warm-up.
        assert!(browser.warmed(), "the start-up warm-up did not run");
        let text = browser.open(&url, false).await.unwrap();
        assert!(text.starts_with("Counter\nURL: http://127.0.0.1"), "{text}");
        assert!(text.contains("0 clicks"), "{text}");
        browser.click("#add").await.unwrap();
        assert!(browser.read().await.unwrap().contains("1 click"));
        browser.type_text("#name", "Ada", false).await.unwrap();
        let view = browser.view().await.unwrap();
        assert_eq!(view.title, "Hello Ada");
        assert!(view.screenshot.len() > 100);
        assert!(
            browser
                .click("#missing")
                .await
                .unwrap_err()
                .contains("no element matches")
        );
        assert!(browser.open("file:///etc/passwd", false).await.is_err());
    }

    /// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
    #[tokio::test]
    async fn the_user_clicks_types_presses_keys_and_navigates() {
        let Some(program) = browser_for_tests() else {
            return;
        };
        // A page that is one big button, then a field to type in, then a second page.
        let first = r#"<html><head><title>First</title></head><body style="margin:0">
            <button style="position:fixed;inset:0;width:100%;height:100%"
              onclick="document.title = 'Clicked'">Big</button></body></html>"#;
        let second = r#"<html><head><title>Second</title></head><body>
            <input id="f" autofocus oninput="document.title = 'Typed ' + this.value"
              onkeydown="if (event.key === 'Enter') document.title += ' Enter'"></body></html>"#;
        let app = Router::new()
            .route("/", get(move || async move { axum::response::Html(first) }))
            .route(
                "/second",
                get(move || async move { axum::response::Html(second) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let profile = tempfile::tempdir().unwrap();
        let mut browser = Browser::launch(&program, profile.path(), true)
            .await
            .unwrap();
        let navigate = BrowserAction::Navigate {
            url: format!("{base}/"),
        };
        browser.act(&navigate).await.unwrap();
        browser
            .act(&BrowserAction::Click { x: 0.5, y: 0.5 })
            .await
            .unwrap();
        assert_eq!(browser.view().await.unwrap().title, "Clicked");

        let second_page = BrowserAction::Navigate {
            url: format!("{base}/second"),
        };
        browser.act(&second_page).await.unwrap();
        browser
            .evaluate("document.getElementById('f').focus()")
            .await
            .unwrap();
        browser
            .act(&BrowserAction::Type { text: "Ada".into() })
            .await
            .unwrap();
        browser
            .act(&BrowserAction::Key {
                key: "Enter".into(),
            })
            .await
            .unwrap();
        assert_eq!(browser.view().await.unwrap().title, "Typed Ada Enter");

        browser.act(&BrowserAction::Back).await.unwrap();
        assert!(browser.view().await.unwrap().url.ends_with('/'));
        browser.act(&BrowserAction::Forward).await.unwrap();
        assert!(browser.view().await.unwrap().url.ends_with("/second"));
        browser
            .act(&BrowserAction::Scroll { dy: 10.0 })
            .await
            .unwrap();
        browser.act(&BrowserAction::Reload).await.unwrap();

        let file = BrowserAction::Navigate {
            url: "file:///etc/passwd".into(),
        };
        assert!(browser.act(&file).await.is_err());
        let bad_key = BrowserAction::Key { key: "F13".into() };
        assert!(browser.act(&bad_key).await.is_err());
    }

    /// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
    #[tokio::test]
    async fn tabs_open_switch_list_close_and_are_bounded() {
        let Some(program) = browser_for_tests() else {
            return;
        };
        let page = |title: &'static str, body: &'static str| {
            get(move || async move {
                axum::response::Html(format!(
                    "<html><head><title>{title}</title></head><body>{body}</body></html>"
                ))
            })
        };
        let app = Router::new()
            .route(
                "/a",
                page("Page A", r#"<a id="pop" target="_blank" href="/c">pop</a>"#),
            )
            .route("/b", page("Page B", "second"))
            .route("/c", page("Page C", "popup"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let profile = tempfile::tempdir().unwrap();
        let mut browser = Browser::launch(&program, profile.path(), true)
            .await
            .unwrap();
        browser.open(&format!("{base}/a"), false).await.unwrap();
        let text = browser.open(&format!("{base}/b"), true).await.unwrap();
        assert!(text.starts_with("Page B\nURL:"), "{text}");

        let tabs = browser.tabs().await.unwrap();
        assert_eq!(tabs.len(), 2, "{tabs:?}");
        assert_eq!(
            (tabs[0].id.as_str(), tabs[0].title.as_str()),
            ("1", "Page A")
        );
        assert_eq!(
            (tabs[1].id.as_str(), tabs[1].title.as_str()),
            ("2", "Page B")
        );
        assert!(!tabs[0].active && tabs[1].active);

        // Each tab keeps its own page; the browser's own view follows the active one.
        browser.switch_tab("1").await.unwrap();
        let view = browser.view().await.unwrap();
        assert_eq!((view.tab.as_str(), view.title.as_str()), ("1", "Page A"));
        assert!(view.screenshot.len() > 100);
        assert_eq!(view.tabs.iter().filter(|tab| tab.active).count(), 1);
        assert!(browser.read().await.unwrap().contains("Page A"));
        assert!(
            browser
                .switch_tab("9")
                .await
                .unwrap_err()
                .contains("there is no tab")
        );

        // A page that opens a window gets a tab of its own, which the agent is told about but
        // is not switched to.
        browser.click("#pop").await.unwrap();
        let notices = browser.notices().await;
        assert!(
            notices.iter().any(|line| line.contains("opened tab 3")),
            "{notices:?}"
        );
        assert_eq!(browser.tabs().await.unwrap().len(), 3);
        assert_eq!(browser.view().await.unwrap().tab, "1");
        browser.switch_tab("3").await.unwrap();
        // The window the page opened may still be loading its page.
        let mut text = String::new();
        for _ in 0..100 {
            text = browser.read().await.unwrap();
            if text.contains("Page C") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(text.contains("Page C"), "{text}");

        // Closing the active tab hands over to a neighbour; the last tab stays.
        browser.close_tab("3").await.unwrap();
        let tabs = browser.tabs().await.unwrap();
        assert_eq!(tabs.len(), 2, "{tabs:?}");
        assert!(tabs.iter().any(|tab| tab.active));
        assert!(browser.read().await.is_ok());
        browser.close_tab("2").await.unwrap();
        assert!(
            browser
                .close_tab("1")
                .await
                .unwrap_err()
                .contains("last tab")
        );

        // The most tabs open at once is bounded.
        for _ in 1..MAX_TABS {
            browser.open(&format!("{base}/b"), true).await.unwrap();
        }
        assert_eq!(browser.tabs().await.unwrap().len(), MAX_TABS);
        let error = browser.open(&format!("{base}/b"), true).await.unwrap_err();
        assert!(error.contains("the most allowed"), "{error}");
    }

    /// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
    #[tokio::test]
    async fn the_browser_refuses_downloads_and_reports_them() {
        let Some(program) = browser_for_tests() else {
            return;
        };
        let page = r#"<html><head><title>Files</title></head><body>
            <a id="dl" href="/file.bin" download="report.bin">get</a></body></html>"#;
        let app = Router::new()
            .route("/", get(move || async move { axum::response::Html(page) }))
            .route(
                "/file.bin",
                get(|| async {
                    (
                        [
                            ("content-type", "application/octet-stream"),
                            ("content-disposition", "attachment; filename=\"report.bin\""),
                        ],
                        vec![1u8; 64],
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });

        let profile = tempfile::tempdir().unwrap();
        let mut browser = Browser::launch(&program, profile.path(), true)
            .await
            .unwrap();
        browser.open(&format!("{base}/"), false).await.unwrap();
        assert!(browser.last_download().is_none());
        browser.click("#dl").await.unwrap();
        let notices = browser.notices().await;
        assert!(
            notices
                .iter()
                .any(|line| line.contains("refused it") && line.contains("/file.bin")),
            "{notices:?}"
        );
        let pending = browser.last_download().expect("the download is remembered");
        assert_eq!(pending.url, format!("{base}/file.bin"));
        assert_eq!(pending.filename, "report.bin");
        assert_eq!(
            browser.suggested_name(&pending.url).as_deref(),
            Some("report.bin")
        );
        // The page is still there and the browser wrote nothing into its profile's downloads.
        assert!(browser.read().await.unwrap().contains("Files"));
        browser.forget_download(&pending.url);
        assert!(browser.last_download().is_none());
        // Told once.
        assert!(browser.notices().await.is_empty());
    }

    #[test]
    fn the_profile_sends_stray_downloads_to_an_empty_folder() {
        let profile = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(profile.path().join("Default")).unwrap();
        let path = profile.path().join("Default").join("Preferences");
        std::fs::write(&path, r#"{"keep":1,"download":{"other":true}}"#).unwrap();
        set_download_prefs(profile.path()).unwrap();
        let prefs: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(prefs["keep"], 1);
        assert_eq!(prefs["download"]["other"], true);
        assert_eq!(prefs["download"]["prompt_for_download"], true);
        let folder = prefs["download"]["default_directory"].as_str().unwrap();
        assert!(folder.ends_with("blocked-downloads"), "{folder}");
        assert_eq!(std::fs::read_dir(folder).unwrap().count(), 0);
        // A damaged file is replaced, not fatal.
        std::fs::write(&path, "not json").unwrap();
        set_download_prefs(profile.path()).unwrap();
    }
}
