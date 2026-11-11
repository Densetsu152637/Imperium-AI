//! Live preview: the page the project's dev server serves, shown beside the chat.
//!
//! Agents declare a preview with `preview_open`: a loopback `url` to show, or a `command` and
//! the `port` it serves on. A command runs as one managed background process per session under
//! the session's isolation tier and permission rules; it is stopped by `preview_close`, by
//! opening another preview, and when the session ends. The dev server reloads the page when
//! files change; Imperium only shows it.
//!
//! Previews are limited to loopback addresses (`localhost`, `127.0.0.1`, `[::1]`) because the
//! page runs scripts inside the desktop app's window: the app's content security policy
//! allows frames from loopback origins only, so a preview cannot navigate to, or load, a
//! remote site. [`loopback_url`] is the same rule applied before any URL is accepted.

use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use imperium_api::{
    RuleAction, ToolCheck, ToolContext, ToolEvent, ToolFuture, ToolProvider, ToolSpec,
    parse_arguments,
};
use imperium_tools::permissions::Target;
use imperium_tools::{Sandbox, Workspace};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpStream;
use tokio::process::Child;

/// How long a started server may take to listen.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// How much of the server's output an error reports.
const LOG_TAIL: usize = 2_000;

const TOOLS: [&str; 2] = ["preview_open", "preview_close"];

/// `input` as a preview URL: an http or https URL whose host is `localhost`, `127.0.0.1`, or
/// `[::1]`, with no user name or password. Returns the URL in its normal form. Anything else
/// is refused with the reason, as a remote page must never run inside the app.
pub fn loopback_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    let url = reqwest::Url::parse(input)
        .map_err(|_| format!("{input} is not a URL (expected http://localhost:<port>)"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{input} is not an http or https URL"));
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost",
        Some(url::Host::Ipv4(address)) => address == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(address)) => address == std::net::Ipv6Addr::LOCALHOST,
        None => false,
    };
    if !loopback {
        return Err(format!(
            "{input} is not a loopback address: a preview shows only localhost, 127.0.0.1, or [::1]"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{input} must not carry a user name or password"));
    }
    Ok(url.to_string())
}

/// A started server and the page it serves.
struct Running {
    url: String,
    /// Kills the server and everything it started when dropped.
    _server: Option<Server>,
}

/// A process tree killed on drop (the shell and the dev server it started).
struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            kill_tree(pid);
        }
        let _ = self.child.start_kill();
    }
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .status();
}

#[cfg(not(windows))]
fn kill_tree(pid: u32) {
    // The server runs in its own process group, so one signal reaches all of it.
    let _ = std::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The session's preview tools.
#[derive(Debug)]
pub struct PreviewTools {
    workspace: Workspace,
    current: Arc<Mutex<Option<Running>>>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running").field("url", &self.url).finish()
    }
}

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    port: Option<u16>,
}

impl PreviewTools {
    pub fn new(workspace: Workspace) -> Self {
        PreviewTools {
            workspace,
            current: Arc::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Running>> {
        self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The page the preview shows now.
    pub fn url(&self) -> Option<String> {
        self.lock().as_ref().map(|running| running.url.clone())
    }

    /// Stops the preview's server, if any, and clears the page. True if there was a preview.
    pub fn close(&self) -> bool {
        self.lock().take().is_some()
    }

    async fn open(&self, context: &ToolContext<'_>, arguments: &str) -> Result<String, String> {
        let args: Args = parse_arguments(arguments)?;
        let (url, command, server) = match (args.url, args.command) {
            (Some(_), Some(_)) => return Err("give either url or command, not both".into()),
            (Some(url), None) => (loopback_url(&url)?, None, None),
            (None, Some(command)) => {
                let port = args
                    .port
                    .filter(|port| *port != 0)
                    .ok_or("a command needs the port it serves on")?;
                let server = match self.start(&command, port).await {
                    Ok(server) => server,
                    Err(error) => {
                        // An earlier preview was stopped to make room for this one.
                        if self.url().is_none() {
                            (context.emit)(ToolEvent::PreviewChanged {
                                url: None,
                                command: None,
                            });
                        }
                        return Err(error);
                    }
                };
                (
                    format!("http://localhost:{port}/"),
                    Some(command),
                    Some(server),
                )
            }
            (None, None) => return Err("give a url to show, or a command and its port".into()),
        };
        // The old server stops only now, once the new one is up (or the URL accepted).
        *self.lock() = Some(Running {
            url: url.clone(),
            _server: server,
        });
        (context.emit)(ToolEvent::PreviewChanged {
            url: Some(url.clone()),
            command,
        });
        Ok(format!(
            "The preview shows {url} in the Preview panel. The dev server reloads the page when files change."
        ))
    }

    /// Starts `command` under the session's isolation tier and waits for `port` to listen.
    async fn start(&self, command: &str, port: u16) -> Result<Server, String> {
        if self.workspace.runtime().is_some() {
            return Err(
                "this session runs in a remote environment; a preview there cannot be reached \
                 from the app. Start the server there and open a url only if it is forwarded \
                 to this machine"
                    .into(),
            );
        }
        match self.workspace.sandbox() {
            Sandbox::Policy => {}
            Sandbox::Os { network: true } => {}
            Sandbox::Os { network: false } | Sandbox::Container { .. } => {
                return Err(format!(
                    "the session's isolation ({}) keeps a server's port from being reached by \
                     the app; use the policy tier or allow the OS sandbox network",
                    self.workspace.sandbox().label()
                ));
            }
        }
        // Replace any earlier preview first so the old server frees its port.
        drop(self.lock().take());
        if listening(port).await {
            return Err(format!(
                "port {port} is already in use; stop what listens there or choose another port"
            ));
        }
        let roots = self.workspace.command_roots();
        let roots: Vec<&std::path::Path> = roots.iter().map(std::path::PathBuf::as_path).collect();
        let mut process = self
            .workspace
            .sandbox()
            .command(&roots, self.workspace.grants(), command)
            .map_err(|error| error.to_string())?;
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process
            .spawn()
            .map_err(|error| format!("could not start the command: {error}"))?;
        let output = Arc::new(Mutex::new(String::new()));
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(collect(stdout, Arc::clone(&output)));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(collect(stderr, Arc::clone(&output)));
        }
        let mut server = Server { child };
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if listening(port).await {
                return Ok(server);
            }
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(format!(
                    "the command ended ({status}) before listening on port {port}; output: {}",
                    tail(&output)
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "nothing listened on port {port} within {} seconds; output: {}",
                    READY_TIMEOUT.as_secs(),
                    tail(&output)
                ));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Whether something accepts connections on `port` on the loopback address.
async fn listening(port: u16) -> bool {
    for host in ["127.0.0.1", "::1"] {
        let attempt =
            tokio::time::timeout(Duration::from_millis(300), TcpStream::connect((host, port)))
                .await;
        if matches!(attempt, Ok(Ok(_))) {
            return true;
        }
    }
    false
}

/// Keeps the end of what a stream prints, so the pipe never fills and errors can quote it.
async fn collect(mut stream: impl AsyncRead + Unpin, output: Arc<Mutex<String>>) {
    let mut buffer = [0u8; 1024];
    while let Ok(read) = stream.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let mut text = output.lock().unwrap_or_else(PoisonError::into_inner);
        text.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if text.len() > 4 * LOG_TAIL {
            let mut cut = text.len() - 2 * LOG_TAIL;
            while !text.is_char_boundary(cut) {
                cut += 1;
            }
            text.drain(..cut);
        }
    }
}

fn tail(output: &Mutex<String>) -> String {
    let text = output.lock().unwrap_or_else(PoisonError::into_inner);
    let mut start = text.len().saturating_sub(LOG_TAIL);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let tail = text[start..].trim();
    if tail.is_empty() {
        "none".into()
    } else {
        tail.to_string()
    }
}

impl ToolProvider for PreviewTools {
    fn instructions(&self) -> Option<&str> {
        Some(
            "Preview: preview_open shows the app you are building in the user's Preview panel. \
             Give the command that starts the project's dev server and the port it listens on \
             (it runs in the background and stops with the session), or a localhost url that is \
             already served. Only localhost, 127.0.0.1, and [::1] are shown.",
        )
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "preview_open".into(),
                description: "Show a page the project serves in the user's Preview panel: either a loopback url (http://localhost:<port>), or a command that starts a dev server plus the port it listens on, which runs in the background until preview_close or the session ends.".into(),
                parameters: json!({"type": "object", "properties": {
                    "url": {"type": "string", "description": "A localhost, 127.0.0.1, or [::1] URL already being served."},
                    "command": {"type": "string", "description": "Shell command that starts the dev server, such as `npm run dev`."},
                    "port": {"type": "integer", "description": "The port the command's server listens on (required with command)."}
                }}),
            },
            ToolSpec {
                name: "preview_close".into(),
                description: "Close the Preview panel's page and stop the dev server preview_open started.".into(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        ]
    }

    fn handles(&self, name: &str) -> bool {
        TOOLS.contains(&name)
    }

    /// A URL only shows a page and never asks; a command asks unless a `run_command` rule
    /// allows it, and is refused when a rule denies it.
    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        if name == "preview_close" {
            return Ok(ToolCheck {
                summary: "close the preview".into(),
                ask: false,
                needs_context: false,
            });
        }
        let args: Args = parse_arguments(arguments)?;
        let (summary, ask) = match (&args.url, &args.command) {
            (Some(url), None) => (format!("preview {}", loopback_url(url)?), false),
            (None, Some(command)) => {
                let rule = self
                    .workspace
                    .permissions()
                    .evaluate("run_command", &Target::Command(command.clone()));
                let ask = match rule {
                    Some(RuleAction::Deny) => {
                        return Err(format!("a permission rule denies running {command}"));
                    }
                    Some(RuleAction::Allow) => false,
                    _ => true,
                };
                let port = args
                    .port
                    .map(|port| format!(" on port {port}"))
                    .unwrap_or_default();
                (format!("start {command}{port} and preview it"), ask)
            }
            _ => return Err("give either a url, or a command and its port".into()),
        };
        Ok(ToolCheck {
            summary,
            ask,
            needs_context: false,
        })
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            if name == "preview_close" {
                let closed = self.close();
                (context.emit)(ToolEvent::PreviewChanged {
                    url: None,
                    command: None,
                });
                return Ok(if closed {
                    "The preview is closed and its server stopped.".into()
                } else {
                    "No preview was open.".into()
                });
            }
            self.open(context, arguments).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_http_urls_are_previewed() {
        for good in [
            "http://localhost:5173",
            "http://LOCALHOST:3000/app?x=1#top",
            "http://127.0.0.1:8080/",
            "http://[::1]:9000/index.html",
            "https://localhost:8443",
            "  http://localhost/  ",
        ] {
            assert!(loopback_url(good).is_ok(), "{good}");
        }
        assert_eq!(
            loopback_url("http://localhost:5173").unwrap(),
            "http://localhost:5173/"
        );
        for bad in [
            "https://example.com",
            "http://localhost.evil.com:80",
            "http://127.0.0.1.evil.com/",
            "http://evil.com/?u=http://localhost",
            "http://localhost@evil.com/",
            "http://127.0.0.1:80@evil.com/",
            "http://user:pw@localhost:3000/",
            "http://192.168.1.5:3000",
            "http://0.0.0.0:3000",
            "http://127.0.0.2:3000",
            "http://[::2]:3000",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://localhost/",
            "localhost:3000",
            "//localhost:3000",
            "",
            "not a url",
        ] {
            assert!(loopback_url(bad).is_err(), "{bad}");
        }
        // A backslash is a slash for http URLs, so this host is evil.com.
        assert!(loopback_url(r"http://evil.com\@localhost/").is_err());
    }

    fn tools() -> PreviewTools {
        let dir = tempfile::tempdir().unwrap();
        // The folder is deleted when `dir` drops, but the tests below never use it.
        PreviewTools::new(Workspace::new(dir.path()).unwrap())
    }

    #[test]
    fn urls_do_not_ask_and_commands_do() {
        let tools = tools();
        let url = tools
            .check("preview_open", r#"{"url":"http://localhost:5173"}"#)
            .unwrap();
        assert!(!url.ask);
        let command = tools
            .check("preview_open", r#"{"command":"npm run dev","port":5173}"#)
            .unwrap();
        assert!(command.ask);
        assert!(command.summary.contains("npm run dev"));
        assert!(
            tools
                .check("preview_open", r#"{"url":"https://example.com"}"#)
                .is_err()
        );
        assert!(
            tools
                .check(
                    "preview_open",
                    r#"{"url":"http://localhost:1","command":"x"}"#
                )
                .is_err()
        );
        assert!(tools.check("preview_open", "{}").is_err());
    }

    /// Not a test of its own: the lifecycle tests run this test binary again, in a workspace
    /// that names a port, as a stand-in dev server. Elsewhere it does nothing.
    #[tokio::test]
    async fn stand_in_dev_server() {
        let Ok(port) = std::fs::read_to_string("imperium-preview-port") else {
            return;
        };
        let port: u16 = port.trim().parse().unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        // Never outlives a test run that forgot to stop it.
        let _ = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let _ = listener.accept().await;
            }
        })
        .await;
    }

    /// A workspace, its preview tools, and the command that serves `port` from it.
    fn served() -> (tempfile::TempDir, PreviewTools, u16, String) {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("imperium-preview-port"), port.to_string()).unwrap();
        let exe = std::env::current_exe().unwrap();
        let command = format!(
            "\"{}\" --exact preview::tests::stand_in_dev_server --nocapture",
            exe.display()
        );
        let tools = PreviewTools::new(Workspace::new(dir.path()).unwrap());
        (dir, tools, port, command)
    }

    async fn open(
        tools: &PreviewTools,
        arguments: serde_json::Value,
    ) -> (Result<String, String>, Vec<ToolEvent>) {
        let events = Mutex::new(Vec::new());
        let emit = |event: ToolEvent| events.lock().unwrap().push(event);
        let context = ToolContext {
            root: std::path::Path::new("."),
            emit: &emit,
        };
        let result = tools
            .call(&context, "preview_open", &arguments.to_string())
            .await;
        let events = events.into_inner().unwrap();
        (result, events)
    }

    async fn stops_listening(port: u16) -> bool {
        for _ in 0..50 {
            if !listening(port).await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_command_runs_in_the_background_and_close_stops_it() {
        let (_dir, tools, port, command) = served();
        let (result, events) = open(&tools, json!({"command": command, "port": port})).await;
        result.unwrap();
        let url = format!("http://localhost:{port}/");
        assert_eq!(tools.url().as_deref(), Some(url.as_str()));
        assert!(listening(port).await);
        assert_eq!(
            events,
            [ToolEvent::PreviewChanged {
                url: Some(url),
                command: Some(command)
            }]
        );
        assert!(tools.close());
        assert!(stops_listening(port).await, "the server kept running");
        assert_eq!(tools.url(), None);
    }

    #[tokio::test]
    async fn the_server_stops_with_the_session() {
        let (_dir, tools, port, command) = served();
        open(&tools, json!({"command": command, "port": port}))
            .await
            .0
            .unwrap();
        assert!(listening(port).await);
        // The session ends: its tool providers are dropped.
        drop(tools);
        assert!(
            stops_listening(port).await,
            "the server outlived its session"
        );
    }

    #[tokio::test]
    async fn non_loopback_urls_and_taken_ports_are_refused() {
        let (_dir, tools, port, command) = served();
        for url in [
            "https://example.com/",
            "http://192.168.0.2:3000",
            "http://localhost@evil.com",
        ] {
            let (result, events) = open(&tools, json!({"url": url})).await;
            assert!(result.is_err(), "{url}");
            assert!(events.is_empty());
        }
        assert_eq!(tools.url(), None);
        let held = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        let (result, _) = open(&tools, json!({"command": command, "port": port})).await;
        assert!(result.unwrap_err().contains("already in use"));
        drop(held);
        let (result, _) = open(&tools, json!({"command": "echo hi"})).await;
        assert!(result.unwrap_err().contains("port"));
    }

    #[tokio::test]
    async fn a_command_that_ends_early_reports_its_output() {
        let (_dir, tools, port, _) = served();
        let (result, events) = open(
            &tools,
            json!({"command": "echo boom && exit 3", "port": port}),
        )
        .await;
        let error = result.unwrap_err();
        assert!(error.contains("boom"), "{error}");
        assert!(error.contains("before listening"), "{error}");
        assert_eq!(tools.url(), None);
        assert!(
            events
                .iter()
                .all(|event| matches!(event, ToolEvent::PreviewChanged { url: None, .. }))
        );
    }
}
