//! A browser agents drive over the Chrome DevTools Protocol: Chrome, Chromium, or Edge,
//! started with its own profile under Imperium's data folder (never the user's), visible or
//! headless. After each action a screenshot is taken so the desktop app can mirror the page.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use imperium_protocol::methods::BrowserAction;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// From starting the browser to having a page to drive; cold starts on CI runners are slow.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest one request to the browser's debugger may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest the browser may take to answer its first request. Chrome holds that request back
/// until its own start-up work (a fresh profile registers dozens of components first) has
/// finished, which took 10 to 45 seconds on a Windows CI runner; it is waited out once, in
/// `launch`, so no later page load pays for it.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(90);
/// Longest page text returned by `read`.
const MAX_TEXT: usize = 20_000;
/// How much of the browser's output an error reports, in bytes and in lines.
const LOG_TAIL: usize = 2_000;
const LOG_LINES: usize = 10;

/// What the page looks like after an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub url: String,
    pub title: String,
    /// A JPEG screenshot, base64-encoded.
    pub screenshot: String,
}

pub struct Browser {
    log_path: PathBuf,
    _process: Option<Child>,
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: AtomicU64,
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

impl Browser {
    /// Starts `program` with a profile in `profile_dir` and connects to its first page.
    pub async fn launch(
        program: &Path,
        profile_dir: &Path,
        headless: bool,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(profile_dir).map_err(|error| error.to_string())?;
        let log_path = profile_dir.join("imperium-browser.log");
        let mut command = Command::new(program);
        let deadline = Instant::now() + START_TIMEOUT;
        // We choose the port ourselves and poll the debugger over HTTP: the port file the
        // browser writes for `--remote-debugging-port=0` is not reliably written on CI runners.
        let port = free_port()?;
        let log = std::fs::File::create(&log_path).map_err(|error| error.to_string())?;
        command
            .arg(format!("--remote-debugging-port={port}"))
            .arg("--remote-debugging-address=127.0.0.1")
            .arg(format!("--user-data-dir={}", profile_dir.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--window-size=1280,900",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(log))
            .kill_on_drop(true);
        if headless {
            command.arg("--headless=new");
        }
        command.arg("about:blank");
        let mut process = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;
        let waited = wait_for_page(port, profile_dir, deadline, || {
            matches!(process.try_wait(), Ok(Some(_)))
        })
        .await;
        let socket_url = match waited {
            Ok(url) => url,
            Err(reason) => {
                let status = match process.try_wait() {
                    Ok(Some(status)) => format!("the browser exited ({status})"),
                    Ok(None) => "the browser was still running".to_string(),
                    Err(error) => format!("the browser state is unknown ({error})"),
                };
                return Err(format!(
                    "{}; {status}; output: {}",
                    redact(&reason),
                    log_tail(&log_path)
                ));
            }
        };
        let (socket, _) = tokio_tungstenite::connect_async(socket_url.as_str())
            .await
            .map_err(|error| {
                format!(
                    "could not connect to the browser: {}",
                    redact(&error.to_string())
                )
            })?;
        let mut browser = Browser {
            log_path,
            _process: Some(process),
            socket,
            next_id: AtomicU64::new(1),
        };
        browser.call("Page.enable", json!({})).await?;
        browser.warm_up(port).await?;
        Ok(browser)
    }

    /// Waits until the browser answers a first request, for at most `WARM_UP_TIMEOUT` in all.
    /// A throwaway page loads the browser's own debugging page (nothing leaves the machine);
    /// that request is held back by Chrome's start-up work in the same way as the first page
    /// load, so doing it here, with a long limit, keeps that wait out of the first real
    /// `Page.navigate`, whose limit is short. The page being driven is not touched, so its
    /// history never holds the debugging page.
    async fn warm_up(&mut self, port: u16) -> Result<(), String> {
        self.warm_up_within(port, WARM_UP_TIMEOUT).await
    }

    /// `warm_up` with its limit chosen. The limit is on the whole wait; events the browser
    /// sends meanwhile do not extend it. Only a timeout or a closed browser fails the wait: any
    /// other answer (a navigation refused, a page replaced by another) means the request ended.
    async fn warm_up_within(&mut self, port: u16, limit: Duration) -> Result<(), String> {
        let started = Instant::now();
        let outcome = tokio::time::timeout(limit, async {
            let opened = Self::warm_up_navigations(port, limit, self.log_path.clone()).await?;
            if opened {
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
                    log_tail(&self.log_path)
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

    /// Opens, loads and closes the throwaway page; true when it was opened.
    async fn warm_up_navigations(
        port: u16,
        limit: Duration,
        log_path: PathBuf,
    ) -> Result<bool, String> {
        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();
        let created = match client
            .put(format!("{base}/json/new?about:blank"))
            .send()
            .await
        {
            Ok(response) => response
                .json::<Value>()
                .await
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        // Without a throwaway page there is nothing to wait on; the first navigation then
        // pays, as it did before.
        let target = match created {
            Ok(target) => target,
            Err(error) => {
                tracing::warn!(%error, "could not open a page to warm the browser up");
                return Ok(false);
            }
        };
        let Some(id) = target["id"].as_str().map(str::to_string) else {
            tracing::warn!("the browser opened a page without an id");
            return Ok(false);
        };
        let outcome = match page_socket(&json!([target]), port) {
            Ok(Some(socket_url)) => {
                Self::navigate_throwaway(&socket_url, &base, limit, log_path).await
            }
            Ok(None) => Ok(()),
            Err(error) => Err(error),
        };
        // Close it whatever happened (failing to is harmless) and wait until it is gone.
        let _ = client.get(format!("{base}/json/close/{id}")).send().await;
        for _ in 0..30 {
            let listed = match client.get(format!("{base}/json/list")).send().await {
                Ok(response) => response.json::<Value>().await.ok(),
                Err(_) => None,
            };
            let present = listed
                .as_ref()
                .and_then(Value::as_array)
                .is_some_and(|targets| targets.iter().any(|target| target["id"] == id.as_str()));
            if !present {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        outcome.map(|()| true)
    }

    async fn navigate_throwaway(
        socket_url: &str,
        base: &str,
        limit: Duration,
        log_path: PathBuf,
    ) -> Result<(), String> {
        let (socket, _) = tokio_tungstenite::connect_async(socket_url)
            .await
            .map_err(|error| format!("could not connect to the browser: {error}"))?;
        let mut page = Browser {
            log_path,
            _process: None,
            socket,
            next_id: AtomicU64::new(1),
        };
        let url = format!("{base}/json/version");
        match page
            .call_within(limit, "Page.navigate", json!({"url": url}))
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

    /// Sends a CDP command and waits for its result, skipping events in between.
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
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({"id": id, "method": method, "params": params});
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .map_err(|error| error.to_string())?;
        loop {
            let reply = tokio::time::timeout(limit, self.socket.next())
                .await
                .map_err(|_| {
                    format!(
                        "{method} timed out; browser output: {}",
                        log_tail(&self.log_path)
                    )
                })?
                .ok_or("the browser closed")?
                .map_err(|error| error.to_string())?;
            let Message::Text(text) = reply else { continue };
            let value: Value =
                serde_json::from_str(text.as_str()).map_err(|error| error.to_string())?;
            if value["id"].as_u64() != Some(id) {
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

    /// Evaluates a JavaScript expression and returns its value.
    async fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
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
        Ok(View {
            url,
            title,
            screenshot: shot["data"].as_str().unwrap_or_default().to_string(),
        })
    }

    pub async fn open(&mut self, url: &str) -> Result<String, String> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!("{url} is not an http or https URL"));
        }
        let result = self.call("Page.navigate", json!({"url": url})).await?;
        if let Some(error) = result["errorText"].as_str() {
            return Err(format!("could not open {url}: {error}"));
        }
        self.wait_until_loaded().await?;
        self.read().await
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
            .evaluate(&format!(
                "(() => {{ const el = document.querySelector({selector}); if (!el) return 'no element matches'; \
                 el.scrollIntoView({{block: 'center'}}); el.click(); return 'clicked'; }})()"
            ))
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
                let result = self.call("Page.navigate", json!({"url": url})).await?;
                if let Some(error) = result["errorText"].as_str() {
                    return Err(format!("could not open {url}: {error}"));
                }
            }
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

/// A port nothing is listening on right now.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
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

/// The debugger URL of the first page in a `/json/list` reply, if it points at the browser we
/// started: a loopback host and the port we chose.
fn page_socket(pages: &Value, port: u16) -> Result<Option<String>, String> {
    let Some(url) = pages
        .as_array()
        .into_iter()
        .flatten()
        .find(|target| target["type"] == "page")
        .and_then(|target| target["webSocketDebuggerUrl"].as_str())
    else {
        return Ok(None);
    };
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| format!("the browser gave an unusable debugger URL: {error}"))?;
    let local = matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    );
    if !local || parsed.port_or_known_default() != Some(port) {
        return Err(
            "the debugger URL is not on the loopback port of the browser we started".to_string(),
        );
    }
    Ok(Some(url.to_string()))
}

/// Waits for the browser to list its first page and returns that page's debugger socket. The
/// debugger can come up after the process starts, and the page target after the debugger
/// (slow starts, notably Edge on Windows), so a refused connection or an empty list is
/// retried until `deadline`, unless `exited` reports the browser is gone. A port file in
/// `profile_dir` naming a different port, or a debugger URL that is not ours, fails at once.
async fn wait_for_page(
    port: u16,
    profile_dir: &Path,
    deadline: Instant,
    mut exited: impl FnMut() -> bool,
) -> Result<String, String> {
    let list = format!("http://127.0.0.1:{port}/json/list");
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;
    let mut last_error = "the browser has no page to drive".to_string();
    while Instant::now() < deadline {
        if exited() {
            return Err(format!(
                "the browser exited before opening its debugging port ({last_error})"
            ));
        }
        if let Ok(text) = std::fs::read_to_string(profile_dir.join("DevToolsActivePort"))
            && let Some(named) = text
                .lines()
                .next()
                .and_then(|line| line.trim().parse::<u16>().ok())
            && named != port
        {
            return Err(
                "the browser listens on another port than the one we asked for".to_string(),
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let request = tokio::time::timeout(remaining, async {
            client.get(&list).send().await?.json::<Value>().await
        });
        match request.await {
            Ok(Ok(pages)) => {
                if let Some(url) = page_socket(&pages, port)? {
                    return Ok(url);
                }
            }
            Ok(Err(error)) => last_error = error.to_string(),
            Err(_) => break,
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "the browser did not open its debugging port ({last_error})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    enum Mock {
        /// Reads nothing and says nothing.
        Silent,
        /// Sends an event every 50 ms and never answers.
        Chatty,
        /// Answers every command with a protocol error.
        Refusing,
        /// Answers every command with a result.
        Answering,
    }

    type Seen = Arc<Mutex<Vec<String>>>;

    /// Serves one debugger socket as `mock` says, noting each command as "method url".
    async fn serve_ws(mut ws: WebSocketStream<TcpStream>, mock: Mock, seen: Seen) {
        let refusing = matches!(mock, Mock::Refusing);
        match mock {
            Mock::Silent => std::future::pending::<()>().await,
            Mock::Chatty => loop {
                let event = Message::Text(r#"{"method":"Page.frameStartedLoading"}"#.into());
                if ws.send(event).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            },
            Mock::Refusing | Mock::Answering => {
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let command: Value = serde_json::from_str(text.as_str()).unwrap();
                    seen.lock().unwrap().push(format!(
                        "{} {}",
                        command["method"].as_str().unwrap_or_default(),
                        command["params"]["url"].as_str().unwrap_or_default()
                    ));
                    let reply = if refusing {
                        json!({"id": command["id"], "error": {"message": "Cannot navigate"}})
                    } else {
                        json!({"id": command["id"], "result": {"frameId": "F"}})
                    };
                    if ws
                        .send(Message::Text(reply.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    }

    /// A `Browser` talking to a stand-in debugger instead of a process, with `log` as the
    /// browser's output, and what the stand-in was sent.
    async fn mock_browser(mock: Mock, log: &str) -> (Browser, tempfile::TempDir, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Seen::default();
        let served = Arc::clone(&seen);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            serve_ws(ws, mock, served).await;
        });
        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("imperium-browser.log");
        std::fs::write(&log_path, log).unwrap();
        let browser = Browser {
            log_path,
            _process: None,
            socket,
            next_id: AtomicU64::new(1),
        };
        (browser, dir, seen)
    }

    /// What a stand-in debugger on one port was asked to do.
    struct Debugger {
        port: u16,
        /// Commands sent to the page it opened, as "method url".
        page: Seen,
        /// Request lines it got to close pages.
        closed: Seen,
    }

    async fn read_request(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut chunk = [0u8; 512];
        while !data.windows(4).any(|window| window == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            data.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    /// A stand-in for the browser's debugging port, HTTP and WebSocket on one port like the
    /// real one: `PUT /json/new` opens a page whose socket behaves as `page_mock` says, and
    /// `GET /json/close/<id>` closes it.
    async fn mock_debugger(page_mock: Mock) -> Debugger {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let page = Seen::default();
        let closed = Seen::default();
        let (page_seen, closed_seen) = (Arc::clone(&page), Arc::clone(&closed));
        tokio::spawn(async move {
            let mut page_mock = Some(page_mock);
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut head = [0u8; 16];
                let n = stream.peek(&mut head).await.unwrap_or(0);
                let head = &head[..n];
                if head.starts_with(b"PUT /json/new")
                    || head.starts_with(b"GET /json/close")
                    || head.starts_with(b"GET /json/list")
                {
                    let request = read_request(&mut stream).await;
                    let body = if request.starts_with("PUT") {
                        json!({
                            "id": "T1",
                            "type": "page",
                            "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/page/T1"),
                        })
                        .to_string()
                    } else if request.starts_with("GET /json/list") {
                        // The throwaway page is gone as soon as it is closed.
                        "[]".to_string()
                    } else {
                        let line = request.lines().next().unwrap_or_default().to_string();
                        closed_seen.lock().unwrap().push(line);
                        "Target is closing".to_string()
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                } else if let Some(mock) = page_mock.take() {
                    let seen = Arc::clone(&page_seen);
                    tokio::spawn(async move {
                        let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                        serve_ws(ws, mock, seen).await;
                    });
                }
            }
        });
        Debugger { port, page, closed }
    }

    const LISTENING: &str =
        "DevTools listening on ws://127.0.0.1:9222/devtools/browser/0a1b2c3d-secret";

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

    #[tokio::test]
    async fn a_call_the_browser_never_answers_times_out_with_its_output_but_not_its_endpoint() {
        let log = format!("{LISTENING}\nboom happened\n");
        let (mut browser, _dir, _) = mock_browser(Mock::Silent, &log).await;
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
    async fn the_warm_up_limit_is_on_the_whole_wait_not_on_silence() {
        // Events every 50 ms keep a silence timer from ever firing; the limit must still hold.
        let debugger = mock_debugger(Mock::Chatty).await;
        let (mut browser, _dir, _) = mock_browser(Mock::Silent, LISTENING).await;
        let started = Instant::now();
        let error = browser
            .warm_up_within(debugger.port, Duration::from_millis(400))
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
    async fn a_refused_warm_up_navigation_is_not_a_failure() {
        let debugger = mock_debugger(Mock::Refusing).await;
        let (mut browser, _dir, _) = mock_browser(Mock::Answering, "").await;
        browser
            .warm_up_within(debugger.port, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(debugger.page.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_warm_up_uses_a_throwaway_page_and_leaves_the_driven_one_alone() {
        let debugger = mock_debugger(Mock::Answering).await;
        let (mut browser, _dir, driven) = mock_browser(Mock::Answering, "").await;
        browser
            .warm_up_within(debugger.port, Duration::from_secs(10))
            .await
            .unwrap();
        // The page being driven was navigated nowhere, so its history holds nothing of the
        // warm-up; it was only brought to the front.
        assert_eq!(*driven.lock().unwrap(), ["Page.bringToFront "]);
        let navigations = debugger.page.lock().unwrap().clone();
        assert_eq!(
            navigations,
            [format!(
                "Page.navigate http://127.0.0.1:{}/json/version",
                debugger.port
            )]
        );
        let closed = debugger.closed.lock().unwrap().clone();
        assert_eq!(closed.len(), 1, "{closed:?}");
        assert!(closed[0].contains("/json/close/T1"), "{closed:?}");
    }

    #[tokio::test]
    async fn a_debugger_that_cannot_open_a_page_does_not_fail_the_start() {
        // Nothing listens on the port, so there is no throwaway page; the start goes on.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (mut browser, _dir, driven) = mock_browser(Mock::Answering, "").await;
        browser
            .warm_up_within(port, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(driven.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_debugger_that_never_replies_is_given_up_on_at_the_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok(connection) = listener.accept().await {
                held.push(connection);
            }
        });
        let profile = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_secs(3);
        let error = wait_for_page(port, profile.path(), deadline, || false)
            .await
            .unwrap_err();
        assert!(error.contains("debugging port"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(6));
    }

    #[test]
    fn only_our_own_loopback_debugger_is_accepted() {
        let list = |url: &str| json!([{"type": "page", "webSocketDebuggerUrl": url}]);
        let ours = "ws://127.0.0.1:9222/devtools/page/A";
        assert_eq!(
            page_socket(&list(ours), 9222).unwrap().as_deref(),
            Some(ours)
        );
        assert!(page_socket(&list("ws://localhost:9222/devtools/page/A"), 9222).is_ok());
        assert!(page_socket(&list("ws://[::1]:9222/devtools/page/A"), 9222).is_ok());
        assert!(page_socket(&list("ws://evil.example:9222/devtools/page/A"), 9222).is_err());
        assert!(page_socket(&list("ws://127.0.0.1:9333/devtools/page/A"), 9222).is_err());
        assert_eq!(page_socket(&json!([]), 9222).unwrap(), None);
    }

    #[tokio::test]
    async fn a_port_file_naming_another_port_is_an_error() {
        let profile = tempfile::tempdir().unwrap();
        std::fs::write(
            profile.path().join("DevToolsActivePort"),
            "9333\n/devtools/x",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let error = wait_for_page(9222, profile.path(), deadline, || false)
            .await
            .unwrap_err();
        assert!(error.contains("another port"), "{error}");
        assert!(
            !error.contains("9333") && !error.contains("9222"),
            "{error}"
        );
    }

    /// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
    #[tokio::test]
    async fn the_browser_opens_reads_clicks_types_and_screenshots() {
        let Some(program) = find_browser() else {
            eprintln!("no Chrome, Chromium, or Edge found; skipping");
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
        let text = browser.open(&url).await.unwrap();
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
        assert!(browser.open("file:///etc/passwd").await.is_err());
    }

    /// Runs where Chrome, Chromium, or Edge is installed; skipped elsewhere.
    #[tokio::test]
    async fn the_user_clicks_types_presses_keys_and_navigates() {
        let Some(program) = find_browser() else {
            eprintln!("no Chrome, Chromium, or Edge found; skipping");
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
}
