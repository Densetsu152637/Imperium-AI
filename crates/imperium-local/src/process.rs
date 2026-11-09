//! Runs a runtime's server (`llama-server`, `vllm serve`, ...) as a child process and waits until
//! it is ready.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::backend::{LLAMA_CPP, VLLM};
use crate::supervisor::{
    LaunchError, LaunchFuture, LaunchSpec, Launcher, Server, SlotFuture, SlotTransfer, StopFuture,
};

/// How long a model may take to load before the launch is abandoned.
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// How long saving or restoring a slot may take: a long context is gigabytes of KV cache.
const SLOT_TIMEOUT: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(250);

/// What the launcher asks the daemon around starting `llama-server`, so the runtime is
/// installed when a model first needs it rather than when the app starts.
pub trait RuntimeHooks: Send + Sync + fmt::Debug {
    /// A llama.cpp model is about to load with `program`; `present` says whether that file
    /// exists. Returns the `llama-server` to start instead (after installing one, when that is
    /// allowed and nothing is there), or why there is none. With `present`, a hook may start
    /// work in the background (a GPU build, a newer release) and return `program` at once.
    fn ensure(
        &self,
        program: PathBuf,
        present: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PathBuf, String>> + Send>>;

    /// A model loaded with `program`.
    fn started(&self, program: &Path);
}

/// Starts each runtime's executable, found by the runtime's id: `llama-server` from a runtime
/// pack or a custom path, and `vllm`.
#[derive(Debug)]
pub struct ProcessLauncher {
    /// Runtime id and its executable.
    programs: Mutex<Vec<(String, PathBuf)>>,
    /// Asked before and after a llama.cpp model loads.
    hooks: Mutex<Option<Arc<dyn RuntimeHooks>>>,
    logs: PathBuf,
    /// Where llama.cpp servers save slots when their model is swapped out.
    slots: Option<PathBuf>,
    http: reqwest::Client,
}

impl ProcessLauncher {
    /// `server` is the `llama-server` executable and `vllm` the `vllm` one, for Hugging Face
    /// models; each model's output goes to `<logs>/llama-<model>.log`.
    pub fn new(server: PathBuf, vllm: PathBuf, logs: PathBuf) -> Self {
        ProcessLauncher {
            programs: Mutex::new(vec![(LLAMA_CPP.into(), server), (VLLM.into(), vllm)]),
            hooks: Mutex::new(None),
            logs,
            slots: None,
            http: reqwest::Client::new(),
        }
    }

    /// Saves the slots of models swapped out to `dir` (see [`Launcher::slot_dir`]).
    pub fn with_slot_dir(mut self, dir: PathBuf) -> Self {
        self.slots = Some(dir);
        self
    }

    /// Runs `program` for the runtime with id `runtime`, replacing any executable set for it.
    pub fn with_program(self, runtime: impl Into<String>, program: PathBuf) -> Self {
        self.set_program(runtime, program);
        self
    }

    /// Runs `program` for the runtime with id `runtime` from the next launch on, such as after
    /// the user picks another `llama-server` or installs a runtime pack. Servers already
    /// running are not affected.
    pub fn set_program(&self, runtime: impl Into<String>, program: PathBuf) {
        let runtime = runtime.into();
        let mut programs = self.programs.lock().unwrap_or_else(PoisonError::into_inner);
        programs.retain(|(id, _)| *id != runtime);
        programs.push((runtime, program));
    }

    /// Asks `hooks` before and after each llama.cpp model loads ([`RuntimeHooks`]).
    pub fn set_hooks(&self, hooks: Arc<dyn RuntimeHooks>) {
        *self.hooks.lock().unwrap_or_else(PoisonError::into_inner) = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<dyn RuntimeHooks>> {
        self.hooks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn program(&self, runtime: &str) -> Result<PathBuf, String> {
        self.programs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(id, _)| id == runtime)
            .map(|(_, program)| program.clone())
            .ok_or_else(|| format!("no executable is set for the {runtime} runtime"))
    }

    async fn start(&self, spec: &LaunchSpec) -> Result<Box<dyn Server>, LaunchError> {
        let mut program = self.program(spec.backend.id())?;
        let hooks = (spec.backend.id() == LLAMA_CPP)
            .then(|| self.hooks())
            .flatten();
        if let Some(hooks) = &hooks {
            let present = program.is_file();
            program = hooks.ensure(program, present).await?;
        }
        let program = program.as_path();
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
        if let (Ok(Ok(())), Some(hooks)) = (&ready, &hooks) {
            hooks.started(program);
        }
        match ready {
            Ok(Ok(())) => Ok(Box::new(ProcessServer {
                pid: child.id(),
                child: Mutex::new(Some(child)),
                base_url: format!("{base}/v1"),
                root: base,
                http: self.http.clone(),
            })),
            Ok(Err(message)) => Err(format!("{message}; see {}", log_path.display()).into()),
            Err(_) => Err(LaunchError::timed_out(format!(
                "the model did not load within {} s; see {}",
                LOAD_TIMEOUT.as_secs(),
                log_path.display()
            ))),
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

    fn slot_dir(&self) -> Option<PathBuf> {
        self.slots.clone()
    }
}

/// A running server process; dropping it kills the process.
struct ProcessServer {
    /// Killed on drop if [`Server::stop`] has not taken it.
    child: Mutex<Option<Child>>,
    /// The process id, for resource readings; kept after [`Server::stop`] takes the child.
    pid: Option<u32>,
    base_url: String,
    /// The server's address without `/v1`, where llama.cpp's `/slots` endpoints are.
    root: String,
    http: reqwest::Client,
}

impl ProcessServer {
    /// `POST /slots/{slot}?action=save|restore` with the file name (llama.cpp b12180); the
    /// reply counts tokens in `n_saved`/`n_restored` and bytes in `n_written`/`n_read`.
    async fn slot_action(
        &self,
        slot: u32,
        action: &str,
        file: &str,
    ) -> Result<SlotTransfer, String> {
        let url = format!("{}/slots/{slot}?action={action}", self.root);
        let response = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "filename": file }).to_string())
            .timeout(SLOT_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("{action} slot {slot}: {error}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| format!("{action} slot {slot}: {error}"))?;
        let body: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if !status.is_success() {
            let message = body["error"]["message"].as_str().unwrap_or(&text);
            return Err(format!("{action} slot {slot}: {status}: {message}"));
        }
        let (tokens, bytes) = match action {
            "save" => ("n_saved", "n_written"),
            _ => ("n_restored", "n_read"),
        };
        Ok(SlotTransfer {
            tokens: body[tokens].as_u64().unwrap_or(0),
            bytes: body[bytes].as_u64().unwrap_or(0),
        })
    }
}

impl Server for ProcessServer {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn save_slot<'a>(&'a self, slot: u32, file: &'a str) -> SlotFuture<'a> {
        Box::pin(self.slot_action(slot, "save", file))
    }

    fn restore_slot<'a>(&'a self, slot: u32, file: &'a str) -> SlotFuture<'a> {
        Box::pin(self.slot_action(slot, "restore", file))
    }

    fn stop(&self) -> StopFuture<'_> {
        let child = self
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Box::pin(async move {
            if let Some(mut child) = child
                && let Err(error) = child.kill().await
            {
                tracing::warn!(%error, "could not stop a model server");
            }
        })
    }

    fn pid(&self) -> Option<u32> {
        self.pid
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

/// Shared libraries are found beside `llama-server` (llama.cpp's own release builds) or in
/// `lib/` beside `bin/` (self-built packs); Windows finds DLLs beside the program by itself.
pub(crate) fn library_path(command: &mut Command, server: &Path) {
    if cfg!(windows) {
        return;
    }
    let Some(bin) = server.parent() else {
        return;
    };
    let mut paths: Vec<std::path::PathBuf> = [
        Some(bin.to_path_buf()),
        bin.parent().map(|pack| pack.join("lib")),
    ]
    .into_iter()
    .flatten()
    .filter(|dir| dir.is_dir())
    .collect();
    if paths.is_empty() {
        return;
    }
    let variable = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
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
            pool_tokens: 4096,
            slots: 1,
            gpu_layers: 0,
            cpu_only: false,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            flash_attn: "auto".into(),
            cache_ram_mb: 0,
            slot_save_path: None,
            extra_args: Vec::new(),
        };
        let error = launcher.launch(&spec).await.err().unwrap();
        assert!(
            error.message.contains("imperium runtime install"),
            "{error}"
        );
        assert!(!error.timed_out, "a missing server fails at once");
    }
}
