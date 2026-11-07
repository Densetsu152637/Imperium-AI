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

const START_TIMEOUT: Duration = Duration::from_secs(20);
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest page text returned by `read`.
const MAX_TEXT: usize = 20_000;

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
        let port_file = profile_dir.join("DevToolsActivePort");
        let _ = std::fs::remove_file(&port_file);
        let mut command = Command::new(program);
        command
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile_dir.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--window-size=1280,900",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if headless {
            command.arg("--headless=new");
        }
        command.arg("about:blank");
        let process = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;
        let port = wait_for_port(&port_file).await?;
        let pages: Value = reqwest::get(format!("http://127.0.0.1:{port}/json/list"))
            .await
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())?;
        let socket_url = pages
            .as_array()
            .into_iter()
            .flatten()
            .find(|target| target["type"] == "page")
            .and_then(|target| target["webSocketDebuggerUrl"].as_str())
            .ok_or("the browser has no page to drive")?
            .to_string();
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

/// Waits for the browser to write the port it listens on.
async fn wait_for_port(file: &Path) -> Result<u16, String> {
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(file)
            && let Some(port) = text
                .lines()
                .next()
                .and_then(|line| line.trim().parse().ok())
        {
            return Ok(port);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("the browser did not open its debugging port".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;

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
