//! Runs a runtime's server (`llama-server`, `vllm serve`, ...) as a child process and waits until
//! it is ready.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::backend::{LLAMA_CPP, NCM, VLLM};
use crate::supervisor::{
    LaunchError, LaunchFuture, LaunchSpec, Launcher, Server, SlotFuture, SlotTransfer,
    SpeculatingFuture, StopFuture,
};

/// How long a model may take to load before the launch is abandoned.
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// How long saving or restoring a slot may take: a long context is gigabytes of KV cache.
const SLOT_TIMEOUT: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(250);
/// How often a loaded `ncm-server` is asked whether it still serves ([`watch_health`]).
const NCM_HEALTH_EVERY: Duration = Duration::from_secs(2);

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
    /// `docker`, the image vLLM models run in and the scope their containers are labelled with
    /// ([`crate::vllm_env::scope`]), when the managed environment is Docker's
    /// ([`crate::vllm_env`]); each model then gets a container of its own.
    docker_vllm: Mutex<Option<(PathBuf, String, String)>>,
    /// `wsl.exe`, the WSL2 distribution and `vllm`'s path inside it, when vLLM runs in WSL2 for a
    /// Windows daemon ([`crate::wsl`]); each model is then `wsl.exe -d <distro> --exec ...`.
    wsl_vllm: Mutex<Option<(PathBuf, String, String)>>,
    /// Asked before and after a llama.cpp model loads.
    hooks: Mutex<Option<Arc<dyn RuntimeHooks>>>,
    logs: PathBuf,
    /// Where llama.cpp servers save slots when their model is swapped out.
    slots: Option<PathBuf>,
    /// The data folder, whose `runtimes/ncm-server` holds the approved `ncm-server` copy
    /// ([`crate::ncm_runtime`]); `None` until set, when NCM models cannot load.
    ncm_data: Mutex<Option<PathBuf>>,
    http: reqwest::Client,
}

impl ProcessLauncher {
    /// `server` is the `llama-server` executable and `vllm` the `vllm` one, for Hugging Face
    /// models; each model's output goes to `<logs>/llama-<model>.log`.
    pub fn new(server: PathBuf, vllm: PathBuf, logs: PathBuf) -> Self {
        ProcessLauncher {
            programs: Mutex::new(vec![(LLAMA_CPP.into(), server), (VLLM.into(), vllm)]),
            docker_vllm: Mutex::new(None),
            wsl_vllm: Mutex::new(None),
            hooks: Mutex::new(None),
            logs,
            slots: None,
            ncm_data: Mutex::new(None),
            http: reqwest::Client::new(),
        }
    }

    /// Runs NCM models with the `ncm-server` approved under `data_dir`
    /// ([`crate::ncm_runtime::verify`]), checked again at every launch.
    pub fn with_ncm_data_dir(self, data_dir: PathBuf) -> Self {
        *self.ncm_data.lock().unwrap_or_else(PoisonError::into_inner) = Some(data_dir);
        self
    }

    /// The approved `ncm-server` copy, verified for one launch (hashed off the async threads).
    async fn verified_ncm(&self) -> Result<crate::ncm_runtime::Verified, String> {
        let data = self
            .ncm_data
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| crate::ncm_runtime::HOW_TO_INSTALL.to_string())?;
        tokio::task::spawn_blocking(move || crate::ncm_runtime::verify(&data))
            .await
            .map_err(|error| format!("checking the approved ncm-server failed: {error}"))?
    }

    /// Saves the slots of models swapped out to `dir` (see [`Launcher::slot_dir`]).
    pub fn with_slot_dir(mut self, dir: PathBuf) -> Self {
        self.slots = Some(dir);
        self
    }

    /// Runs vLLM models in containers of `image`, started by `docker`, labelled with the scope of
    /// `data_dir`. Once per daemon, containers an earlier daemon of this host and data folder
    /// left behind are removed, off the start's path ([`crate::vllm_env::sweep_stale_containers`]).
    pub fn with_docker_vllm(self, docker: PathBuf, image: String, data_dir: &Path) -> Self {
        static SWEEP: std::sync::Once = std::sync::Once::new();
        let scope = crate::vllm_env::scope(data_dir);
        let sweeper = (docker.clone(), scope.clone());
        SWEEP.call_once(|| {
            std::thread::spawn(move || {
                let (docker, scope) = sweeper;
                let remote = std::env::var_os("DOCKER_HOST").is_some_and(|host| !host.is_empty());
                let system = sysinfo::System::new_all();
                let removed = crate::vllm_env::sweep_stale_containers(
                    &docker,
                    &scope,
                    std::process::id(),
                    &|pid| system.process(sysinfo::Pid::from_u32(pid)).is_some(),
                    remote,
                    crate::vllm_env::SWEEP_TIMEOUT,
                );
                if !removed.is_empty() {
                    tracing::info!(containers = ?removed, "removed vLLM containers a previous daemon left");
                }
            });
        });
        *self
            .docker_vllm
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((docker, image, scope));
        self
    }

    /// Runs vLLM models inside the WSL2 distribution `distro`, with `program` (the path of `vllm`
    /// inside it), started by `wsl` (`wsl.exe`). The model folder is translated to its WSL path,
    /// the server listens on `127.0.0.1` inside WSL2 and is reached through WSL2's localhost
    /// forwarding, and stopping a model kills the `vllm` process inside the distribution
    /// ([`WslGuard`]), not only `wsl.exe`.
    pub fn with_wsl_vllm(self, wsl: PathBuf, distro: String, program: String) -> Self {
        *self.wsl_vllm.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((wsl, distro, program));
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
        // An NCM model runs the approved copy of ncm-server, hashed through a handle held for
        // the server's life, from its own folder; nothing beside the binary the user named is
        // loaded.
        let ncm = if spec.backend.id() == NCM {
            let verified = self.verified_ncm().await?;
            if !verified.approval.cuda
                && spec
                    .args()
                    .windows(2)
                    .any(|pair| pair[0] == "--device" && pair[1] == "cuda")
            {
                return Err(format!(
                    "the approved ncm-server ({}) was approved as a build without CUDA, so it \
                     cannot run this model on the GPU: set runtime.ncm.device to auto or cpu, or \
                     build it with `--features cuda` and approve it again with `imperium runtime \
                     ncm use <path>`",
                    verified.approval.source.display()
                )
                .into());
            }
            Some(verified)
        } else {
            None
        };
        let mut program = match &ncm {
            Some(verified) => verified.program.clone(),
            None => self.program(spec.backend.id())?,
        };
        let hooks = (spec.backend.id() == LLAMA_CPP)
            .then(|| self.hooks())
            .flatten();
        if let Some(hooks) = &hooks {
            let present = program.is_file();
            program = hooks.ensure(program, present).await?;
        }
        let docker = (spec.backend.id() == VLLM)
            .then(|| {
                self.docker_vllm
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
            .flatten();
        let wsl = (spec.backend.id() == VLLM)
            .then(|| {
                self.wsl_vllm
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
            .flatten();
        if docker.is_none() && wsl.is_none() {
            spec.backend.check_program(program.as_path())?;
        }
        if wsl.is_some()
            && let Some(problem) = crate::wsl::forwarding_problem_here()
        {
            return Err(problem.into());
        }
        let port = free_port()?;
        // A container is named so it can be removed; its client process is what is supervised.
        // The same goes for a `vllm` inside WSL2: `wsl.exe` is what is supervised.
        let mut in_wsl = None;
        let (program, args, container) = match (docker, wsl) {
            (Some((docker, image, scope)), _) => {
                let name = crate::vllm_env::fresh_container_name(port);
                let args =
                    crate::vllm_env::docker_run_args(&image, &name, port, &scope, &spec.args())?;
                (docker.clone(), args, Some((docker, name)))
            }
            (None, Some((wsl, distro, vllm))) => {
                let serve = spec.args();
                let args = crate::wsl::serve_args(&distro, &vllm, port, &serve)?;
                let folder = crate::wsl::model_folder(&distro, &serve)?;
                in_wsl = Some((
                    wsl.clone(),
                    distro,
                    crate::wsl::stop_pattern(&vllm, &folder, port),
                ));
                (wsl, args, None)
            }
            (None, None) => {
                let mut args = spec.args();
                args.extend(["--port".to_string(), port.to_string()]);
                (program, args, None)
            }
        };
        let program = program.as_path();
        std::fs::create_dir_all(&self.logs).map_err(|error| error.to_string())?;
        let prefix = if ncm.is_some() { "ncm" } else { "llama" };
        let log_path = self.logs.join(format!("{prefix}-{}.log", spec.model_id));
        let log = std::fs::File::create(&log_path).map_err(|error| error.to_string())?;
        let mut command = Command::new(program);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|error| error.to_string())?)
            .stderr(log)
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            if in_wsl.is_some() {
                // The daemon has no console: `wsl.exe` would open one.
                command.creation_flags(0x0800_0000);
            }
        }
        // The draft model is Imperium's to choose and to count in memory: llama-server must not
        // pick one up from the environment the daemon runs in.
        for variable in crate::backend::scrubbed_env(spec) {
            command.env_remove(&variable);
        }
        if let Some(allowlist) = spec.backend.env_allowlist() {
            // Only what the server needs: no secrets, no CUBECL_* switches.
            command.env_clear();
            command.envs(crate::backend::allowed_env(
                allowlist,
                std::env::vars_os()
                    .filter_map(|(name, value)| Some((name.into_string().ok()?, value))),
                cfg!(windows),
            ));
        }
        if let Some(verified) = &ncm {
            command.current_dir(&verified.dir);
        } else if container.is_none() && in_wsl.is_none() {
            library_path(&mut command, program);
        }
        if spec.backend.id() == VLLM {
            // vLLM reports usage to its maintainers unless told not to.
            command
                .env("VLLM_NO_USAGE_STATS", "1")
                .env("DO_NOT_TRACK", "1");
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;
        // Written down so a daemon that dies without stopping it does not leave it running.
        if let Some(pid) = child.id() {
            crate::ledger::record(&self.logs, pid);
        }
        // From here on the container is ours to remove: by `stop`, on a failed start, or when
        // this future is dropped (a cancelled start) or the server is.
        let container = container.map(|(docker, name)| ContainerGuard::new(docker, name));
        // The same for the `vllm` inside WSL2, which can outlive the `wsl.exe` that started it.
        let wsl_guard = in_wsl.map(|(wsl, distro, pattern)| WslGuard::new(wsl, distro, pattern));
        let base = format!("http://127.0.0.1:{port}");
        let ready = tokio::time::timeout(LOAD_TIMEOUT, self.wait_ready(&base, &mut child)).await;
        if let (Ok(Ok(())), Some(hooks)) = (&ready, &hooks) {
            hooks.started(program);
        }
        match ready {
            Ok(Ok(())) => Ok(Box::new(ProcessServer {
                container,
                wsl: wsl_guard,
                // ncm-server keeps running after an internal error but answers 503 to
                // everything: watched, it counts as crashed and is loaded again.
                watch: ncm.as_ref().map(|_| {
                    let poisoned = Arc::new(Mutex::new(None));
                    let task = watch_health(
                        self.http.clone(),
                        base.clone(),
                        NCM_HEALTH_EVERY,
                        Arc::clone(&poisoned),
                    );
                    HealthWatch {
                        poisoned,
                        _task: AbortOnDrop(task),
                    }
                }),
                _held: ncm.map(|verified| verified.held),
                pid: child.id(),
                child: Mutex::new(Some(child)),
                base_url: format!("{base}/v1"),
                root: base,
                http: self.http.clone(),
            })),
            Ok(Err(message)) => {
                if let Some(guard) = &wsl_guard {
                    guard.remove().await;
                }
                let hint = if let Some(guard) = &container {
                    guard.remove().await;
                    "; if the image is missing, run `imperium runtime vllm setup --docker`"
                } else if wsl_guard.is_some() {
                    "; if vLLM is not installed there, run `imperium runtime vllm setup --wsl`"
                } else {
                    ""
                };
                Err(format!("{message}{hint}; see {}", log_path.display()).into())
            }
            Err(_) => {
                if let Some(guard) = &container {
                    guard.remove().await;
                }
                let forwarding = if let Some(guard) = &wsl_guard {
                    guard.remove().await;
                    "; if the log shows the server running, Windows is not reaching it: check \
                     WSL2's localhost forwarding (`localhostForwarding` and `networkingMode` in \
                     %USERPROFILE%\\.wslconfig)"
                } else {
                    ""
                };
                Err(LaunchError::timed_out(format!(
                    "the model did not load within {} s{forwarding}; see {}",
                    LOAD_TIMEOUT.as_secs(),
                    log_path.display()
                )))
            }
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
    /// The container of a vLLM model run in Docker; removed by [`Server::stop`] or on drop.
    container: Option<ContainerGuard>,
    /// The `vllm` process inside WSL2 of a model run there; killed by [`Server::stop`] or on drop.
    wsl: Option<WslGuard>,
    /// The handle the approved `ncm-server` copy was hashed through, held while the server
    /// runs so the file cannot be replaced under it (Windows).
    _held: Option<std::fs::File>,
    /// The `/health` watch of an `ncm-server` ([`watch_health`]).
    watch: Option<HealthWatch>,
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
    /// `GET /slots` (llama.cpp b12180): each slot's `speculative` flag is true when the server
    /// has a speculative decoding context. `None` when the reply cannot be read (`--no-slots`,
    /// an older server).
    async fn slots_speculating(&self) -> Option<bool> {
        let response = self
            .http
            .get(format!("{}/slots", self.root))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let slots: serde_json::Value = response.json().await.ok()?;
        speculating_in(&slots)
    }

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
            // Erasing frees cells; it moves no bytes.
            "erase" => ("n_erased", "n_bytes_none"),
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

    /// `POST /slots/{slot}?action=erase` (llama.cpp b12180): frees the slot's KV cells.
    fn erase_slot(&self, slot: u32) -> SlotFuture<'_> {
        Box::pin(self.slot_action(slot, "erase", ""))
    }

    fn speculating(&self) -> SpeculatingFuture<'_> {
        Box::pin(self.slots_speculating())
    }

    fn stop(&self) -> StopFuture<'_> {
        let child = self
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Box::pin(async move {
            // One that has exited on its own (see `exited`) is already gone.
            if let Some(mut child) = child
                && !matches!(child.try_wait(), Ok(Some(_)))
                && let Err(error) = child.kill().await
            {
                tracing::warn!(%error, "could not stop a model server");
            }
            // Killing `docker run` leaves its container running.
            if let Some(guard) = &self.container {
                guard.remove().await;
            }
            // Killing `wsl.exe` need not end the `vllm` it started inside the distribution.
            if let Some(guard) = &self.wsl {
                guard.remove().await;
            }
        })
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Asks the OS whether the process is still there (a non-blocking `try_wait`); a child
    /// [`Server::stop`] has taken was stopped on purpose, not crashed. An `ncm-server` that
    /// answered 503 after it was ready has stopped serving and counts as crashed too.
    fn exited(&self) -> Option<String> {
        if let Some(watch) = &self.watch
            && let Some(why) = watch
                .poisoned
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        {
            return Some(why);
        }
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        match child.as_mut()?.try_wait() {
            Ok(Some(status)) => Some(format!("the server exited ({status})")),
            // Not knowing is not a crash: calls to it still say whether it serves.
            Ok(None) | Err(_) => None,
        }
    }
}

/// The `/health` watch of a loaded `ncm-server`: what it found, and its task, stopped with the
/// server.
struct HealthWatch {
    /// Why the server stopped serving, once it answered 503.
    poisoned: Arc<Mutex<Option<String>>>,
    _task: AbortOnDrop,
}

/// Aborts a task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Asks `GET <root>/health` every `every` until the server answers 503, then records why in
/// `poisoned` and stops. A loaded `ncm-server` answers 503 to every request, `/health` included,
/// once its engine hit an internal error, while the process keeps running; the supervisor then
/// treats it as crashed ([`Server::exited`]), stops it and loads the model again at its next
/// call. A request that fails or times out is not taken for that: only an explicit 503 is.
fn watch_health(
    http: reqwest::Client,
    root: String,
    every: Duration,
    poisoned: Arc<Mutex<Option<String>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            let Ok(response) = http
                .get(format!("{root}/health"))
                .timeout(Duration::from_secs(10))
                .send()
                .await
            else {
                continue;
            };
            if response.status() != reqwest::StatusCode::SERVICE_UNAVAILABLE {
                continue;
            }
            let body = response.text().await.unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(body);
            let message: String = message.chars().take(300).collect();
            *poisoned.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!(
                "the server stopped serving after an internal error (503: {message})"
            ));
            return;
        }
    })
}

/// A model's Docker container, removed when this is dropped unless [`ContainerGuard::remove`]
/// already did it, so a cancelled start, a stopped or dropped server and a daemon shutting down
/// leave nothing running (a killed daemon is covered by the sweep at the next start,
/// [`crate::vllm_env::sweep_stale_containers`]).
#[derive(Debug)]
pub(crate) struct ContainerGuard {
    docker: PathBuf,
    name: String,
    /// Cleared by whoever removes the container, so it is removed once.
    armed: AtomicBool,
}

impl ContainerGuard {
    pub(crate) fn new(docker: PathBuf, name: String) -> Self {
        ContainerGuard {
            docker,
            name,
            armed: AtomicBool::new(true),
        }
    }

    /// `docker rm -f <name>`, awaited. Failures are not errors (it may be gone already).
    pub(crate) async fn remove(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = Command::new(&self.docker)
            .args(["rm", "-f", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

impl Drop for ContainerGuard {
    /// Starts the removal and does not wait for it (a reaper thread collects the process), so
    /// dropping never blocks the runtime.
    fn drop(&mut self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        let spawned = std::process::Command::new(&self.docker)
            .args(["rm", "-f", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// How long a `vllm` inside WSL2 is given to end after SIGTERM before it is sent SIGKILL.
const WSL_STOP_GRACE: Duration = Duration::from_secs(10);

/// A model's `vllm` process inside a WSL2 distribution: ended when this is stopped or dropped
/// unless [`WslGuard::remove`] already did it. `wsl.exe` is only the relay to it, and killing
/// the relay does not reliably end what it started, so the process is found by its command line
/// (the `vllm` path, `serve`, the model folder and the port, so another model is never
/// matched) and signalled inside the distribution: SIGTERM first (vLLM shuts its engine and
/// workers down), SIGKILL if it is still there after the grace period. A daemon that is killed
/// leaves it running; nothing sweeps those yet.
#[derive(Debug)]
pub(crate) struct WslGuard {
    wsl: PathBuf,
    distro: String,
    /// The `pkill -f` pattern ([`crate::wsl::stop_pattern`]).
    pattern: String,
    grace: Duration,
    /// Cleared by whoever stops it, so it is stopped once.
    armed: AtomicBool,
}

impl WslGuard {
    pub(crate) fn new(wsl: PathBuf, distro: String, pattern: String) -> Self {
        Self::with_grace(wsl, distro, pattern, WSL_STOP_GRACE)
    }

    pub(crate) fn with_grace(
        wsl: PathBuf,
        distro: String,
        pattern: String,
        grace: Duration,
    ) -> Self {
        WslGuard {
            wsl,
            distro,
            pattern,
            grace,
            armed: AtomicBool::new(true),
        }
    }

    /// `wsl.exe -d <distro> --exec <tool> -f -- <pattern>` for `pkill -TERM`, `pkill -KILL` or
    /// `pgrep` (success means a match is still running).
    fn args(&self, tool: &[&str]) -> Vec<String> {
        let mut command: Vec<&str> = tool.to_vec();
        command.extend(["-f", "--", self.pattern.as_str()]);
        crate::wsl::exec_args(&self.distro, &command)
    }

    async fn run(&self, args: &[String]) -> bool {
        let mut command = Command::new(&self.wsl);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            command.creation_flags(0x0800_0000);
        }
        command.status().await.is_ok_and(|status| status.success())
    }

    /// Sends SIGTERM, waits up to the grace period for the process to go, then SIGKILL if it did
    /// not. Failures are not errors (the distribution may be shut down, so nothing runs).
    pub(crate) async fn remove(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = self.run(&self.args(&["pkill", "-TERM"])).await;
        let deadline = tokio::time::Instant::now() + self.grace;
        let pgrep = self.args(&["pgrep"]);
        loop {
            if !self.run(&pgrep).await {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        let _ = self.run(&self.args(&["pkill", "-KILL"])).await;
    }
}

impl Drop for WslGuard {
    /// Does the same on a thread of its own and does not wait for it, so dropping never blocks
    /// the runtime.
    fn drop(&mut self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        let (wsl, term, pgrep, kill, grace) = (
            self.wsl.clone(),
            self.args(&["pkill", "-TERM"]),
            self.args(&["pgrep"]),
            self.args(&["pkill", "-KILL"]),
            self.grace,
        );
        std::thread::spawn(move || {
            let run = |args: &[String]| {
                let mut command = std::process::Command::new(&wsl);
                command
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    command.creation_flags(0x0800_0000);
                }
                command.status().is_ok_and(|status| status.success())
            };
            run(&term);
            let deadline = std::time::Instant::now() + grace;
            loop {
                if !run(&pgrep) {
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(POLL);
            }
            run(&kill);
        });
    }
}

/// Whether any slot in a `/slots` reply has a speculative decoding context; `None` when the
/// reply has no slot with the field.
fn speculating_in(slots: &serde_json::Value) -> Option<bool> {
    let flags: Vec<bool> = slots
        .as_array()?
        .iter()
        .filter_map(|slot| slot["speculative"].as_bool())
        .collect();
    (!flags.is_empty()).then(|| flags.contains(&true))
}

/// Asks the OS for a free port. Another process could take it before the server binds; the
/// launch then fails and is reported.
pub(crate) fn free_port() -> Result<u16, String> {
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

    #[test]
    fn the_slots_reply_says_whether_the_server_speculates() {
        use serde_json::json;
        assert_eq!(
            speculating_in(
                &json!([{"id": 0, "speculative": true}, {"id": 1, "speculative": true}])
            ),
            Some(true)
        );
        assert_eq!(
            speculating_in(&json!([{"id": 0, "speculative": false}])),
            Some(false)
        );
        // No field, no slots, or not a list (an error body): nothing is known.
        assert_eq!(speculating_in(&json!([{"id": 0}])), None);
        assert_eq!(speculating_in(&json!([])), None);
        assert_eq!(speculating_in(&json!({"error": {"code": 501}})), None);
    }

    #[tokio::test]
    async fn a_missing_server_is_explained() {
        let launcher = ProcessLauncher::new(
            PathBuf::from("does/not/exist"),
            PathBuf::from("vllm"),
            PathBuf::from("."),
        );
        let spec = LaunchSpec {
            draft: None,
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

    /// A `docker` that appends its arguments to `log`.
    #[cfg(unix)]
    fn fake_docker(dir: &Path, log: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("docker");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn lines(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Waits for the detached removal a drop starts.
    #[cfg(unix)]
    fn wait_for_lines(log: &Path, count: usize) -> Vec<String> {
        for _ in 0..200 {
            let seen = lines(log);
            if seen.len() >= count {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        lines(log)
    }

    #[tokio::test]
    async fn an_ncm_server_answering_503_after_it_was_ready_counts_as_crashed() {
        use std::sync::atomic::AtomicUsize;
        // ncm-server after an internal error: 200 while it serves, then 503 to everything.
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let app = axum::Router::new().route(
            "/health",
            axum::routing::get(move || {
                let counted = Arc::clone(&counted);
                async move {
                    if counted.fetch_add(1, Ordering::SeqCst) < 2 {
                        (axum::http::StatusCode::OK, r#"{"status":"ok"}"#.to_string())
                    } else {
                        (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            r#"{"error":{"code":503,"message":"the model stopped after an internal error: boom","type":"unavailable_error"}}"#.to_string(),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let served = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let poisoned = Arc::new(Mutex::new(None));
        let watch = HealthWatch {
            poisoned: Arc::clone(&poisoned),
            _task: AbortOnDrop(watch_health(
                reqwest::Client::new(),
                root.clone(),
                Duration::from_millis(20),
                Arc::clone(&poisoned),
            )),
        };
        let server = ProcessServer {
            _held: None,
            watch: Some(watch),
            container: None,
            wsl: None,
            pid: None,
            child: Mutex::new(None),
            base_url: format!("{root}/v1"),
            root,
            http: reqwest::Client::new(),
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while server.exited().is_none() {
            assert!(tokio::time::Instant::now() < deadline, "503 never seen");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let why = server.exited().unwrap();
        assert!(
            why.contains("internal error") && why.contains("boom"),
            "{why}"
        );
        assert!(calls.load(Ordering::SeqCst) >= 3);
        served.abort();
    }

    #[tokio::test]
    async fn a_server_that_does_not_answer_is_not_taken_for_a_poisoned_one() {
        let poisoned = Arc::new(Mutex::new(None));
        // Nothing listens there.
        let task = watch_health(
            reqwest::Client::new(),
            "http://127.0.0.1:9".into(),
            Duration::from_millis(10),
            Arc::clone(&poisoned),
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        task.abort();
        assert!(poisoned.lock().unwrap().is_none());
    }

    #[cfg(unix)]
    fn server_with(container: Option<ContainerGuard>) -> ProcessServer {
        ProcessServer {
            _held: None,
            watch: None,
            container,
            wsl: None,
            pid: None,
            child: Mutex::new(None),
            base_url: String::new(),
            root: String::new(),
            http: reqwest::Client::new(),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_a_docker_server_removes_its_container_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = fake_docker(dir.path(), &log);
        let server = server_with(Some(ContainerGuard::new(
            docker,
            "imperium-vllm-1-0-9".into(),
        )));
        server.stop().await;
        assert_eq!(lines(&log), vec!["rm -f imperium-vllm-1-0-9".to_string()]);
        // Stopping again, and dropping afterwards, remove nothing more.
        server.stop().await;
        drop(server);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(lines(&log).len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_a_docker_server_that_was_never_stopped_removes_its_container() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = fake_docker(dir.path(), &log);
        drop(server_with(Some(ContainerGuard::new(
            docker,
            "imperium-vllm-1-1-9".into(),
        ))));
        assert_eq!(
            wait_for_lines(&log, 1),
            vec!["rm -f imperium-vllm-1-1-9".to_string()]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cancelled_start_leaves_no_container() {
        // The guard a start holds is dropped with its future when the start is cancelled.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = fake_docker(dir.path(), &log);
        let start = async {
            let _guard = ContainerGuard::new(docker, "imperium-vllm-1-2-9".into());
            std::future::pending::<()>().await;
        };
        let _ = tokio::time::timeout(Duration::from_millis(50), start).await;
        assert_eq!(
            wait_for_lines(&log, 1),
            vec!["rm -f imperium-vllm-1-2-9".to_string()]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_server_without_a_container_has_nothing_to_remove() {
        server_with(None).stop().await;
    }

    /// A `wsl.exe` that appends its arguments to `log` and answers `pgrep` with the exit status
    /// in `pgrep_status` (0: still running, 1: gone).
    #[cfg(unix)]
    fn fake_wsl(dir: &Path, log: &Path, pgrep_status: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let status = dir.join("pgrep_status");
        std::fs::write(&status, pgrep_status).unwrap();
        let path = dir.join("wsl.exe");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"$@\" >> {}\ncase \"$*\" in *\" pgrep \"*) exit $(cat {});; esac\n",
                log.display(),
                status.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    const WSL_PROGRAM: &str = "/home/me/.local/share/imperium/runtimes/vllm/0.11.0/venv/bin/vllm";

    #[cfg(unix)]
    fn wsl_pattern(port: u16) -> String {
        crate::wsl::stop_pattern(WSL_PROGRAM, "/mnt/d/AI Models/Qwen", port)
    }

    #[cfg(unix)]
    fn wsl_server(guard: WslGuard) -> ProcessServer {
        ProcessServer {
            _held: None,
            wsl: Some(guard),
            ..server_with(None)
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_a_wsl_server_ends_the_process_inside_the_distro_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let wsl = fake_wsl(dir.path(), &log, "1");
        let pattern = wsl_pattern(41000);
        let server = wsl_server(WslGuard::new(wsl, "Ubuntu".into(), pattern.clone()));
        server.stop().await;
        // SIGTERM, then a look: nothing is left, so no SIGKILL.
        assert_eq!(
            lines(&log),
            vec![
                format!("-d Ubuntu --exec pkill -TERM -f -- {pattern}"),
                format!("-d Ubuntu --exec pgrep -f -- {pattern}"),
            ]
        );
        server.stop().await;
        drop(server);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(lines(&log).len(), 2, "stopped once");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_wsl_process_that_survives_sigterm_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let wsl = fake_wsl(dir.path(), &log, "0");
        let pattern = wsl_pattern(41001);
        let guard = WslGuard::with_grace(wsl, "Ubuntu".into(), pattern.clone(), Duration::ZERO);
        wsl_server(guard).stop().await;
        assert_eq!(
            lines(&log),
            vec![
                format!("-d Ubuntu --exec pkill -TERM -f -- {pattern}"),
                format!("-d Ubuntu --exec pgrep -f -- {pattern}"),
                format!("-d Ubuntu --exec pkill -KILL -f -- {pattern}"),
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_a_wsl_server_that_was_never_stopped_ends_its_process() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let wsl = fake_wsl(dir.path(), &log, "1");
        let pattern = wsl_pattern(41002);
        drop(wsl_server(WslGuard::new(
            wsl,
            "Ubuntu".into(),
            pattern.clone(),
        )));
        assert_eq!(
            wait_for_lines(&log, 2),
            vec![
                format!("-d Ubuntu --exec pkill -TERM -f -- {pattern}"),
                format!("-d Ubuntu --exec pgrep -f -- {pattern}"),
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_wsl_start_runs_wsl_with_exec_and_a_translated_folder_and_cleans_up_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let wsl = fake_wsl(dir.path(), &log, "1");
        let launcher = ProcessLauncher::new(
            PathBuf::from("llama-server"),
            PathBuf::from("vllm"),
            dir.path().join("logs"),
        )
        .with_wsl_vllm(wsl, "Ubuntu".into(), WSL_PROGRAM.into());
        let spec = LaunchSpec {
            draft: None,
            backend: Arc::new(crate::backend::Vllm::default()),
            format: crate::scan::Format::Safetensors,
            gpu_memory_utilization: 0.5,
            model_id: "qwen".into(),
            model_path: PathBuf::from(r"D:\AI Models\Qwen"),
            mmproj: None,
            context: 4096,
            pool_tokens: 4096,
            slots: 2,
            gpu_layers: 0,
            cpu_only: false,
            cache_type_k: "f16".into(),
            cache_type_v: "f16".into(),
            flash_attn: "auto".into(),
            cache_ram_mb: 0,
            slot_save_path: None,
            // An attempt to open the server to the network is rewritten.
            extra_args: vec!["--host".into(), "0.0.0.0".into()],
        };
        // The fake exits at once, which is a server that died while loading.
        let error = launcher.launch(&spec).await.err().unwrap();
        assert!(error.message.contains("exited while loading"), "{error}");
        assert!(error.message.contains("setup --wsl"), "{error}");
        let seen = lines(&log);
        let start = &seen[0];
        assert!(
            start.starts_with(&format!(
                "-d Ubuntu --exec env VLLM_NO_USAGE_STATS=1 DO_NOT_TRACK=1 VLLM_HOST_IP=127.0.0.1 {WSL_PROGRAM} serve /mnt/d/AI Models/Qwen "
            )),
            "{start}"
        );
        assert!(!start.contains("0.0.0.0"), "{start}");
        assert!(start.contains(" --host 127.0.0.1 --port "), "{start}");
        // The failed start ended whatever the relay left inside the distribution.
        assert!(
            seen.iter().any(|line| line.contains("pkill -TERM -f -- ")
                && line.contains("serve /mnt/d/AI Models/Qwen .* --port")),
            "{seen:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_wsl_start_with_a_network_folder_is_refused_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let wsl = fake_wsl(dir.path(), &log, "1");
        let launcher = ProcessLauncher::new(
            PathBuf::from("llama-server"),
            PathBuf::from("vllm"),
            dir.path().join("logs"),
        )
        .with_wsl_vllm(wsl, "Ubuntu".into(), WSL_PROGRAM.into());
        let spec = LaunchSpec {
            draft: None,
            backend: Arc::new(crate::backend::Vllm::default()),
            format: crate::scan::Format::Safetensors,
            gpu_memory_utilization: 0.5,
            model_id: "nas".into(),
            model_path: PathBuf::from(r"\\nas\models\Qwen"),
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
        assert!(error.message.contains("network path"), "{error}");
        assert!(lines(&log).is_empty());
    }
}
