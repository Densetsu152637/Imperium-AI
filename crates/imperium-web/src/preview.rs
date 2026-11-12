//! Live preview: the page the project's dev server serves, shown beside the chat.
//!
//! Agents declare a preview with `preview_open`: a loopback `url` to show, or a `command` and
//! the `port` it serves on. A command runs in the session's background process table
//! ([`imperium_tools::Processes`]), so it runs under the session's isolation tier, is judged
//! like `run_command` (permission rules, safety floor, permission modes), is visible to
//! `command_output` and `command_stop`, and ends with the session. It is also stopped by
//! `preview_close` and by opening another preview. The dev server reloads the page when files
//! change; Imperium only shows it.
//!
//! Previews are limited to `localhost` and `127.0.0.1` because the page runs scripts inside the
//! desktop app's window: the app's content security policy allows frames from those origins
//! only, so a preview cannot navigate to, or load, a remote site. (`[::1]` is left out: CSP
//! host sources cannot name an IPv6 literal, so the window could not show it.)
//! [`loopback_url`] is the same rule applied before any URL is accepted.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use imperium_api::{
    ToolCheck, ToolContext, ToolEvent, ToolFuture, ToolProvider, ToolSpec, parse_arguments,
};
use imperium_tools::{Sandbox, Workspace};
use serde::Deserialize;
use serde_json::json;
use tokio::net::TcpStream;

/// How long a started server may take to listen.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

const TOOLS: [&str; 2] = ["preview_open", "preview_close"];

/// `input` as a preview URL: an http or https URL whose host is `localhost` or `127.0.0.1`,
/// with no user name or password. Returns the URL in its normal form. Anything else is refused
/// with the reason, as a remote page must never run inside the app.
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
        _ => false,
    };
    if !loopback {
        return Err(format!(
            "{input} is not a loopback address: a preview shows only localhost or 127.0.0.1"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{input} must not carry a user name or password"));
    }
    Ok(url.to_string())
}

/// What the preview shows now.
#[derive(Debug)]
struct Running {
    url: String,
    /// The background process serving it and its port, when the session started one.
    server: Option<(String, u16)>,
    /// The command that started it, shown with the page.
    command: Option<String>,
}

/// The session's preview tools.
#[derive(Debug)]
pub struct PreviewTools {
    workspace: Workspace,
    current: Mutex<Option<Running>>,
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
            current: Mutex::default(),
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
    pub async fn close(&self) -> bool {
        let running = self.lock().take();
        let Some(running) = running else {
            return false;
        };
        if let Some((id, _)) = running.server {
            let _ = self.workspace.processes().stop(&id).await;
        }
        true
    }

    /// Why a command cannot be previewed in this session, if so. Decided before any approval.
    fn command_refusal(&self) -> Option<String> {
        if self.workspace.runtime().is_some() {
            return Some(
                "this session runs in a remote environment; a preview there cannot be reached \
                 from the app"
                    .into(),
            );
        }
        match self.workspace.sandbox() {
            Sandbox::Policy | Sandbox::Os { network: true } => None,
            sandbox @ (Sandbox::Os { network: false } | Sandbox::Container { .. }) => {
                Some(format!(
                    "the session's isolation ({}) keeps a server's port from being reached by the \
                 app; use the policy tier or allow the OS sandbox network",
                    sandbox.label()
                ))
            }
        }
    }

    /// Whether the preview's own server (started by this session's preview command) is still
    /// running.
    fn server_alive(&self, id: &str) -> bool {
        matches!(self.workspace.processes().ended(id), Ok(None))
    }

    /// The server of the current preview, if its process is still running.
    fn live_server(&self) -> Option<(String, u16)> {
        let (id, port) = self.lock().as_ref()?.server.clone()?;
        self.server_alive(&id).then_some((id, port))
    }

    /// Whether `url` is the page of the server this session's own preview command started,
    /// which is still running.
    fn is_own_server(&self, url: &str) -> bool {
        let port = reqwest::Url::parse(url)
            .ok()
            .and_then(|url| url.port_or_known_default());
        matches!((self.live_server(), port), (Some((_, own)), Some(port)) if own == port)
    }

    /// Closes a preview whose server has ended (stopped with `command_stop`, or crashed), so
    /// the panel does not keep showing a dead page.
    fn clear_if_ended(&self, context: &ToolContext<'_>) {
        let ended = {
            let guard = self.lock();
            match &*guard {
                Some(Running {
                    server: Some((id, _)),
                    ..
                }) => !self.server_alive(id),
                _ => false,
            }
        };
        if ended {
            *self.lock() = None;
            (context.emit)(ToolEvent::PreviewChanged {
                url: None,
                command: None,
            });
        }
    }

    async fn open(&self, context: &ToolContext<'_>, arguments: &str) -> Result<String, String> {
        let args: Args = parse_arguments(arguments)?;
        self.clear_if_ended(context);
        let mut shown_command = None;
        let (url, server) = match (args.url, args.command) {
            (Some(_), Some(_)) => return Err("give either url or command, not both".into()),
            (Some(url), None) => (loopback_url(&url)?, None),
            (None, Some(command)) => {
                let port = args
                    .port
                    .filter(|port| *port != 0)
                    .ok_or("a command needs the port it serves on")?;
                if let Some(reason) = self.command_refusal() {
                    return Err(reason);
                }
                // The earlier preview stops first so its server frees the port.
                if self.close().await {
                    (context.emit)(ToolEvent::PreviewChanged {
                        url: None,
                        command: None,
                    });
                }
                let id = self.start(&command, port).await?;
                shown_command = Some(command);
                (format!("http://localhost:{port}/"), Some((id, port)))
            }
            (None, None) => return Err("give a url to show, or a command and its port".into()),
        };
        // The page of the session's own running server keeps that server (and its command);
        // any other page replaces it, and the server stops unless the new page uses it.
        let (server, shown_command) = match server {
            Some(server) => (Some(server), shown_command),
            None => match (self.live_server(), reqwest::Url::parse(&url)) {
                (Some((id, own)), Ok(parsed)) if parsed.port_or_known_default() == Some(own) => {
                    let command = self.lock().as_ref().and_then(|r| r.command.clone());
                    (Some((id, own)), command)
                }
                _ => (None, None),
            },
        };
        let new_id = server.as_ref().map(|(id, _)| id.clone());
        let previous = self.lock().replace(Running {
            url: url.clone(),
            server,
            command: shown_command.clone(),
        });
        if let Some(Running {
            server: Some((id, _)),
            ..
        }) = previous
            && Some(&id) != new_id.as_ref()
        {
            let _ = self.workspace.processes().stop(&id).await;
        }
        (context.emit)(ToolEvent::PreviewChanged {
            url: Some(url.clone()),
            command: shown_command,
        });
        Ok(format!(
            "The preview shows {url} in the Preview panel. The dev server reloads the page when files change."
        ))
    }

    /// Starts `command` in the session's background processes and waits for `port` to listen.
    /// Returns the process id.
    async fn start(&self, command: &str, port: u16) -> Result<String, String> {
        if listening(port).await {
            return Err(format!(
                "port {port} is already in use; stop what listens there or choose another port"
            ));
        }
        let processes = self.workspace.processes();
        let id = processes
            .start(&self.workspace, command)
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if listening(port).await {
                return Ok(id);
            }
            let ended = processes.ended(&id).map_err(|error| error.to_string())?;
            let failure = match ended {
                Some(ended) => Some(format!(
                    "the command {ended} before listening on port {port}"
                )),
                None if Instant::now() >= deadline => Some(format!(
                    "nothing listened on port {port} within {} seconds",
                    READY_TIMEOUT.as_secs()
                )),
                None => None,
            };
            if let Some(failure) = failure {
                let output = processes.output(&id, Some(0)).unwrap_or_default();
                let _ = processes.stop(&id).await;
                return Err(format!("{failure}; {output}"));
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

impl ToolProvider for PreviewTools {
    /// The preview serves the session's own folders; a writer works in worktrees of its own.
    fn for_writers(&self) -> bool {
        false
    }

    fn instructions(&self) -> Option<&str> {
        Some(
            "Preview: preview_open shows the app you are building in the user's Preview panel. \
             Give the command that starts the project's dev server and the port it listens on \
             (it runs in the background, is judged like run_command, and stops with the \
             session), or a localhost url that is already served. Only localhost and 127.0.0.1 \
             are shown.",
        )
    }

    fn specs(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "preview_open".into(),
                description: "Show a page the project serves in the user's Preview panel: either a loopback url (http://localhost:<port>), or a command that starts a dev server plus the port it listens on, which runs in the background until preview_close or the session ends.".into(),
                parameters: json!({"type": "object", "properties": {
                    "url": {"type": "string", "description": "A localhost or 127.0.0.1 URL already being served."},
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

    /// A command is judged as `run_command` on it (rules, safety floor, permission modes,
    /// required context). A URL asks unless it is the page of the server this session's own
    /// preview command started; closing never asks.
    fn check(&self, name: &str, arguments: &str) -> Result<ToolCheck, String> {
        if name == "preview_close" {
            return Ok(ToolCheck {
                summary: "close the preview".into(),
                ask: false,
                needs_context: false,
                command: None,
            });
        }
        let args: Args = parse_arguments(arguments)?;
        match (&args.url, &args.command) {
            (Some(url), None) => {
                let url = loopback_url(url)?;
                Ok(ToolCheck {
                    // Any other loopback port may be something private on this machine.
                    ask: !self.is_own_server(&url),
                    summary: format!("preview {url}"),
                    needs_context: false,
                    command: None,
                })
            }
            (None, Some(command)) => {
                let port = args
                    .port
                    .filter(|port| *port != 0)
                    .ok_or("a command needs the port it serves on")?;
                if let Some(reason) = self.command_refusal() {
                    return Err(reason);
                }
                Ok(ToolCheck {
                    summary: format!("start {command} on port {port} and preview it"),
                    ask: true,
                    needs_context: true,
                    command: Some(command.clone()),
                })
            }
            _ => Err("give either a url, or a command and its port".into()),
        }
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext<'a>,
        name: &'a str,
        arguments: &'a str,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            if name == "preview_close" {
                let closed = self.close().await;
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
            // The window's content security policy cannot name an IPv6 literal.
            "http://[::1]:9000/index.html",
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
    fn commands_are_checked_as_run_command_and_need_context() {
        let tools = tools();
        let command = tools
            .check("preview_open", r#"{"command":"npm run dev","port":5173}"#)
            .unwrap();
        assert!(command.ask);
        assert!(command.needs_context);
        assert_eq!(command.command.as_deref(), Some("npm run dev"));
        assert!(command.summary.contains("npm run dev"));
        let url = tools
            .check("preview_open", r#"{"url":"http://localhost:5173"}"#)
            .unwrap();
        assert!(url.ask, "a port this session did not start asks");
        assert!(!url.needs_context);
        assert_eq!(url.command, None);
        let close = tools.check("preview_close", "{}").unwrap();
        assert!(!close.ask && !close.needs_context && close.command.is_none());
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
        assert!(
            tools
                .check("preview_open", r#"{"command":"npm run dev"}"#)
                .is_err()
        );
        assert!(tools.check("preview_open", "{}").is_err());
    }

    #[test]
    fn unreachable_isolation_refuses_before_any_approval() {
        let dir = tempfile::tempdir().unwrap();
        for sandbox in [
            Sandbox::Os { network: false },
            Sandbox::Container {
                runtime: "docker".into(),
                image: "ubuntu".into(),
                network: true,
            },
        ] {
            let workspace = Workspace::new(dir.path()).unwrap().with_sandbox(sandbox);
            let error = PreviewTools::new(workspace)
                .check("preview_open", r#"{"command":"npm run dev","port":3000}"#)
                .unwrap_err();
            assert!(error.contains("isolation"), "{error}");
        }
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
        let port = free_port();
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

    /// Opens the preview served by `command`, which listens on the port named in `dir`. A port
    /// found free a moment ago can be taken by the time the server binds it (the other tests
    /// in this binary bind ports too), so a refusal for a taken port, or a server that could
    /// not bind it, retries on another one. Returns the port that was used.
    async fn open_served(
        dir: &std::path::Path,
        tools: &PreviewTools,
        command: &str,
        mut port: u16,
    ) -> (u16, Result<String, String>, Vec<ToolEvent>) {
        for _ in 0..5 {
            let (result, events) = open(tools, json!({"command": command, "port": port})).await;
            if !matches!(&result, Err(error) if error.contains("already in use") || error.contains("before listening"))
            {
                return (port, result, events);
            }
            let taken = port;
            port = free_port();
            eprintln!("port {taken} was taken; retrying the preview on {port}");
            std::fs::write(dir.join("imperium-preview-port"), port.to_string()).unwrap();
        }
        let (result, events) = open(tools, json!({"command": command, "port": port})).await;
        (port, result, events)
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
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
        let (dir, tools, port, command) = served();
        let (port, result, events) = open_served(dir.path(), &tools, &command, port).await;
        result.unwrap();
        let url = format!("http://localhost:{port}/");
        assert_eq!(tools.url().as_deref(), Some(url.as_str()));
        assert!(listening(port).await);
        assert_eq!(
            events,
            [ToolEvent::PreviewChanged {
                url: Some(url.clone()),
                command: Some(command.clone())
            }]
        );
        // It is one of the session's background processes, and its own page needs no approval.
        assert_eq!(tools.workspace.processes().running(), 1);
        assert!(
            !tools
                .check("preview_open", &json!({"url": url}).to_string())
                .unwrap()
                .ask
        );
        let other = format!("http://localhost:{}/", u32::from(port) ^ 1);
        assert!(
            tools
                .check("preview_open", &json!({"url": other}).to_string())
                .unwrap()
                .ask
        );
        assert!(tools.close().await);
        assert!(stops_listening(port).await, "the server kept running");
        assert_eq!(tools.url(), None);
        assert!(
            tools
                .check("preview_open", &json!({"url": url}).to_string())
                .unwrap()
                .ask
        );
    }

    #[tokio::test]
    async fn a_url_on_the_servers_own_port_keeps_it_and_another_port_stops_it() {
        let (dir, tools, port, command) = served();
        let (port, result, _) = open_served(dir.path(), &tools, &command, port).await;
        result.unwrap();
        let own = format!("http://localhost:{port}/x");
        assert!(tools.is_own_server(&own));
        let (result, events) = open(&tools, json!({"url": own})).await;
        result.unwrap();
        assert!(
            listening(port).await,
            "its own page must not stop the server"
        );
        assert!(tools.is_own_server(&own));
        assert_eq!(
            events,
            [ToolEvent::PreviewChanged {
                url: Some(own.clone()),
                command: Some(command.clone())
            }]
        );
        assert_eq!(tools.workspace.processes().running(), 1);

        let other = format!("http://localhost:{}/", u32::from(port) ^ 1);
        let (result, events) = open(&tools, json!({"url": other})).await;
        result.unwrap();
        assert!(stops_listening(port).await, "another page stops the server");
        assert_eq!(
            events,
            [ToolEvent::PreviewChanged {
                url: Some(other),
                command: None
            }]
        );
    }

    #[tokio::test]
    async fn a_server_that_ended_is_no_longer_the_sessions_own() {
        let (dir, tools, port, command) = served();
        let (port, result, _) = open_served(dir.path(), &tools, &command, port).await;
        result.unwrap();
        let url = format!("http://localhost:{port}/");
        // Stopped behind the preview's back, as command_stop or a crash would.
        tools.workspace.processes().stop_all();
        assert!(stops_listening(port).await);
        for _ in 0..50 {
            if !tools.is_own_server(&url) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!tools.is_own_server(&url));
        assert!(
            tools
                .check("preview_open", &json!({"url": url}).to_string())
                .unwrap()
                .ask
        );
        // The next call clears the dead page.
        let events = Mutex::new(Vec::new());
        let emit = |event: ToolEvent| events.lock().unwrap().push(event);
        let context = ToolContext {
            root: std::path::Path::new("."),
            emit: &emit,
        };
        tools.clear_if_ended(&context);
        assert_eq!(tools.url(), None);
        assert_eq!(
            events.into_inner().unwrap(),
            [ToolEvent::PreviewChanged {
                url: None,
                command: None
            }]
        );
    }

    #[tokio::test]
    async fn the_server_stops_with_the_session() {
        let (dir, tools, port, command) = served();
        let (port, result, _) = open_served(dir.path(), &tools, &command, port).await;
        result.unwrap();
        assert!(listening(port).await);
        // The session stops: the daemon stops its background processes.
        tools.workspace.processes().stop_all();
        assert!(
            stops_listening(port).await,
            "the server outlived its session"
        );
    }

    #[tokio::test]
    async fn the_server_stops_when_the_session_is_dropped() {
        let (dir, tools, port, command) = served();
        let (port, result, _) = open_served(dir.path(), &tools, &command, port).await;
        result.unwrap();
        assert!(listening(port).await);
        drop(tools);
        assert!(
            stops_listening(port).await,
            "the server outlived its session"
        );
    }

    #[tokio::test]
    async fn non_loopback_urls_and_taken_ports_are_refused() {
        let (_dir, tools, _, command) = served();
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
        // The port is taken by this listener, held until the call, not one found free earlier.
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port();
        let (result, _) = open(&tools, json!({"command": command, "port": port})).await;
        assert!(result.unwrap_err().contains("already in use"));
        drop(held);
        let (result, _) = open(&tools, json!({"command": "echo hi"})).await;
        assert!(result.unwrap_err().contains("port"));
    }

    #[tokio::test]
    async fn a_command_that_ends_early_reports_its_output() {
        let (_dir, tools, mut port, _) = served();
        // A port found free a moment ago may be taken by now, which is another refusal.
        let mut attempt = 0;
        let (result, events) = loop {
            let (result, events) = open(
                &tools,
                json!({"command": "echo boom && exit 3", "port": port}),
            )
            .await;
            attempt += 1;
            if attempt < 5 && matches!(&result, Err(error) if error.contains("already in use")) {
                port = free_port();
                continue;
            }
            break (result, events);
        };
        let error = result.unwrap_err();
        assert!(error.contains("boom"), "{error}");
        assert!(error.contains("before listening"), "{error}");
        assert_eq!(tools.url(), None);
        assert!(events.is_empty());
    }
}
