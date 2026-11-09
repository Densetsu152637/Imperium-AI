//! Runs a runtime's server (`llama-server`, `vllm serve`, ...) as a child process and waits until
//! it is ready.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::backend::{LLAMA_CPP, VLLM};
use crate::supervisor::{LaunchFuture, LaunchSpec, Launcher, Server};

/// How long a model may take to load before the launch is abandoned.
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_millis(250);

/// Starts each runtime's executable, found by the runtime's id: `llama-server` from a runtime
/// pack or a custom path, and `vllm`.
#[derive(Debug)]
pub struct ProcessLauncher {
    /// Runtime id and its executable.
    programs: Vec<(String, PathBuf)>,
    logs: PathBuf,
    http: reqwest::Client,
}

impl ProcessLauncher {
    /// `server` is the `llama-server` executable and `vllm` the `vllm` one, for Hugging Face
    /// models; each model's output goes to `<logs>/llama-<model>.log`.
    pub fn new(server: PathBuf, vllm: PathBuf, logs: PathBuf) -> Self {
        ProcessLauncher {
            programs: vec![(LLAMA_CPP.into(), server), (VLLM.into(), vllm)],
            logs,
            http: reqwest::Client::new(),
        }
    }

    /// Runs `program` for the runtime with id `runtime`, replacing any executable set for it.
    pub fn with_program(mut self, runtime: impl Into<String>, program: PathBuf) -> Self {
        let runtime = runtime.into();
        self.programs.retain(|(id, _)| *id != runtime);
        self.programs.push((runtime, program));
        self
    }

    fn program(&self, runtime: &str) -> Result<&Path, String> {
        self.programs
            .iter()
            .find(|(id, _)| id == runtime)
            .map(|(_, program)| program.as_path())
            .ok_or_else(|| format!("no executable is set for the {runtime} runtime"))
    }

    async fn start(&self, spec: &LaunchSpec) -> Result<Box<dyn Server>, String> {
        let program = self.program(spec.backend.id())?;
        spec.backend.check_program(program)?;
        let port = free_port()?;
        std::fs::create_dir_all(&self.logs).map_err(|error| error.to_string())?;
        let log_path = self.logs.join(format!("llama-{}.log", spec.model_id));
        let log = std::fs::File::create(&log_path).map_err(|error| error.to_string())?;
        let mut command = Command::new(program);
        command
            .args(spec.args())
            .args(["--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|error| error.to_string())?)
            .stderr(log)
            .kill_on_drop(true);
        library_path(&mut command, program);
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;
        let base = format!("http://127.0.0.1:{port}");
        let ready = tokio::time::timeout(LOAD_TIMEOUT, self.wait_ready(&base, &mut child)).await;
        match ready {
            Ok(Ok(())) => Ok(Box::new(ProcessServer {
                child,
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
                return Err(format!("the model server exited while loading ({status})"));
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

/// A running server process; dropping it kills the process.
struct ProcessServer {
    child: Child,
    base_url: String,
}

impl Server for ProcessServer {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
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
    use crate::backend::LlamaCpp;
    use crate::scan::Format;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_missing_server_is_explained() {
        let launcher = ProcessLauncher::new(
            PathBuf::from("does/not/exist"),
            PathBuf::from("vllm"),
            PathBuf::from("."),
        );
        let spec = LaunchSpec {
            backend: Arc::new(LlamaCpp::default()),
            format: Format::Gguf,
            gpu_memory_utilization: 0.9,
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
