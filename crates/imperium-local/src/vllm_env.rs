//! The managed vLLM environment: a `uv` virtual environment with a pinned vLLM, or the official
//! Docker image, set up only when the user asks (`imperium runtime vllm setup`), never by itself.
//!
//! Layout, under `<data dir>/runtimes/vllm`:
//!
//! - `managed.json`: what was set up (mode, version, image), written last, so an interrupted
//!   setup leaves no record of a half-made environment.
//! - `<version>/venv`: the virtual environment (venv mode).
//!
//! WSL mode (native Windows) makes the same environment inside a WSL2 distribution, under
//! `~/.local/share/imperium/runtimes/vllm/<version>/venv` in the distro's own filesystem; the
//! record then also names the distro and the environment's Linux path, and every command is
//! `wsl.exe -d <distro> --exec ...` ([`crate::wsl`]).
//!
//! Every command runs without a shell through a [`Runner`], so tests use a fake one; the real
//! one ([`SystemRunner`]) merges the command's output, cuts it into short lines, and the lines
//! are shown with URL credentials and Hugging Face tokens masked ([`redact`]). No secret is
//! passed on a command line or written to `managed.json`.
//!
//! The daemon asks [`resolve`] which `vllm` to start: a `runtime.vllm.command` that is set (even
//! to `vllm`, which forces `PATH`) wins, then the managed environment, then `vllm` on `PATH`.
//! Docker mode starts each model in its own container ([`docker_run_args`]) that is removed when
//! the model stops, and containers a killed daemon left are swept at the next start
//! ([`sweep_stale_containers`]). The commands inherit the caller's environment, so `UV_*`,
//! `PIP_*` and proxy variables apply to the install (and `uv` may download a Python of its own).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use imperium_config::VllmSettings;

use crate::gpu::{Tool, run_tool_for};
use serde::{Deserialize, Serialize};

/// The command name used when the user has not chosen one.
pub const DEFAULT_COMMAND: &str = "vllm";
/// Longest a shown output line is.
const MAX_LINE: usize = 400;
/// Most files counted when measuring the environment's size.
const MAX_SIZE_ENTRIES: usize = 500_000;

fn pinned(key: &str) -> &'static str {
    include_str!("../../../runtimes/vllm.env")
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(str::trim)
        .unwrap_or_else(|| panic!("runtimes/vllm.env sets {key}"))
}

/// The vLLM release installed, pinned in `runtimes/vllm.env`.
pub fn pinned_version() -> &'static str {
    pinned("VLLM_VERSION")
}

/// The Docker image used, pinned in `runtimes/vllm.env`.
pub fn pinned_image() -> &'static str {
    pinned("VLLM_DOCKER_IMAGE")
}

/// The Python the virtual environment is made with.
pub fn pinned_python() -> &'static str {
    pinned("VLLM_PYTHON")
}

/// How vLLM is provided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Venv,
    Docker,
    /// A virtual environment inside a WSL2 distribution, for a Windows daemon.
    Wsl,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Venv => "venv",
            Mode::Docker => "docker",
            Mode::Wsl => "wsl",
        }
    }

    /// Whether this mode may be set up or started by a daemon in server mode (`server`,
    /// `daemon.server`): Docker mode starts sibling containers through the Docker socket, which
    /// is root on the host, so a server refuses it ([`DOCKER_ON_SERVER`]).
    pub fn allowed(self, server: bool) -> Result<(), String> {
        if server && self == Mode::Docker {
            Err(DOCKER_ON_SERVER.to_string())
        } else {
            Ok(())
        }
    }
}

/// Why Docker mode is refused in server mode.
pub const DOCKER_ON_SERVER: &str = "vLLM in Docker is unavailable on a server (daemon.server): it \
     needs the Docker socket, which is root on the host; run vLLM as its own compose service \
     (docker-compose.vllm.yml) or in a virtual environment instead";

/// What the user asked to set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub version: String,
    pub image: String,
    pub python: String,
    /// The WSL2 distribution, in WSL mode: `runtime.vllm.wsl_distro` when set, else chosen by
    /// the caller (the default distro) before [`setup`].
    pub distro: Option<String>,
}

impl Plan {
    /// A plan from the settings (`runtime.vllm.version`, `runtime.vllm.docker_image`), or the
    /// pinned values where they are empty; `version` overrides the setting.
    pub fn new(mode: Mode, settings: &VllmSettings, version: Option<&str>) -> Result<Self, String> {
        let pick = |chosen: Option<&str>, setting: &str, pinned: &str| {
            chosen
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .or_else(|| Some(setting.trim()).filter(|text| !text.is_empty()))
                .unwrap_or(pinned)
                .to_string()
        };
        // `--version` is for the virtual environment; the image names a Docker setup's release.
        let version = if mode == Mode::Docker { None } else { version };
        let plan = Plan {
            mode,
            version: pick(version, &settings.version, pinned_version()),
            image: pick(None, &settings.docker_image, pinned_image()),
            python: pinned_python().to_string(),
            distro: Some(settings.wsl_distro.trim())
                .filter(|name| mode == Mode::Wsl && !name.is_empty())
                .map(str::to_string),
        };
        if let Some(distro) = &plan.distro {
            crate::wsl::valid_distro(distro)?;
        }
        valid_version(&plan.version)?;
        valid_image(&plan.image)?;
        Ok(plan)
    }

    /// What setting up costs and needs, one line each, to show before asking.
    pub fn requirements(&self) -> Vec<String> {
        match self.mode {
            Mode::Venv => vec![
                format!(
                    "creates a Python {} environment with vLLM {} (installed by uv from PyPI; uv may also download Python {})",
                    self.python, self.version, self.python
                ),
                "downloads and uses about 10 to 15 GiB of disk: PyTorch, CUDA libraries and vLLM"
                    .into(),
                "needs Linux or WSL2 and an NVIDIA GPU with a CUDA 12 driver; vLLM keeps the \
                 whole model in GPU memory"
                    .into(),
                "needs uv (https://docs.astral.sh/uv/) on PATH".into(),
            ],
            Mode::Wsl => vec![
                format!(
                    "creates a Python {} environment with vLLM {} inside the WSL2 distribution {} \
                     (installed by uv from PyPI; uv may also download Python {})",
                    self.python,
                    self.version,
                    self.distro.as_deref().unwrap_or("(the default one)"),
                    self.python
                ),
                "downloads and uses about 10 to 15 GiB of disk inside the distribution's virtual \
                 disk: PyTorch, CUDA libraries and vLLM"
                    .into(),
                "needs WSL2 and an NVIDIA GPU with a Windows driver that supports WSL (CUDA 12); \
                 vLLM keeps the whole model in GPU memory"
                    .into(),
                "needs uv (https://docs.astral.sh/uv/) installed inside the distribution; \
                 Imperium does not install it"
                    .into(),
                "vLLM listens on 127.0.0.1 inside WSL2 and the daemon reaches it through WSL2's \
                 localhost forwarding"
                    .into(),
            ],
            Mode::Docker => vec![
                format!("pulls the Docker image {}", self.image),
                "downloads and uses about 10 to 20 GiB of disk".into(),
                "needs Linux or WSL2, Docker, and for NVIDIA GPUs the NVIDIA Container Toolkit; \
                 for AMD or Intel set runtime.vllm.docker_image to a build for that GPU"
                    .into(),
            ],
        }
    }
}

/// A vLLM version as pip writes it: letters, digits and `.+!_-`, not starting with `-`.
fn valid_version(version: &str) -> Result<(), String> {
    let ok = !version.is_empty()
        && version.len() <= 64
        && !version.starts_with(['-', '.'])
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+!_-".contains(c));
    ok.then_some(())
        .ok_or_else(|| format!("{version:?} is not a vLLM version such as 0.11.0"))
}

/// A Docker image reference: no spaces, not an option, only the characters references use.
fn valid_image(image: &str) -> Result<(), String> {
    let ok = !image.is_empty()
        && image.len() <= 200
        && !image.starts_with('-')
        && image
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "./:_@-".contains(c));
    ok.then_some(())
        .ok_or_else(|| format!("{image:?} is not a Docker image such as vllm/vllm-openai:v0.11.0"))
}

/// The folder everything lives in.
pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("runtimes").join("vllm")
}

fn state_path(data_dir: &Path) -> PathBuf {
    root(data_dir).join("managed.json")
}

/// The virtual environment of `version`.
pub fn venv_dir(data_dir: &Path, version: &str) -> PathBuf {
    root(data_dir).join(version).join("venv")
}

/// A program installed in a virtual environment.
pub fn venv_program(venv: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join(format!("{name}.exe"))
    } else {
        venv.join("bin").join(name)
    }
}

/// What was set up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Managed {
    pub mode: Mode,
    pub version: String,
    /// The image, in Docker mode.
    #[serde(default)]
    pub image: Option<String>,
    /// The WSL2 distribution, in WSL mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// The environment's path inside the distribution, in WSL mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venv: Option<String>,
}

/// Whether `venv` is the environment setup makes for `version` in WSL: a home folder, then
/// [`crate::wsl::HOME_SUBDIR`], the version and `venv`, so a record edited by hand cannot point
/// a removal or a launch anywhere else.
fn valid_wsl_venv(venv: &str, version: &str) -> bool {
    let tail = format!("/{}/{version}/venv", crate::wsl::HOME_SUBDIR);
    crate::wsl::valid_linux_path(venv)
        && venv
            .strip_suffix(&tail)
            .is_some_and(|home| home.starts_with('/') && home != "/")
}

/// The environment's path inside a WSL distribution whose user's home is `home`.
fn wsl_venv_path(home: &str, version: &str) -> Result<String, String> {
    let home = home.trim().trim_end_matches('/');
    let venv = format!("{home}/{}/{version}/venv", crate::wsl::HOME_SUBDIR);
    valid_wsl_venv(&venv, version)
        .then_some(venv)
        .ok_or_else(|| format!("{home:?} is not a usable home folder inside the distribution"))
}

/// The folder of one version inside the distribution (what removal deletes).
fn wsl_version_dir(venv: &str) -> &str {
    venv.strip_suffix("/venv").unwrap_or(venv)
}

/// The record as written, whatever it says; `None` when there is none or it is unreadable.
fn read_record(data_dir: &Path) -> Option<Managed> {
    let text = std::fs::read_to_string(state_path(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// What is recorded as set up; `None` when nothing is, the record is unreadable, or it names a
/// version or image that could not have been written by setup (a record edited by hand to point
/// out of the managed folder or to inject an option).
pub fn managed(data_dir: &Path) -> Option<Managed> {
    read_record(data_dir).filter(|record| {
        valid_version(&record.version).is_ok()
            && record
                .image
                .as_deref()
                .is_none_or(|image| valid_image(image).is_ok())
            && (record.mode != Mode::Wsl || wsl_record_valid(record))
    })
}

/// A WSL record names a valid distribution and the environment setup makes for its version.
fn wsl_record_valid(record: &Managed) -> bool {
    matches!((&record.distro, &record.venv), (Some(distro), Some(venv))
        if crate::wsl::valid_distro(distro).is_ok() && valid_wsl_venv(venv, &record.version))
}

fn write_managed(data_dir: &Path, managed: &Managed) -> Result<(), String> {
    let path = state_path(data_dir);
    std::fs::create_dir_all(root(data_dir)).map_err(|error| error.to_string())?;
    let text = serde_json::to_string_pretty(managed).map_err(|error| error.to_string())?;
    // Written whole and renamed, so a reader never sees half of it.
    let temp = path.with_extension("json.tmp");
    imperium_api::fs::replace_file(&path, &temp, text).map_err(|error| error.to_string())
}

/// What the daemon should start for `vllm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// This executable.
    Command(PathBuf),
    /// A container of this image, started by this `docker`.
    Docker { docker: PathBuf, image: String },
    /// `vllm` at this path inside a WSL2 distribution, started by `wsl.exe` (a Windows daemon).
    Wsl {
        wsl: PathBuf,
        distro: String,
        program: String,
    },
}

/// The `vllm` to start: a `configured` command (`runtime.vllm.command`) that is not empty is the
/// user's choice and wins, `vllm` included, which forces the one on `PATH`; else the managed
/// environment when its executable is there; else `vllm` on `PATH`.
pub fn resolve(data_dir: &Path, configured: &str) -> Launch {
    if !configured.trim().is_empty() {
        return Launch::Command(PathBuf::from(configured));
    }
    match managed(data_dir) {
        Some(Managed {
            mode: Mode::Venv,
            version,
            ..
        }) => {
            let exe = venv_program(&venv_dir(data_dir, &version), "vllm");
            if exe.is_file() {
                return Launch::Command(exe);
            }
        }
        Some(Managed {
            mode: Mode::Docker,
            image: Some(image),
            ..
        }) if valid_image(&image).is_ok() => {
            return Launch::Docker {
                docker: PathBuf::from("docker"),
                image,
            };
        }
        _ => {}
    }
    Launch::Command(PathBuf::from(DEFAULT_COMMAND))
}

/// [`resolve`] for a daemon that may run on Windows (`windows`), where vLLM runs in WSL2: the
/// managed WSL environment when `settings.command` is empty and the record says so, or a command
/// pointing into WSL (`/home/me/venv/bin/vllm`, `\\wsl$\Ubuntu\...`), in the distro the path
/// names, else `runtime.vllm.wsl_distro`, else `default_distro()` (the WSL default). Anything
/// else is [`resolve`]'s answer, which is unusable on Windows (models stay unavailable).
pub fn resolve_for(
    data_dir: &Path,
    settings: &VllmSettings,
    windows: bool,
    default_distro: &dyn Fn() -> Option<String>,
) -> Launch {
    if windows {
        let configured = settings.command.trim();
        if configured.is_empty() {
            if let Some(Managed {
                mode: Mode::Wsl,
                distro: Some(distro),
                venv: Some(venv),
                ..
            }) = managed(data_dir)
            {
                return Launch::Wsl {
                    wsl: PathBuf::from(crate::wsl::WSL_EXE),
                    distro,
                    program: format!("{venv}/bin/vllm"),
                };
            }
        } else if let Some((named, program)) = crate::wsl::command_in_wsl(configured) {
            let chosen = named
                .or_else(|| {
                    Some(settings.wsl_distro.trim().to_string()).filter(|name| !name.is_empty())
                })
                .or_else(default_distro)
                .filter(|name| crate::wsl::valid_distro(name).is_ok());
            if let Some(distro) = chosen {
                return Launch::Wsl {
                    wsl: PathBuf::from(crate::wsl::WSL_EXE),
                    distro,
                    program,
                };
            }
        }
    }
    resolve(data_dir, &settings.command)
}

/// Masks what must not be shown or logged: credentials in URLs (`https://user:pass@host`) and
/// Hugging Face tokens (`hf_...`).
pub fn redact(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        // Userinfo ends at the first `@` before any `/`, space or `?`.
        let end = tail
            .find(|c: char| c == '/' || c == '?' || c.is_whitespace())
            .unwrap_or(tail.len());
        match tail[..end].rfind('@') {
            Some(userinfo_end) => {
                out.push_str("***@");
                rest = &tail[userinfo_end + 1..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    // Hugging Face tokens.
    let mut masked = String::with_capacity(out.len());
    let mut remaining = out.as_str();
    while let Some(at) = remaining.find("hf_") {
        let (head, tail) = remaining.split_at(at);
        masked.push_str(head);
        let token_len = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .count();
        if token_len >= 3 + 16
            && head
                .chars()
                .last()
                .is_none_or(|c| !c.is_ascii_alphanumeric())
        {
            masked.push_str("hf_***");
        } else {
            masked.push_str(&tail[..token_len.max(3)]);
        }
        remaining = &tail[token_len.max(3)..];
    }
    masked.push_str(remaining);
    masked
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    Ok,
    /// The program is not installed.
    Missing,
    /// It could not start or exited unsuccessfully; why, in a few words.
    Failed(String),
}

/// Runs a command without a shell, handing its output to `on_line` as it comes.
pub trait Runner {
    fn run(&self, program: &str, args: &[String], on_line: &mut dyn FnMut(&str)) -> RunResult;
}

/// The real [`Runner`]. A command that runs longer than `timeout` is killed.
#[derive(Debug)]
pub struct SystemRunner {
    pub timeout: Duration,
}

impl Default for SystemRunner {
    fn default() -> Self {
        SystemRunner {
            // A first install downloads gigabytes; an hour is slow, not stuck.
            timeout: Duration::from_secs(60 * 60),
        }
    }
}

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String], on_line: &mut dyn FnMut(&str)) -> RunResult {
        let Ok((mut reader, writer)) = std::io::pipe() else {
            return RunResult::Failed("could not read the command's output".into());
        };
        let Ok(errors) = writer.try_clone() else {
            return RunResult::Failed("could not read the command's output".into());
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(writer))
            .stderr(Stdio::from(errors));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let spawned = command.spawn();
        // The pipe's write ends live in `command`; the reader sees the end only when they close.
        drop(command);
        let child = match spawned {
            Ok(child) => Arc::new(Mutex::new(child)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return RunResult::Missing;
            }
            Err(error) => return RunResult::Failed(format!("could not start: {error}")),
        };
        // A watchdog kills the command at the deadline, which also ends the output.
        let done = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        {
            let (child, done, timed_out) = (child.clone(), done.clone(), timed_out.clone());
            let (started, timeout) = (Instant::now(), self.timeout);
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    if started.elapsed() >= timeout {
                        timed_out.store(true, Ordering::SeqCst);
                        let _ = child.lock().unwrap_or_else(PoisonError::into_inner).kill();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }
        let mut pending = Vec::new();
        let mut buffer = [0u8; 8192];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            for &byte in &buffer[..read] {
                // Progress bars redraw with a carriage return; each redraw is a line.
                if byte == b'\n' || byte == b'\r' {
                    emit(&mut pending, on_line);
                } else if pending.len() < MAX_LINE * 4 {
                    pending.push(byte);
                }
            }
        }
        emit(&mut pending, on_line);
        let status = loop {
            let polled = child
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_wait();
            match polled {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    done.store(true, Ordering::SeqCst);
                    return RunResult::Failed(error.to_string());
                }
            }
        };
        done.store(true, Ordering::SeqCst);
        if timed_out.load(Ordering::SeqCst) {
            return RunResult::Failed(format!("timed out after {:?}", self.timeout));
        }
        if status.success() {
            RunResult::Ok
        } else {
            RunResult::Failed(format!("exited with {status}"))
        }
    }
}

fn emit(pending: &mut Vec<u8>, on_line: &mut dyn FnMut(&str)) {
    // `wsl.exe` writes its own messages in UTF-16: read as bytes they are text with a NUL between
    // the characters.
    let text = String::from_utf8_lossy(pending).replace('\0', "");
    pending.clear();
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let mut line = redact(text);
    if line.chars().count() > MAX_LINE {
        line = line.chars().take(MAX_LINE).collect::<String>() + "...";
    }
    on_line(&line);
}

/// What happens during setup or removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A step begins.
    Step(String),
    /// A line of a command's output (masked).
    Output(String),
    /// Something went wrong that did not stop the work.
    Warning(String),
}

/// Why setup stopped.
#[derive(Debug, thiserror::Error)]
#[error("{step}: {message}")]
pub struct SetupError {
    pub step: String,
    pub message: String,
    /// The last lines the failing command printed.
    pub tail: Vec<String>,
}

const TAIL_LINES: usize = 12;
/// Most output lines [`step_lines`] hands back.
const KEPT_LINES: usize = 64;

/// Runs one step, reporting it and keeping the output's last lines for a failure.
fn step(
    runner: &dyn Runner,
    on: &mut dyn FnMut(Event),
    title: &str,
    program: &str,
    args: &[String],
    missing: &str,
) -> Result<(), SetupError> {
    step_lines(runner, on, title, program, args, missing).map(|_| ())
}

/// [`step`], returning the lines the command printed (the first [`KEPT_LINES`] of them).
fn step_lines(
    runner: &dyn Runner,
    on: &mut dyn FnMut(Event),
    title: &str,
    program: &str,
    args: &[String],
    missing: &str,
) -> Result<Vec<String>, SetupError> {
    on(Event::Step(format!(
        "{title}: {} {}",
        program,
        args.join(" ")
    )));
    let mut tail: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let result = runner.run(program, args, &mut |line| {
        if tail.len() == TAIL_LINES {
            tail.remove(0);
        }
        tail.push(line.to_string());
        if kept.len() < KEPT_LINES {
            kept.push(line.to_string());
        }
        on(Event::Output(line.to_string()));
    });
    match result {
        RunResult::Ok => Ok(kept),
        RunResult::Missing => Err(SetupError {
            step: title.to_string(),
            message: missing.to_string(),
            tail: Vec::new(),
        }),
        RunResult::Failed(why) => Err(SetupError {
            step: title.to_string(),
            message: why,
            tail,
        }),
    }
}

/// Whether what is already set up stops `plan`: a different environment is made only after the
/// old one is removed, so none is left behind. The same one is not a conflict (setup then does
/// nothing).
pub fn check_existing(data_dir: &Path, plan: &Plan) -> Result<(), SetupError> {
    let Some(existing) = read_record(data_dir) else {
        return Ok(());
    };
    let same = existing.mode == plan.mode
        && match plan.mode {
            Mode::Venv => existing.version == plan.version,
            Mode::Docker => existing.image.as_deref() == Some(plan.image.as_str()),
            // Another distribution is another environment.
            Mode::Wsl => {
                existing.version == plan.version
                    && plan
                        .distro
                        .as_deref()
                        .is_none_or(|distro| existing.distro.as_deref() == Some(distro))
            }
        };
    if same {
        return Ok(());
    }
    Err(SetupError {
        step: "checking the existing environment".into(),
        message: format!(
            "vLLM {} ({}{}) is already set up; run `imperium runtime vllm remove` first to change it",
            existing.version,
            existing.mode.name(),
            existing
                .image
                .as_deref()
                .or(existing.distro.as_deref())
                .map(|detail| format!(", {detail}"))
                .unwrap_or_default()
        ),
        tail: Vec::new(),
    })
}

/// Sets up the environment of `plan` (the caller has shown [`Plan::requirements`] and asked).
/// An environment already set up at this version is left as it is. A failure removes what this
/// run made.
pub fn setup(
    runner: &dyn Runner,
    data_dir: &Path,
    plan: &Plan,
    on: &mut dyn FnMut(Event),
) -> Result<Managed, SetupError> {
    check_existing(data_dir, plan)?;
    match plan.mode {
        Mode::Venv => setup_venv(runner, data_dir, plan, on),
        Mode::Docker => setup_docker(runner, data_dir, plan, on),
        Mode::Wsl => setup_wsl(runner, data_dir, plan, on),
    }
}

fn setup_venv(
    runner: &dyn Runner,
    data_dir: &Path,
    plan: &Plan,
    on: &mut dyn FnMut(Event),
) -> Result<Managed, SetupError> {
    let record = Managed {
        mode: Mode::Venv,
        version: plan.version.clone(),
        image: None,
        distro: None,
        venv: None,
    };
    let venv = venv_dir(data_dir, &plan.version);
    let exe = venv_program(&venv, "vllm");
    if managed(data_dir).as_ref() == Some(&record) && exe.is_file() {
        on(Event::Step(format!(
            "vLLM {} is already set up at {}",
            plan.version,
            venv.display()
        )));
        return Ok(record);
    }
    step(
        runner,
        on,
        "checking uv",
        "uv",
        &["--version".into()],
        "uv is not installed; install it from https://docs.astral.sh/uv/ and run this again",
    )?;
    let fail = |error: SetupError, venv: &Path, on: &mut dyn FnMut(Event)| {
        // Only what this run made, and only inside the managed folder.
        if venv.starts_with(root(data_dir)) && std::fs::remove_dir_all(venv).is_ok() {
            on(Event::Warning("removed the unfinished environment".into()));
        }
        error
    };
    // A leftover of an earlier failed run is made fresh.
    if venv.exists() {
        let _ = std::fs::remove_dir_all(&venv);
    }
    let created = step(
        runner,
        on,
        "creating the environment",
        "uv",
        &[
            "venv".into(),
            "--python".into(),
            plan.python.clone(),
            venv.display().to_string(),
        ],
        "uv is not installed",
    );
    if let Err(error) = created {
        return Err(fail(error, &venv, on));
    }
    let installed = step(
        runner,
        on,
        "installing vLLM (this downloads several GiB and can take a while)",
        "uv",
        &[
            "pip".into(),
            "install".into(),
            "--python".into(),
            venv_program(&venv, "python").display().to_string(),
            format!("vllm=={}", plan.version),
        ],
        "uv is not installed",
    );
    if let Err(error) = installed {
        return Err(fail(error, &venv, on));
    }
    let checked = step(
        runner,
        on,
        "checking vLLM",
        &exe.display().to_string(),
        &["--version".into()],
        "vLLM was not installed",
    );
    if let Err(error) = checked {
        return Err(fail(error, &venv, on));
    }
    if let Err(message) = write_managed(data_dir, &record) {
        return Err(fail(
            SetupError {
                step: "recording the environment".into(),
                message,
                tail: Vec::new(),
            },
            &venv,
            on,
        ));
    }
    Ok(record)
}

const WSL_MISSING: &str =
    "wsl.exe is not installed; install WSL with `wsl --install` and run this again";

fn setup_error(step: &str, message: impl Into<String>) -> SetupError {
    SetupError {
        step: step.to_string(),
        message: message.into(),
        tail: Vec::new(),
    }
}

/// Runs each of `candidates` (a program inside `distro`) with `args` until one succeeds, and
/// returns it. `Err(None)` when none did; `Err(Some(_))` when `wsl.exe` itself is missing.
fn first_that_runs(
    runner: &dyn Runner,
    on: &mut dyn FnMut(Event),
    title: &str,
    distro: &str,
    candidates: &[String],
    args: &[&str],
) -> Result<String, Option<SetupError>> {
    for candidate in candidates {
        let command: Vec<&str> = std::iter::once(candidate.as_str())
            .chain(args.iter().copied())
            .collect();
        let wsl_args = crate::wsl::exec_args(distro, &command);
        match step(
            runner,
            on,
            title,
            crate::wsl::WSL_EXE,
            &wsl_args,
            WSL_MISSING,
        ) {
            Ok(()) => return Ok(candidate.clone()),
            Err(error) if error.message == WSL_MISSING => return Err(Some(error)),
            Err(_) => {}
        }
    }
    Err(None)
}

/// Removes `dir` inside `distro`; whether it worked.
fn remove_wsl_dir(runner: &dyn Runner, distro: &str, dir: &str) -> bool {
    let args = crate::wsl::exec_args(distro, &["rm", "-rf", "--", dir]);
    runner.run(crate::wsl::WSL_EXE, &args, &mut |_| {}) == RunResult::Ok
}

fn setup_wsl(
    runner: &dyn Runner,
    data_dir: &Path,
    plan: &Plan,
    on: &mut dyn FnMut(Event),
) -> Result<Managed, SetupError> {
    let wsl = crate::wsl::WSL_EXE;
    let distro = plan.distro.clone().ok_or_else(|| {
        setup_error(
            "choosing the distribution",
            "no WSL distribution was chosen; set runtime.vllm.wsl_distro",
        )
    })?;
    crate::wsl::valid_distro(&distro)
        .map_err(|message| setup_error("choosing the distribution", message))?;
    // The distribution answers, and where its user's home is: `--exec` has no `~`.
    let home = step_lines(
        runner,
        on,
        "checking the WSL distribution",
        wsl,
        &crate::wsl::exec_args(&distro, &["printenv", "HOME"]),
        WSL_MISSING,
    )
    .map_err(|mut error| {
        if error.message != WSL_MISSING {
            error.message = format!(
                "{} (is {distro} an installed WSL2 distribution? see `wsl.exe -l -v`)",
                error.message
            );
        }
        error
    })?
    .into_iter()
    .find(|line| line.trim().starts_with('/'))
    .unwrap_or_default();
    let venv = wsl_venv_path(&home, &plan.version)
        .map_err(|message| setup_error("checking the WSL distribution", message))?;
    let home = home.trim().trim_end_matches('/').to_string();
    let record = Managed {
        mode: Mode::Wsl,
        version: plan.version.clone(),
        image: None,
        distro: Some(distro.clone()),
        venv: Some(venv.clone()),
    };
    let exe = format!("{venv}/bin/vllm");
    if managed(data_dir).as_ref() == Some(&record)
        && runner.run(
            wsl,
            &crate::wsl::exec_args(&distro, &["test", "-x", &exe]),
            &mut |_| {},
        ) == RunResult::Ok
    {
        on(Event::Step(format!(
            "vLLM {} is already set up at {venv} in {distro}",
            plan.version
        )));
        return Ok(record);
    }
    // `uv` is looked for where its installer puts it as well as on the PATH `--exec` has, which
    // is not a login shell's. Imperium does not install it.
    let uv = first_that_runs(
        runner,
        on,
        "checking uv",
        &distro,
        &[
            "uv".to_string(),
            format!("{home}/.local/bin/uv"),
            format!("{home}/.cargo/bin/uv"),
        ],
        &["--version"],
    )
    .map_err(|error| {
        error.unwrap_or_else(|| {
            setup_error(
                "checking uv",
                format!(
                    "uv is not installed in {distro}; install it there yourself (inside the \
                     distribution run `curl -LsSf https://astral.sh/uv/install.sh | sh`, see \
                     https://docs.astral.sh/uv/) and run this again; Imperium does not install it"
                ),
            )
        })
    })?;
    first_that_runs(
        runner,
        on,
        "checking the GPU",
        &distro,
        &[
            "nvidia-smi".to_string(),
            "/usr/lib/wsl/lib/nvidia-smi".to_string(),
        ],
        &["-L"],
    )
    .map_err(|error| {
        error.unwrap_or_else(|| {
            setup_error(
                "checking the GPU",
                format!(
                    "no NVIDIA GPU is visible inside {distro}: vLLM needs WSL2 and an NVIDIA \
                     driver for Windows that supports WSL (do not install a Linux display driver \
                     inside WSL); `nvidia-smi` must work inside the distribution"
                ),
            )
        })
    })?;
    let fail = |error: SetupError, on: &mut dyn FnMut(Event)| {
        if remove_wsl_dir(runner, &distro, &venv) {
            on(Event::Warning("removed the unfinished environment".into()));
        }
        error
    };
    // A leftover of an earlier failed run is made fresh.
    remove_wsl_dir(runner, &distro, &venv);
    let created = step(
        runner,
        on,
        "creating the environment",
        wsl,
        &crate::wsl::exec_args(&distro, &[&uv, "venv", "--python", &plan.python, &venv]),
        WSL_MISSING,
    );
    if let Err(error) = created {
        return Err(fail(error, on));
    }
    let python = format!("{venv}/bin/python");
    let requirement = format!("vllm=={}", plan.version);
    let installed = step(
        runner,
        on,
        "installing vLLM (this downloads several GiB and can take a while)",
        wsl,
        &crate::wsl::exec_args(
            &distro,
            &[&uv, "pip", "install", "--python", &python, &requirement],
        ),
        WSL_MISSING,
    );
    if let Err(error) = installed {
        return Err(fail(error, on));
    }
    let checked = step(
        runner,
        on,
        "checking vLLM",
        wsl,
        &crate::wsl::exec_args(&distro, &[&exe, "--version"]),
        WSL_MISSING,
    );
    if let Err(error) = checked {
        return Err(fail(error, on));
    }
    if let Err(message) = write_managed(data_dir, &record) {
        return Err(fail(setup_error("recording the environment", message), on));
    }
    Ok(record)
}

fn setup_docker(
    runner: &dyn Runner,
    data_dir: &Path,
    plan: &Plan,
    on: &mut dyn FnMut(Event),
) -> Result<Managed, SetupError> {
    let record = Managed {
        mode: Mode::Docker,
        version: plan.version.clone(),
        image: Some(plan.image.clone()),
        distro: None,
        venv: None,
    };
    step(
        runner,
        on,
        "checking Docker",
        "docker",
        &["--version".into()],
        "Docker is not installed or not on PATH; install it and run this again",
    )?;
    step(
        runner,
        on,
        "pulling the image (several GiB)",
        "docker",
        &["pull".into(), plan.image.clone()],
        "Docker is not installed",
    )?;
    write_managed(data_dir, &record).map_err(|message| SetupError {
        step: "recording the environment".into(),
        message,
        tail: Vec::new(),
    })?;
    Ok(record)
}

/// What is set up now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub managed: Option<Managed>,
    /// The environment's executable exists (venv mode); Docker mode cannot be checked without
    /// asking Docker, which status does not do.
    pub present: bool,
    /// The virtual environment's size on disk.
    pub size_bytes: Option<u64>,
    pub path: Option<PathBuf>,
}

pub fn status(data_dir: &Path) -> Status {
    let Some(record) = managed(data_dir) else {
        return Status {
            managed: None,
            present: false,
            size_bytes: None,
            path: None,
        };
    };
    if record.mode == Mode::Venv {
        let venv = venv_dir(data_dir, &record.version);
        Status {
            present: venv_program(&venv, "vllm").is_file(),
            size_bytes: Some(dir_size(&venv)),
            path: Some(venv),
            managed: Some(record),
        }
    } else if record.mode == Mode::Wsl {
        // Inside the distribution: [`status_checked`] asks it.
        Status {
            present: true,
            size_bytes: None,
            path: record.venv.as_deref().map(PathBuf::from),
            managed: Some(record),
        }
    } else {
        Status {
            present: true,
            size_bytes: None,
            path: None,
            managed: Some(record),
        }
    }
}

/// [`status`], and in WSL mode whether the environment's `vllm` is there, asked of the distribution
/// (which starts it if it is stopped).
pub fn status_checked(runner: &dyn Runner, data_dir: &Path) -> Status {
    let mut status = status(data_dir);
    if let Some(Managed {
        mode: Mode::Wsl,
        distro: Some(distro),
        venv: Some(venv),
        ..
    }) = &status.managed
    {
        let exe = format!("{venv}/bin/vllm");
        let args = crate::wsl::exec_args(distro, &["test", "-x", &exe]);
        status.present = runner.run(crate::wsl::WSL_EXE, &args, &mut |_| {}) == RunResult::Ok;
    }
    status
}

/// Bytes of the files under `dir`, counting at most [`MAX_SIZE_ENTRIES`] of them.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            seen += 1;
            if seen > MAX_SIZE_ENTRIES {
                return total;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata().map_or(0, |meta| meta.len()));
            }
        }
    }
    total
}

/// Removes the managed environment: the virtual environment's folder, or the Docker image, and
/// the record. Nothing else is touched.
pub fn remove(
    runner: &dyn Runner,
    data_dir: &Path,
    on: &mut dyn FnMut(Event),
) -> Result<Option<Managed>, String> {
    let Some(record) = read_record(data_dir) else {
        return Ok(None);
    };
    // The record is a file anyone with access to the data folder could edit: what it names is
    // checked before anything is deleted or run.
    valid_version(&record.version).map_err(|error| {
        format!(
            "{error}; {} was not written by setup, so nothing was removed (delete it by hand if it is wrong)",
            state_path(data_dir).display()
        )
    })?;
    if let Some(image) = &record.image {
        valid_image(image).map_err(|error| {
            format!(
                "{error}; {} was not written by setup, so nothing was removed",
                state_path(data_dir).display()
            )
        })?;
    }
    if record.mode == Mode::Wsl && !wsl_record_valid(&record) {
        return Err(format!(
            "the WSL distribution or environment path in {} was not written by setup, so \
             nothing was removed (delete it by hand if it is wrong)",
            state_path(data_dir).display()
        ));
    }
    match record.mode {
        Mode::Wsl => {
            if let (Some(distro), Some(venv)) = (&record.distro, &record.venv) {
                let dir = wsl_version_dir(venv);
                on(Event::Step(format!("removing {dir} in {distro}")));
                if !remove_wsl_dir(runner, distro, dir) {
                    on(Event::Warning(format!(
                        "could not remove {dir} in {distro} (is the distribution still installed?); \
                         remove it there with `rm -rf {dir}`"
                    )));
                }
            }
        }
        Mode::Venv => {
            let version_dir = root(data_dir).join(&record.version);
            on(Event::Step(format!("removing {}", version_dir.display())));
            if version_dir.exists() {
                std::fs::remove_dir_all(&version_dir).map_err(|error| {
                    format!("could not remove {}: {error}", version_dir.display())
                })?;
            }
        }
        Mode::Docker => {
            if let Some(image) = record.image.as_deref() {
                on(Event::Step(format!("removing the image {image}")));
                let outcome =
                    runner.run("docker", &["rmi".into(), image.to_string()], &mut |line| {
                        on(Event::Output(line.to_string()));
                    });
                if outcome != RunResult::Ok {
                    on(Event::Warning(format!(
                        "docker could not remove {image} ({outcome:?}); remove it with `docker rmi {image}`"
                    )));
                }
            }
        }
    }
    std::fs::remove_file(state_path(data_dir))
        .map_err(|error| format!("could not remove the record: {error}"))?;
    Ok(Some(record))
}

/// The arguments of `docker run` for one vLLM model: `serve_args` are the `vllm serve` ones
/// ([`crate::backend::Vllm`]'s launch args, without the port), whose model folder is mounted
/// read-only and whose host is opened inside the container; the port is published on the
/// loopback only. The container is named, so it can be removed when the model stops.
pub fn docker_run_args(
    image: &str,
    name: &str,
    port: u16,
    scope: &str,
    serve_args: &[String],
) -> Result<Vec<String>, String> {
    valid_image(image)?;
    valid_scope(scope)?;
    let [serve, folder, rest @ ..] = serve_args else {
        return Err("no model folder to serve".into());
    };
    if serve != "serve" {
        return Err("expected vllm serve arguments".into());
    }
    if folder.contains(',') || folder.contains('\n') {
        return Err(format!("{folder:?} cannot be mounted into a container"));
    }
    // `--pull=never`: only the image `setup --docker` pulled is used, never one fetched while a
    // model loads. `--gpus all` and `--ipc=host` are what vLLM's own instructions use (they give
    // the container every GPU and the host's shared memory); `no-new-privileges` keeps the
    // process from gaining more. Dropping capabilities (`--cap-drop=ALL`) is not done: it is not
    // known to work with the NVIDIA runtime and vLLM.
    let mut args: Vec<String> = [
        "run",
        "--rm",
        "--pull=never",
        "--name",
        name,
        "--gpus",
        "all",
        "--ipc=host",
        "--security-opt",
        "no-new-privileges",
        "-e",
        "VLLM_NO_USAGE_STATS=1",
        "-e",
        "DO_NOT_TRACK=1",
        "--label",
        &format!("{SCOPE_LABEL}={scope}"),
        "--label",
        &format!("{PID_LABEL}={}", std::process::id()),
        "-p",
    ]
    .map(String::from)
    .to_vec();
    args.push(format!("127.0.0.1:{port}:{port}"));
    args.push("--mount".into());
    args.push(format!(
        "type=bind,source={folder},target=/models/model,readonly"
    ));
    args.extend(["--entrypoint".into(), "vllm".into(), image.to_string()]);
    args.push("serve".into());
    args.push("/models/model".into());
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        args.push(arg.clone());
        // Inside the container the server must listen on all its interfaces.
        if arg == "--host" && rest.next().is_some() {
            args.push("0.0.0.0".into());
        }
    }
    args.push("--port".into());
    args.push(port.to_string());
    Ok(args)
}

/// The prefix of every container name, for finding the ones a killed daemon left.
pub const CONTAINER_PREFIX: &str = "imperium-vllm-";

/// A container name: this process's id, a counter and the port, so two daemons or two starts on
/// one port never share one.
pub fn container_name(pid: u32, seq: u64, port: u16) -> String {
    format!("{CONTAINER_PREFIX}{pid}-{seq}-{port}")
}

/// A name no other container of this process has had.
pub fn fresh_container_name(port: u16) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    container_name(
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        port,
    )
}

/// The process id in a container name made by [`container_name`].
fn container_owner(name: &str) -> Option<u32> {
    name.strip_prefix(CONTAINER_PREFIX)?
        .split('-')
        .next()?
        .parse()
        .ok()
}

/// The label naming the host and data folder a container belongs to.
pub const SCOPE_LABEL: &str = "imperium.scope";
/// The label naming the daemon (process id) that started a container.
pub const PID_LABEL: &str = "imperium.pid";
/// Longest each Docker call of the sweep may take.
pub const SWEEP_TIMEOUT: Duration = Duration::from_secs(10);

/// What a container belongs to: this host and this data folder (a short hash of its path), so a
/// sweep never touches another user's, another data folder's or another machine's containers.
pub fn scope(data_dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let host: String = sysinfo::System::host_name()
        .unwrap_or_else(|| "unknown".into())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(63)
        .collect();
    let digest = Sha256::digest(data_dir.to_string_lossy().as_bytes());
    let hash: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{host}:{hash}")
}

/// A scope as [`scope`] makes it: it goes into a command line and a filter.
fn valid_scope(scope: &str) -> Result<(), String> {
    let ok = !scope.is_empty()
        && scope.len() <= 100
        && scope
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c));
    ok.then_some(())
        .ok_or_else(|| format!("{scope:?} is not a container scope"))
}

/// Removes the containers a daemon that was killed left behind. Only containers labelled with
/// this host and data folder's `scope` are listed, and of those only the ones whose owner process
/// (`is_alive`) is gone and is not `own_pid`; a pid is only meaningful on the host that made it,
/// which the scope guarantees as long as the daemon and Docker share one pid namespace (a Docker
/// reached through `DOCKER_HOST` may not, so with `remote` set nothing is swept). Each Docker call
/// is killed after `timeout`. Returns the names removed. Failures (no Docker, no permission, a
/// timeout) remove nothing.
pub fn sweep_stale_containers(
    docker: &Path,
    scope: &str,
    own_pid: u32,
    is_alive: &dyn Fn(u32) -> bool,
    remote: bool,
    timeout: Duration,
) -> Vec<String> {
    if remote || valid_scope(scope).is_err() {
        return Vec::new();
    }
    let Some(docker) = docker.to_str() else {
        return Vec::new();
    };
    let label = format!("label={SCOPE_LABEL}={scope}");
    let name = format!("name=^{CONTAINER_PREFIX}");
    let listing = [
        "ps",
        "-a",
        "--filter",
        label.as_str(),
        "--filter",
        name.as_str(),
        "--format",
        "{{.Names}}",
    ];
    let Tool::Ran(listed) = run_tool_for(docker, &listing, timeout) else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    for name in listed.lines().take(256) {
        let name = name.trim();
        let Some(owner) = container_owner(name) else {
            continue;
        };
        if owner == own_pid || is_alive(owner) {
            continue;
        }
        if matches!(
            run_tool_for(docker, &["rm", "-f", name], timeout),
            Tool::Ran(_)
        ) {
            removed.push(name.to_string());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn a_server_refuses_docker_mode_and_nothing_else() {
        let error = Mode::Docker.allowed(true).unwrap_err();
        assert!(error.contains("server"), "{error}");
        assert!(Mode::Docker.allowed(false).is_ok());
        assert!(Mode::Venv.allowed(true).is_ok());
        assert!(Mode::Wsl.allowed(true).is_ok());
    }

    /// A runner that records what it was asked and answers from a script; it can make the files
    /// a real `uv` would.
    struct Fake {
        calls: RefCell<Vec<String>>,
        /// `(program, first argument)` that fails, or is missing.
        fail: Option<(&'static str, &'static str)>,
        missing: Option<&'static str>,
        venv: PathBuf,
    }

    impl Fake {
        fn new(venv: PathBuf) -> Self {
            Fake {
                calls: RefCell::new(Vec::new()),
                fail: None,
                missing: None,
                venv,
            }
        }
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[String], on_line: &mut dyn FnMut(&str)) -> RunResult {
            self.calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            if self.missing == Some(program) {
                return RunResult::Missing;
            }
            let first = args.first().map(String::as_str).unwrap_or("");
            on_line(&format!("{program} {first} output"));
            if self.fail == Some((program_name(program), first)) {
                on_line("error: no matching distribution");
                return RunResult::Failed("exited with code 1".into());
            }
            if program == "uv" && first == "venv" {
                std::fs::create_dir_all(venv_program(&self.venv, "python").parent().unwrap())
                    .unwrap();
            }
            if program == "uv" && first == "pip" {
                std::fs::write(venv_program(&self.venv, "vllm"), "").unwrap();
            }
            RunResult::Ok
        }
    }

    fn program_name(program: &str) -> &str {
        if program.ends_with("vllm") || program.ends_with("vllm.exe") {
            "vllm"
        } else {
            program
        }
    }

    fn plan(mode: Mode) -> Plan {
        Plan::new(mode, &VllmSettings::default(), None).unwrap()
    }

    fn events(on: &RefCell<Vec<Event>>) -> impl FnMut(Event) + '_ {
        move |event| on.borrow_mut().push(event)
    }

    #[test]
    fn the_pins_are_read() {
        assert!(pinned_version().chars().next().unwrap().is_ascii_digit());
        assert!(pinned_image().contains(pinned_version()));
        assert_eq!(pinned_python(), "3.12");
        let plan = plan(Mode::Venv);
        assert_eq!(plan.version, pinned_version());
        assert_eq!(plan.image, pinned_image());
    }

    #[test]
    fn settings_and_a_flag_override_the_pins_and_bad_values_are_refused() {
        let settings = VllmSettings {
            version: "0.10.2".into(),
            docker_image: "registry.example/vllm:rocm".into(),
            ..VllmSettings::default()
        };
        let plan = Plan::new(Mode::Venv, &settings, None).unwrap();
        assert_eq!(plan.version, "0.10.2");
        assert_eq!(plan.image, "registry.example/vllm:rocm");
        let flagged = Plan::new(Mode::Venv, &settings, Some("0.9.0")).unwrap();
        assert_eq!(flagged.version, "0.9.0");
        for bad in ["--upgrade", "0.1 && rm", "", "a b", ".x"] {
            assert!(
                Plan::new(Mode::Venv, &settings, Some(bad)).is_err() || bad.is_empty(),
                "{bad}"
            );
        }
        let bad_image = VllmSettings {
            docker_image: "-v /:/host".into(),
            ..VllmSettings::default()
        };
        assert!(Plan::new(Mode::Docker, &bad_image, None).is_err());
    }

    #[test]
    fn requirements_name_disk_and_gpu() {
        let venv = plan(Mode::Venv).requirements().join("\n");
        assert!(
            venv.contains("GiB") && venv.contains("NVIDIA") && venv.contains("uv"),
            "{venv}"
        );
        let docker = plan(Mode::Docker).requirements().join("\n");
        assert!(
            docker.contains(pinned_image()) && docker.contains("Container Toolkit"),
            "{docker}"
        );
    }

    #[test]
    fn setup_makes_the_environment_with_uv_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(Mode::Venv);
        let venv = venv_dir(dir.path(), &plan.version);
        let fake = Fake::new(venv.clone());
        let log = RefCell::new(Vec::new());
        let record = setup(&fake, dir.path(), &plan, &mut events(&log)).unwrap();
        assert_eq!(record.mode, Mode::Venv);
        let calls = fake.calls.borrow();
        assert_eq!(calls[0], "uv --version");
        assert!(
            calls[1].starts_with("uv venv --python 3.12 "),
            "{}",
            calls[1]
        );
        assert!(
            calls[2].starts_with("uv pip install --python "),
            "{}",
            calls[2]
        );
        assert!(
            calls[2].ends_with(&format!("vllm=={}", plan.version)),
            "{}",
            calls[2]
        );
        assert!(calls[3].ends_with("--version"), "{}", calls[3]);
        assert_eq!(calls.len(), 4);
        // The record and the executable are there, and resolve picks them.
        assert_eq!(managed(dir.path()), Some(record));
        assert_eq!(
            resolve(dir.path(), ""),
            Launch::Command(venv_program(&venv, "vllm"))
        );
        // A command the user chose wins over the managed environment.
        assert_eq!(
            resolve(dir.path(), "/opt/vllm"),
            Launch::Command(PathBuf::from("/opt/vllm"))
        );
        assert!(log.borrow().iter().any(|e| matches!(e, Event::Output(_))));
        let status = status(dir.path());
        assert!(status.present);
        assert_eq!(status.managed.unwrap().version, plan.version);
        // Running it again changes nothing and runs nothing.
        let again = Fake::new(venv);
        setup(&again, dir.path(), &plan, &mut |_| {}).unwrap();
        assert!(again.calls.borrow().is_empty());
    }

    #[test]
    fn a_failed_install_cleans_up_and_shows_the_end_of_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(Mode::Venv);
        let venv = venv_dir(dir.path(), &plan.version);
        let mut fake = Fake::new(venv.clone());
        fake.fail = Some(("uv", "pip"));
        let log = RefCell::new(Vec::new());
        let error = setup(&fake, dir.path(), &plan, &mut events(&log)).unwrap_err();
        assert!(error.step.contains("installing vLLM"), "{error}");
        assert!(
            error
                .tail
                .iter()
                .any(|l| l.contains("no matching distribution")),
            "{error:?}"
        );
        assert!(!venv.exists(), "the unfinished environment is removed");
        assert_eq!(managed(dir.path()), None);
        assert!(log.borrow().iter().any(|e| matches!(e, Event::Warning(_))));
        // Nothing is recorded, so resolve keeps using `vllm` on PATH.
        assert_eq!(
            resolve(dir.path(), ""),
            Launch::Command(PathBuf::from("vllm"))
        );
    }

    #[test]
    fn a_missing_uv_says_how_to_get_it_and_makes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(Mode::Venv);
        let mut fake = Fake::new(venv_dir(dir.path(), &plan.version));
        fake.missing = Some("uv");
        let error = setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap_err();
        assert!(error.message.contains("docs.astral.sh/uv"), "{error}");
        assert!(!root(dir.path()).exists());
    }

    #[test]
    fn docker_mode_pulls_the_image_and_remove_takes_it_away() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(Mode::Docker);
        let fake = Fake::new(dir.path().join("unused"));
        let record = setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap();
        assert_eq!(record.image.as_deref(), Some(pinned_image()));
        assert_eq!(
            *fake.calls.borrow(),
            vec![
                "docker --version".to_string(),
                format!("docker pull {}", pinned_image())
            ]
        );
        assert_eq!(
            resolve(dir.path(), ""),
            Launch::Docker {
                docker: PathBuf::from("docker"),
                image: pinned_image().into()
            }
        );
        let removal = Fake::new(dir.path().join("unused"));
        let removed = remove(&removal, dir.path(), &mut |_| {}).unwrap();
        assert_eq!(removed, Some(record));
        assert_eq!(
            *removal.calls.borrow(),
            vec![format!("docker rmi {}", pinned_image())]
        );
        assert_eq!(managed(dir.path()), None);
        // Nothing left to remove.
        assert_eq!(remove(&removal, dir.path(), &mut |_| {}).unwrap(), None);
    }

    #[test]
    fn remove_deletes_the_environment_and_only_that() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(Mode::Venv);
        let fake = Fake::new(venv_dir(dir.path(), &plan.version));
        setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap();
        let other = dir.path().join("runtimes").join("llama.cpp");
        std::fs::create_dir_all(&other).unwrap();
        let removal = Fake::new(dir.path().join("unused"));
        remove(&removal, dir.path(), &mut |_| {}).unwrap();
        assert!(
            removal.calls.borrow().is_empty(),
            "venv removal runs no command"
        );
        assert!(!root(dir.path()).join(&plan.version).exists());
        assert!(other.exists());
        assert!(!status(dir.path()).present);
    }

    #[test]
    fn a_record_with_a_missing_executable_resolves_to_path() {
        let dir = tempfile::tempdir().unwrap();
        write_managed(
            dir.path(),
            &Managed {
                mode: Mode::Venv,
                version: "0.11.0".into(),
                image: None,
                distro: None,
                venv: None,
            },
        )
        .unwrap();
        assert_eq!(
            resolve(dir.path(), ""),
            Launch::Command(PathBuf::from("vllm"))
        );
        assert!(!status(dir.path()).present);
        // An unreadable record is no record.
        std::fs::write(state_path(dir.path()), "{ not json").unwrap();
        assert_eq!(managed(dir.path()), None);
        // A record naming an image that could be an option is not used.
        write_managed(
            dir.path(),
            &Managed {
                mode: Mode::Docker,
                version: "x".into(),
                image: Some("--privileged".into()),
                distro: None,
                venv: None,
            },
        )
        .unwrap();
        assert_eq!(
            resolve(dir.path(), ""),
            Launch::Command(PathBuf::from("vllm"))
        );
    }

    #[test]
    fn credentials_and_tokens_are_masked() {
        assert_eq!(
            redact("Downloading https://user:s3cret@pypi.example/simple/vllm/"),
            "Downloading https://***@pypi.example/simple/vllm/"
        );
        assert_eq!(
            redact("using https://pypi.org/simple and https://a:b@x.y/z?token=1"),
            "using https://pypi.org/simple and https://***@x.y/z?token=1"
        );
        assert_eq!(
            redact("Authorization: Bearer hf_abcdefghijklmnopqrstuvwxyz0123"),
            "Authorization: Bearer hf_***"
        );
        // Words that merely contain hf_ are not tokens.
        assert_eq!(redact("shelf_life hf_x"), "shelf_life hf_x");
        assert_eq!(redact("plain line"), "plain line");
        assert_eq!(redact("://@"), "://***@");
    }

    #[test]
    fn the_container_command_mounts_the_model_read_only_and_listens_inside() {
        let serve: Vec<String> = [
            "serve",
            "/home/u/models/Qwen3-8B",
            "--host",
            "127.0.0.1",
            "--served-model-name",
            "q",
            "--max-model-len",
            "8192",
        ]
        .map(String::from)
        .to_vec();
        let args = docker_run_args(
            "vllm/vllm-openai:v0.11.0",
            &container_name(77, 3, 8123),
            8123,
            "host-1:0123456789ab",
            &serve,
        )
        .unwrap();
        let text = args.join(" ");
        let expected = format!(
            "run --rm --pull=never --name imperium-vllm-77-3-8123 --gpus all --ipc=host --security-opt no-new-privileges -e VLLM_NO_USAGE_STATS=1 -e DO_NOT_TRACK=1 --label imperium.scope=host-1:0123456789ab --label imperium.pid={} -p 127.0.0.1:8123:8123 ",
            std::process::id()
        );
        assert!(text.starts_with(&expected), "{text}");
        assert!(
            text.contains(
                "--mount type=bind,source=/home/u/models/Qwen3-8B,target=/models/model,readonly"
            ),
            "{text}"
        );
        assert!(text.contains("--entrypoint vllm vllm/vllm-openai:v0.11.0 serve /models/model --host 0.0.0.0 --served-model-name q"), "{text}");
        assert!(text.ends_with("--max-model-len 8192 --port 8123"), "{text}");
        assert!(
            !text.contains("127.0.0.1 "),
            "the host inside is opened: {text}"
        );
        // Refusals.
        assert!(docker_run_args("--privileged", "n", 1, "s", &serve).is_err());
        assert!(docker_run_args("img", "n", 1, "s", &[]).is_err());
        assert!(docker_run_args("img", "n", 1, "bad scope", &serve).is_err());
        assert!(docker_run_args("img", "n", 1, "", &serve).is_err());
        let comma: Vec<String> = ["serve", "/a,b"].map(String::from).to_vec();
        assert!(docker_run_args("img", "n", 1, "s", &comma).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_real_runner_reads_a_commands_output_in_lines_and_tells_failure_and_absence() {
        let runner = SystemRunner::default();
        let mut lines = Vec::new();
        let args = [
            "-c".to_string(),
            "echo one; echo two 1>&2; printf 'a\\rb\\n'".to_string(),
        ];
        assert_eq!(
            runner.run("sh", &args, &mut |l| lines.push(l.to_string())),
            RunResult::Ok
        );
        assert!(
            lines.contains(&"one".to_string()) && lines.contains(&"two".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"a".to_string()) && lines.contains(&"b".to_string()),
            "{lines:?}"
        );
        let failing = ["-c".to_string(), "exit 3".to_string()];
        assert!(matches!(
            runner.run("sh", &failing, &mut |_| {}),
            RunResult::Failed(_)
        ));
        assert_eq!(
            runner.run("imperium-no-such-program", &[], &mut |_| {}),
            RunResult::Missing
        );
        // A long line is cut.
        let long = [
            "-c".to_string(),
            "head -c 5000 /dev/zero | tr '\\0' x; echo".to_string(),
        ];
        let mut seen = Vec::new();
        runner.run("sh", &long, &mut |l| seen.push(l.to_string()));
        assert!(
            seen.iter().all(|l| l.chars().count() <= MAX_LINE + 3),
            "{}",
            seen[0].len()
        );
    }

    #[test]
    fn an_explicit_vllm_command_forces_the_one_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan(Mode::Venv);
        let fake = Fake::new(venv_dir(dir.path(), &plan.version));
        setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap();
        // Unset: the managed environment. Set to `vllm`: the user asked for PATH.
        assert!(matches!(resolve(dir.path(), ""), Launch::Command(p) if p != Path::new("vllm")));
        assert_eq!(
            resolve(dir.path(), "vllm"),
            Launch::Command(PathBuf::from("vllm"))
        );
    }

    #[test]
    fn a_record_edited_to_point_elsewhere_is_ignored_and_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("runtimes").join("x");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), "").unwrap();
        for version in ["../x", "..", "a/b", "-rf", ""] {
            std::fs::create_dir_all(root(dir.path())).unwrap();
            std::fs::write(
                state_path(dir.path()),
                format!(r#"{{"mode":"venv","version":"{version}"}}"#),
            )
            .unwrap();
            assert_eq!(managed(dir.path()), None, "{version:?}");
            assert_eq!(
                resolve(dir.path(), ""),
                Launch::Command(PathBuf::from("vllm"))
            );
            assert!(!status(dir.path()).present);
            let runner = Fake::new(dir.path().join("unused"));
            let error = remove(&runner, dir.path(), &mut |_| {}).unwrap_err();
            assert!(error.contains("nothing was removed"), "{error}");
            assert!(runner.calls.borrow().is_empty());
            assert!(outside.join("keep").exists(), "{version:?}");
            assert!(
                state_path(dir.path()).exists(),
                "the record is left for the user"
            );
        }
        // An image that could be an option is refused too, and no `docker` runs.
        std::fs::write(
            state_path(dir.path()),
            r#"{"mode":"docker","version":"0.11.0","image":"--privileged"}"#,
        )
        .unwrap();
        let runner = Fake::new(dir.path().join("unused"));
        assert!(remove(&runner, dir.path(), &mut |_| {}).is_err());
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn image_references_are_checked() {
        let long_image = "a".repeat(201);
        let long_version = "1".repeat(65);
        for good in [
            "vllm/vllm-openai:v0.11.0",
            "vllm/vllm-openai",
            "registry.example.com:5000/team/vllm_rocm:6.4-rc1",
            "ghcr.io/org/img@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "a--b",
            "a__b",
        ] {
            assert!(valid_image(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "--",
            "-v",
            "--privileged",
            "a b",
            "img;rm",
            "img$(x)",
            "img\nx",
            "img`x`",
            "img'x",
            "img\"x",
            long_image.as_str(),
        ] {
            assert!(valid_image(bad).is_err(), "{bad:?}");
        }
        for good in ["0.11.0", "0.10.2.post1", "1.0.0rc1", "0.11.0+cu126"] {
            assert!(valid_version(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-1",
            ".1",
            "1 2",
            "1/2",
            "1;2",
            "../x",
            long_version.as_str(),
        ] {
            assert!(valid_version(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn setting_up_something_else_needs_the_old_one_removed_first() {
        let dir = tempfile::tempdir().unwrap();
        let venv_plan = plan(Mode::Venv);
        let fake = Fake::new(venv_dir(dir.path(), &venv_plan.version));
        setup(&fake, dir.path(), &venv_plan, &mut |_| {}).unwrap();
        // The same one is fine; another version or mode is not, and nothing runs.
        assert!(check_existing(dir.path(), &venv_plan).is_ok());
        let settings = VllmSettings::default();
        let other = Plan::new(Mode::Venv, &settings, Some("0.9.0")).unwrap();
        let docker = plan(Mode::Docker);
        for plan in [&other, &docker] {
            let runner = Fake::new(dir.path().join("unused"));
            let error = setup(&runner, dir.path(), plan, &mut |_| {}).unwrap_err();
            assert!(error.message.contains("vllm remove"), "{error}");
            assert!(runner.calls.borrow().is_empty());
        }
        // A Docker setup with another image is refused the same way.
        let other_dir = tempfile::tempdir().unwrap();
        let runner = Fake::new(other_dir.path().join("unused"));
        setup(&runner, other_dir.path(), &docker, &mut |_| {}).unwrap();
        let rocm = VllmSettings {
            docker_image: "registry.example/vllm:rocm".into(),
            ..VllmSettings::default()
        };
        let changed = Plan::new(Mode::Docker, &rocm, None).unwrap();
        assert!(check_existing(other_dir.path(), &changed).is_err());
        // `--version` does not apply to Docker.
        let flagged = Plan::new(Mode::Docker, &settings, Some("0.9.0")).unwrap();
        assert_eq!(flagged.version, pinned_version());
        assert!(check_existing(other_dir.path(), &flagged).is_ok());
    }

    #[test]
    fn container_names_are_unique_and_name_their_owner() {
        let a = fresh_container_name(8000);
        let b = fresh_container_name(8000);
        assert_ne!(a, b);
        assert_eq!(container_owner(&a), Some(std::process::id()));
        assert_eq!(container_name(7, 2, 9), "imperium-vllm-7-2-9");
        assert_eq!(container_owner("imperium-vllm-7-2-9"), Some(7));
        assert_eq!(container_owner("imperium-vllm-x-2-9"), None);
        assert_eq!(container_owner("other"), None);
    }

    /// A `docker` that appends the arguments of every call to `log` and lists the given names for
    /// `ps`; with `hang` it then sleeps for a long time instead.
    #[cfg(unix)]
    fn fake_docker(dir: &Path, names: &[&str], log: &Path, hang: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let listing: String = names.iter().map(|name| format!("echo {name}\n")).collect();
        let hang = if hang { "exec sleep 30\n" } else { "" };
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> {}\n{hang}if [ \"$1\" = ps ]; then\n{listing}fi\nexit 0\n",
            log.display()
        );
        let path = dir.join("docker");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn only_containers_of_dead_daemons_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = fake_docker(
            dir.path(),
            &[
                "imperium-vllm-999999999-0-8000",
                "imperium-vllm-555-1-8001",
                "imperium-vllm-4242-0-8002",
                "imperium-vllm-x-1-1",
                "unrelated",
            ],
            &log,
            false,
        );
        // 555 is this process, 4242 is another daemon that is still running.
        let removed = sweep_stale_containers(
            &docker,
            SCOPE,
            555,
            &|pid| pid == 4242,
            false,
            Duration::from_secs(10),
        );
        assert_eq!(removed, vec!["imperium-vllm-999999999-0-8000".to_string()]);
        // Only this host and data folder's containers are listed, then the dead one removed.
        let calls: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(
            calls,
            vec![
                format!(
                    "ps -a --filter label=imperium.scope={SCOPE} --filter name=^imperium-vllm- --format {{{{.Names}}}}"
                ),
                "rm -f imperium-vllm-999999999-0-8000".to_string(),
            ]
        );
        // No Docker, nothing swept.
        assert!(
            sweep_stale_containers(
                &dir.path().join("none"),
                SCOPE,
                1,
                &|_| false,
                false,
                Duration::from_secs(10)
            )
            .is_empty()
        );
    }

    #[cfg(unix)]
    const SCOPE: &str = "host-1:0123456789ab";

    #[cfg(unix)]
    #[test]
    fn a_docker_host_means_nothing_is_swept_and_docker_is_not_called() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = fake_docker(dir.path(), &["imperium-vllm-999999999-0-8000"], &log, false);
        let removed = sweep_stale_containers(
            &docker,
            SCOPE,
            555,
            &|_| false,
            true,
            Duration::from_secs(10),
        );
        assert!(removed.is_empty());
        assert!(!log.exists(), "docker was not run");
        // A scope that is not ours to filter by is refused too.
        assert!(
            sweep_stale_containers(&docker, "", 555, &|_| false, false, Duration::from_secs(10))
                .is_empty()
        );
        assert!(!log.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_docker_is_given_up_on_after_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let docker = fake_docker(dir.path(), &["imperium-vllm-999999999-0-8000"], &log, true);
        let started = Instant::now();
        let removed = sweep_stale_containers(
            &docker,
            SCOPE,
            555,
            &|_| false,
            false,
            Duration::from_millis(300),
        );
        assert!(removed.is_empty());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_scope_names_this_host_and_data_folder_in_command_line_safe_characters() {
        let a = scope(Path::new("/home/a/.local/share/imperium"));
        let b = scope(Path::new("/home/b/.local/share/imperium"));
        assert_ne!(a, b, "another data folder is another scope");
        assert_eq!(a, scope(Path::new("/home/a/.local/share/imperium")));
        assert!(valid_scope(&a).is_ok(), "{a}");
        let (host, hash) = a.rsplit_once(':').unwrap();
        assert!(!host.is_empty());
        assert_eq!(hash.len(), 12);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        for bad in ["", "a b", "a;b", "a=b", "x\ny"] {
            assert!(valid_scope(bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_command_over_its_timeout_is_killed() {
        let runner = SystemRunner {
            timeout: Duration::from_millis(300),
        };
        let args = ["-c".to_string(), "echo started; exec sleep 30".to_string()];
        let mut lines = Vec::new();
        let started = Instant::now();
        let result = runner.run("sh", &args, &mut |l| lines.push(l.to_string()));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            matches!(&result, RunResult::Failed(why) if why.contains("timed out")),
            "{result:?}"
        );
        assert!(lines.contains(&"started".to_string()), "{lines:?}");
    }
}

#[cfg(test)]
mod wsl_tests {
    use super::*;
    use std::cell::RefCell;

    const HOME: &str = "/home/tester";

    /// A runner that answers as `wsl.exe` would, from a script, and records every command line.
    struct WslFake {
        calls: RefCell<Vec<String>>,
        /// A command line containing one of these fails.
        fail: Vec<&'static str>,
        /// `wsl.exe` is not installed.
        no_wsl: bool,
        /// Whether `test -x` finds the environment's `vllm`; set by the install.
        installed: RefCell<bool>,
        data_dir: PathBuf,
        /// Whether `managed.json` existed when `vllm --version` ran.
        record_at_check: RefCell<Option<bool>>,
    }

    impl WslFake {
        fn new(data_dir: &Path) -> Self {
            WslFake {
                calls: RefCell::new(Vec::new()),
                fail: Vec::new(),
                no_wsl: false,
                installed: RefCell::new(false),
                data_dir: data_dir.to_path_buf(),
                record_at_check: RefCell::new(None),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Runner for WslFake {
        fn run(&self, program: &str, args: &[String], on_line: &mut dyn FnMut(&str)) -> RunResult {
            let line = format!("{program} {}", args.join(" "));
            self.calls.borrow_mut().push(line.clone());
            if self.no_wsl {
                return RunResult::Missing;
            }
            let failing = self.fail.iter().any(|f| line.contains(f));
            if line.ends_with("--exec printenv HOME") && !failing {
                // `wsl.exe` can print a warning (UTF-16 read as bytes) before the output.
                on_line("wsl: Failed to translate '\u{fffd}C:/x\u{fffd}'");
                on_line("");
                on_line(HOME);
                return RunResult::Ok;
            }
            if failing {
                on_line("error: boom");
                return RunResult::Failed("exited with code 1".into());
            }
            if line.contains("--exec test -x") && !*self.installed.borrow() {
                return RunResult::Failed("exited with code 1".into());
            }
            if line.contains("uv pip install") {
                *self.installed.borrow_mut() = true;
            }
            if line.ends_with("bin/vllm --version") {
                *self.record_at_check.borrow_mut() = Some(state_path(&self.data_dir).exists());
            }
            on_line("ok");
            RunResult::Ok
        }
    }

    fn settings(distro: &str) -> VllmSettings {
        VllmSettings {
            wsl_distro: distro.into(),
            ..VllmSettings::default()
        }
    }

    fn plan() -> Plan {
        Plan::new(Mode::Wsl, &settings("Ubuntu"), None).unwrap()
    }

    fn venv_of(plan: &Plan) -> String {
        format!(
            "{HOME}/.local/share/imperium/runtimes/vllm/{}/venv",
            plan.version
        )
    }

    fn events(on: &RefCell<Vec<Event>>) -> impl FnMut(Event) + '_ {
        move |event| on.borrow_mut().push(event)
    }

    fn wsl(rest: &str) -> String {
        format!("wsl.exe -d Ubuntu --exec {rest}")
    }

    #[test]
    fn the_plan_takes_the_distro_from_the_setting_in_wsl_mode_only() {
        assert_eq!(plan().distro.as_deref(), Some("Ubuntu"));
        assert_eq!(
            Plan::new(Mode::Wsl, &settings(" "), None).unwrap().distro,
            None
        );
        assert_eq!(
            Plan::new(Mode::Venv, &settings("Ubuntu"), None)
                .unwrap()
                .distro,
            None
        );
        assert!(Plan::new(Mode::Wsl, &settings("-d"), None).is_err());
        let needs = plan().requirements().join("\n");
        assert!(needs.contains("WSL2") && needs.contains("Imperium does not install it"));
    }

    #[test]
    fn setup_runs_the_commands_in_order_and_records_last() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan();
        let venv = venv_of(&plan);
        let fake = WslFake::new(dir.path());
        let log = RefCell::new(Vec::new());
        let record = setup(&fake, dir.path(), &plan, &mut events(&log)).unwrap();
        assert_eq!(
            fake.calls(),
            vec![
                wsl("printenv HOME"),
                wsl("uv --version"),
                wsl("nvidia-smi -L"),
                wsl(&format!("rm -rf -- {venv}")),
                wsl(&format!("uv venv --python 3.12 {venv}")),
                wsl(&format!(
                    "uv pip install --python {venv}/bin/python vllm=={}",
                    plan.version
                )),
                wsl(&format!("{venv}/bin/vllm --version")),
            ]
        );
        assert_eq!(
            *fake.record_at_check.borrow(),
            Some(false),
            "recorded after the check"
        );
        assert_eq!(record.mode, Mode::Wsl);
        assert_eq!(managed(dir.path()), Some(record.clone()));
        assert_eq!(record.distro.as_deref(), Some("Ubuntu"));
        assert_eq!(record.venv.as_deref(), Some(venv.as_str()));
        // Nothing was made on the Windows side but the record.
        assert!(!venv_dir(dir.path(), &plan.version).exists());
        let text = std::fs::read_to_string(state_path(dir.path())).unwrap();
        assert!(text.contains("\"mode\": \"wsl\""), "{text}");
        assert!(text.contains("\"distro\": \"Ubuntu\""), "{text}");
    }

    #[test]
    fn uv_is_found_where_its_installer_puts_it() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan();
        let mut fake = WslFake::new(dir.path());
        fake.fail = vec!["--exec uv --version"];
        setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap();
        let calls = fake.calls();
        assert!(calls.contains(&wsl(&format!("{HOME}/.local/bin/uv --version"))));
        assert!(
            calls
                .iter()
                .any(|call| call.contains(&format!("--exec {HOME}/.local/bin/uv venv"))),
            "{calls:?}"
        );
    }

    #[test]
    fn a_missing_uv_says_how_to_install_it_and_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = WslFake::new(dir.path());
        fake.fail = vec!["uv --version"];
        let error = setup(&fake, dir.path(), &plan(), &mut |_| {}).unwrap_err();
        assert_eq!(error.step, "checking uv");
        assert!(error.message.contains("astral.sh/uv/install.sh"), "{error}");
        assert!(error.message.contains("does not install it"), "{error}");
        let calls = fake.calls().join("\n");
        assert!(!calls.contains("curl") && !calls.contains("uv venv") && !calls.contains("rm -rf"));
        assert_eq!(managed(dir.path()), None);
    }

    #[test]
    fn no_cuda_driver_in_wsl_stops_setup_before_anything_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = WslFake::new(dir.path());
        fake.fail = vec!["nvidia-smi"];
        let error = setup(&fake, dir.path(), &plan(), &mut |_| {}).unwrap_err();
        assert_eq!(error.step, "checking the GPU");
        assert!(
            error
                .message
                .contains("no NVIDIA GPU is visible inside Ubuntu"),
            "{error}"
        );
        let calls = fake.calls();
        assert!(
            calls.contains(&wsl("/usr/lib/wsl/lib/nvidia-smi -L")),
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("uv venv") || c.contains("rm -rf"))
        );
        assert_eq!(managed(dir.path()), None);
    }

    #[test]
    fn a_failure_removes_the_unfinished_environment_and_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan();
        let venv = venv_of(&plan);
        let mut fake = WslFake::new(dir.path());
        fake.fail = vec!["uv pip install"];
        let log = RefCell::new(Vec::new());
        let error = setup(&fake, dir.path(), &plan, &mut events(&log)).unwrap_err();
        assert!(error.step.contains("installing vLLM"), "{error}");
        assert!(
            error.tail.iter().any(|line| line.contains("boom")),
            "{error:?}"
        );
        let calls = fake.calls();
        assert_eq!(
            calls.last().unwrap(),
            &wsl(&format!("rm -rf -- {venv}")),
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.ends_with("--version") && c.contains("bin/vllm"))
        );
        assert!(
            log.borrow()
                .iter()
                .any(|e| matches!(e, Event::Warning(w) if w.contains("unfinished")))
        );
        assert_eq!(managed(dir.path()), None);
    }

    #[test]
    fn a_missing_distro_or_wsl_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake = WslFake::new(dir.path());
        fake.fail = vec!["printenv HOME"];
        let error = setup(&fake, dir.path(), &plan(), &mut |_| {}).unwrap_err();
        assert!(error.message.contains("wsl.exe -l -v"), "{error}");
        let mut absent = WslFake::new(dir.path());
        absent.no_wsl = true;
        let error = setup(&absent, dir.path(), &plan(), &mut |_| {}).unwrap_err();
        assert!(error.message.contains("wsl --install"), "{error}");
        // No distribution chosen is refused before WSL is asked.
        let none = Plan::new(Mode::Wsl, &settings(""), None).unwrap();
        let untouched = WslFake::new(dir.path());
        assert!(setup(&untouched, dir.path(), &none, &mut |_| {}).is_err());
        assert!(untouched.calls().is_empty());
    }

    #[test]
    fn setting_up_what_is_there_changes_nothing_and_another_distro_needs_remove() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan();
        let fake = WslFake::new(dir.path());
        setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap();
        let before = fake.calls().len();
        setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap();
        let again: Vec<String> = fake.calls()[before..].to_vec();
        assert_eq!(
            again,
            vec![
                wsl("printenv HOME"),
                wsl(&format!("test -x {}/bin/vllm", venv_of(&plan)))
            ]
        );
        let other = Plan::new(Mode::Wsl, &settings("Debian"), None).unwrap();
        let error = setup(&fake, dir.path(), &other, &mut |_| {}).unwrap_err();
        assert!(error.message.contains("already set up"), "{error}");
        assert!(error.message.contains("Ubuntu"), "{error}");
        // A venv setup is another mode.
        let venv = Plan::new(Mode::Venv, &VllmSettings::default(), None).unwrap();
        assert!(check_existing(dir.path(), &venv).is_err());
    }

    #[test]
    fn status_and_remove_work_inside_the_distro() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan();
        let venv = venv_of(&plan);
        let fake = WslFake::new(dir.path());
        setup(&fake, dir.path(), &plan, &mut |_| {}).unwrap();

        let status = status_checked(&fake, dir.path());
        assert!(status.present);
        assert_eq!(status.path, Some(PathBuf::from(&venv)));
        assert_eq!(status.managed.as_ref().map(|m| m.mode), Some(Mode::Wsl));
        // Its files gone from the distro: not present.
        *fake.installed.borrow_mut() = false;
        assert!(!status_checked(&fake, dir.path()).present);

        let before = fake.calls().len();
        let log = RefCell::new(Vec::new());
        let removed = remove(&fake, dir.path(), &mut events(&log)).unwrap();
        assert_eq!(removed.map(|m| m.mode), Some(Mode::Wsl));
        let version_dir = format!(
            "{HOME}/.local/share/imperium/runtimes/vllm/{}",
            plan.version
        );
        assert_eq!(
            fake.calls()[before..].to_vec(),
            vec![wsl(&format!("rm -rf -- {version_dir}"))]
        );
        assert_eq!(managed(dir.path()), None);
        assert!(!state_path(dir.path()).exists());
    }

    #[test]
    fn remove_still_drops_the_record_when_the_distro_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan();
        let setup_fake = WslFake::new(dir.path());
        setup(&setup_fake, dir.path(), &plan, &mut |_| {}).unwrap();
        let mut fake = WslFake::new(dir.path());
        fake.fail = vec!["rm -rf"];
        let log = RefCell::new(Vec::new());
        remove(&fake, dir.path(), &mut events(&log)).unwrap();
        assert!(
            log.borrow()
                .iter()
                .any(|e| matches!(e, Event::Warning(w) if w.contains("rm -rf")))
        );
        assert_eq!(managed(dir.path()), None);
    }

    #[test]
    fn a_record_edited_to_point_elsewhere_is_ignored_and_never_acted_on() {
        for (distro, venv) in [
            ("Ubuntu", "/etc"),
            (
                "Ubuntu",
                "/home/x/../../etc/.local/share/imperium/runtimes/vllm/0.11.0/venv",
            ),
            ("Ubuntu", "/.local/share/imperium/runtimes/vllm/0.11.0/venv"),
            (
                "-d",
                "/home/x/.local/share/imperium/runtimes/vllm/0.11.0/venv",
            ),
            (
                "Ubuntu",
                "/home/x/.local/share/imperium/runtimes/vllm/0.9.0/venv",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write_managed(
                dir.path(),
                &Managed {
                    mode: Mode::Wsl,
                    version: "0.11.0".into(),
                    image: None,
                    distro: Some(distro.into()),
                    venv: Some(venv.into()),
                },
            )
            .unwrap();
            assert_eq!(managed(dir.path()), None, "{distro} {venv}");
            let fake = WslFake::new(dir.path());
            assert!(remove(&fake, dir.path(), &mut |_| {}).is_err(), "{venv}");
            assert!(fake.calls().is_empty(), "{venv}");
            assert!(state_path(dir.path()).exists());
        }
        // A WSL record with no distro is no record either.
        let dir = tempfile::tempdir().unwrap();
        write_managed(
            dir.path(),
            &Managed {
                mode: Mode::Wsl,
                version: "0.11.0".into(),
                image: None,
                distro: None,
                venv: None,
            },
        )
        .unwrap();
        assert_eq!(managed(dir.path()), None);
    }

    fn recorded(dir: &Path) -> Managed {
        let fake = WslFake::new(dir);
        setup(&fake, dir, &plan(), &mut |_| {}).unwrap()
    }

    #[test]
    fn windows_uses_wsl_mode_when_the_record_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let record = recorded(dir.path());
        let none = || -> Option<String> { None };
        let launch = resolve_for(dir.path(), &VllmSettings::default(), true, &none);
        assert_eq!(
            launch,
            Launch::Wsl {
                wsl: PathBuf::from("wsl.exe"),
                distro: "Ubuntu".into(),
                program: format!("{}/bin/vllm", record.venv.unwrap()),
            }
        );
        // Off Windows the record is not used, and `resolve` is unchanged.
        assert_eq!(
            resolve_for(dir.path(), &VllmSettings::default(), false, &none),
            Launch::Command(PathBuf::from("vllm"))
        );
        assert_eq!(
            resolve(dir.path(), ""),
            Launch::Command(PathBuf::from("vllm"))
        );
    }

    #[test]
    fn windows_without_a_wsl_record_or_command_has_no_wsl_launch() {
        let dir = tempfile::tempdir().unwrap();
        let default = || Some("Ubuntu".to_string());
        assert_eq!(
            resolve_for(dir.path(), &VllmSettings::default(), true, &default),
            Launch::Command(PathBuf::from("vllm"))
        );
        for command in ["vllm", r"C:\tools\vllm.exe", r"\\nas\share\vllm"] {
            let settings = VllmSettings {
                command: command.into(),
                ..VllmSettings::default()
            };
            assert_eq!(
                resolve_for(dir.path(), &settings, true, &default),
                Launch::Command(PathBuf::from(command)),
                "{command}"
            );
        }
    }

    #[test]
    fn a_command_into_wsl_picks_its_distro_from_the_path_the_setting_or_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let default = || Some("Debian".to_string());
        let wsl_launch = |distro: &str, program: &str| Launch::Wsl {
            wsl: PathBuf::from("wsl.exe"),
            distro: distro.into(),
            program: program.into(),
        };
        let with = |command: &str, distro: &str| VllmSettings {
            command: command.into(),
            wsl_distro: distro.into(),
            ..VllmSettings::default()
        };
        assert_eq!(
            resolve_for(dir.path(), &with("/opt/venv/bin/vllm", ""), true, &default),
            wsl_launch("Debian", "/opt/venv/bin/vllm")
        );
        assert_eq!(
            resolve_for(
                dir.path(),
                &with("/opt/venv/bin/vllm", "Ubuntu"),
                true,
                &default
            ),
            wsl_launch("Ubuntu", "/opt/venv/bin/vllm")
        );
        assert_eq!(
            resolve_for(
                dir.path(),
                &with(r"\\wsl$\Fedora\opt\venv\bin\vllm", "Ubuntu"),
                true,
                &default
            ),
            wsl_launch("Fedora", "/opt/venv/bin/vllm")
        );
        // No distro can be named: the command stays a (useless on Windows) command.
        let none = || -> Option<String> { None };
        assert_eq!(
            resolve_for(dir.path(), &with("/opt/vllm", ""), true, &none),
            Launch::Command(PathBuf::from("/opt/vllm"))
        );
        // The default distro is asked for only when needed.
        let never = || -> Option<String> { panic!("must not ask") };
        resolve_for(dir.path(), &with("/opt/vllm", "Ubuntu"), true, &never);
        resolve_for(dir.path(), &VllmSettings::default(), true, &never);
    }

    #[test]
    fn output_with_nul_characters_from_wsl_is_cleaned() {
        let mut pending = b"T\0h\0e\0 \0e\0r\0r\0o\0r\0".to_vec();
        let mut lines = Vec::new();
        emit(&mut pending, &mut |line| lines.push(line.to_string()));
        assert_eq!(lines, vec!["The error".to_string()]);
    }
}
