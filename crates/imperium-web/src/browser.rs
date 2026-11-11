//! A browser agents drive over the Chrome DevTools Protocol: Chrome, Chromium, or Edge,
//! started with its own profile under Imperium's data folder (never the user's), visible or
//! headless. After each action a screenshot is taken so the desktop app can mirror the page.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
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
/// Longest page text returned by `read`.
const MAX_TEXT: usize = 20_000;
/// How much of the browser's output an error reports.
const LOG_TAIL: usize = 2_000;

/// What the page looks like after an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub url: String,
    pub title: String,
    /// A JPEG screenshot, base64-encoded.
    pub screenshot: String,
}

pub struct Browser {
    _process: Child,
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
                    "{reason}; {status}; output: {}",
                    log_tail(&log_path)
                ));
            }
        };
        let (socket, _) = tokio_tungstenite::connect_async(socket_url.as_str())
            .await
            .map_err(|error| format!("could not connect to the browser: {error}"))?;
        let mut browser = Browser {
            _process: process,
            socket,
            next_id: AtomicU64::new(1),
        };
        browser.call("Page.enable", json!({})).await?;
        Ok(browser)
    }

    /// Sends a CDP command and waits for its result, skipping events in between.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({"id": id, "method": method, "params": params});
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .map_err(|error| error.to_string())?;
        loop {
            let reply = tokio::time::timeout(LOAD_TIMEOUT, self.socket.next())
                .await
                .map_err(|_| format!("{method} timed out"))?
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
}

/// A port nothing is listening on right now.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

/// The last part of the browser's output, for error messages.
fn log_tail(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let tail = &bytes[bytes.len().saturating_sub(LOG_TAIL)..];
    let text = String::from_utf8_lossy(tail);
    let text = text.trim();
    if text.is_empty() {
        "none".into()
    } else {
        text.to_string()
    }
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
        .map_err(|error| format!("the browser gave an unusable debugger URL {url}: {error}"))?;
    let local = matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    );
    if !local || parsed.port_or_known_default() != Some(port) {
        return Err(format!(
            "the debugger URL {url} is not on 127.0.0.1:{port}, the browser we started"
        ));
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
            return Err(format!(
                "the browser listens on port {named}, not the {port} we asked for"
            ));
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
    use axum::Router;
    use axum::routing::get;

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
        assert!(error.contains("9333"), "{error}");
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
}
