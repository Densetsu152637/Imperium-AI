//! Runs `llama-server` as a child process and waits until it is ready.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::supervisor::{LaunchFuture, LaunchSpec, Launcher, Server};

/// How long a model may take to load before the launch is abandoned.
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_millis(250);

/// Starts `llama-server` from a runtime pack or a custom path.
#[derive(Debug)]
pub struct ProcessLauncher {
    server: PathBuf,
    logs: PathBuf,
    http: reqwest::Client,
}

impl ProcessLauncher {
    /// `server` is the `llama-server` executable; each model's output goes to
    /// `<logs>/llama-<model>.log`.
    pub fn new(server: PathBuf, logs: PathBuf) -> Self {
        ProcessLauncher {
            server,
            logs,
            http: reqwest::Client::new(),
        }
    }

    async fn start(&self, spec: &LaunchSpec) -> Result<Box<dyn Server>, String> {
        if !self.server.is_file() {
            return Err(format!(
                "llama-server not found at {}; run `imperium runtime install` or set \
                 runtime.llamacpp.custom_path",
                self.server.display()
            ));
        }
        let port = free_port()?;
        std::fs::create_dir_all(&self.logs).map_err(|error| error.to_string())?;
        let log_path = self.logs.join(format!("llama-{}.log", spec.model_id));
        let log = std::fs::File::create(&log_path).map_err(|error| error.to_string())?;
        let mut command = Command::new(&self.server);
        command
            .args(spec.args())
            .args(["--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|error| error.to_string())?)
            .stderr(log)
            .kill_on_drop(true);
        library_path(&mut command, &self.server);
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", self.server.display()))?;
        let base = format!("http://127.0.0.1:{port}");
        let ready = tokio::time::timeout(LOAD_TIMEOUT, self.wait_ready(&base, &mut child)).await;
        match ready {
            Ok(Ok(())) => Ok(Box::new(ProcessServer {
                _child: child,
                base_url: format!("{base}/v1"),
            })),
            Ok(Err(message)) => Err(format!("{message}; see {}", log_path.display())),
            Err(_) => Err(format!(
                "the model did not load within {} s; see {}",
                LOAD_TIMEOUT.as_secs(),
                log_path.display()
            )),
        }
    }

    /// Polls `/health` until the server reports ready (it answers 503 while loading).
    async fn wait_ready(&self, base: &str, child: &mut Child) -> Result<(), String> {
        loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!("llama-server exited while loading ({status})"));
            }
            let health = self.http.get(format!("{base}/health")).send().await;
            if health.is_ok_and(|response| response.status().is_success()) {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Launcher for ProcessLauncher {
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(self.start(spec))
    }
}

/// A running `llama-server`; dropping it kills the process.
struct ProcessServer {
    _child: Child,
    base_url: String,
}

impl Server for ProcessServer {
    fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Asks the OS for a free port. Another process could take it before the server binds; the
/// launch then fails and is reported.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

/// Runtime packs keep shared libraries in `lib/` beside `bin/` (Windows finds DLLs in `bin/`).
fn library_path(command: &mut Command, server: &Path) {
    let Some(lib) = server
        .parent()
        .and_then(Path::parent)
        .map(|pack| pack.join("lib"))
    else {
        return;
    };
    if !lib.is_dir() {
        return;
    }
    let variable = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let mut paths = vec![lib];
    if let Some(existing) = std::env::var_os(variable) {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        command.env(variable, joined);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_server_is_explained() {
        let launcher = ProcessLauncher::new(PathBuf::from("does/not/exist"), PathBuf::from("."));
        let spec = LaunchSpec {
            model_id: "m".into(),
            model_path: PathBuf::from("m.gguf"),
            mmproj: None,
            context: 4096,
            slots: 1,
            gpu_layers: 0,
            cache_type: "f16".into(),
            extra_args: Vec::new(),
        };
        let error = launcher.launch(&spec).await.err().unwrap();
        assert!(error.contains("imperium runtime install"), "{error}");
    }
}
