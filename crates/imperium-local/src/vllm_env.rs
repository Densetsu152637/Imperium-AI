//! The managed vLLM environment: a `uv` virtual environment with a pinned vLLM, or the official
//! Docker image, set up only when the user asks (`imperium runtime vllm setup`), never by itself.
//!
//! Layout, under `<data dir>/runtimes/vllm`:
//!
//! - `managed.json`: what was set up (mode, version, image), written last, so an interrupted
//!   setup leaves no record of a half-made environment.
//! - `<version>/venv`: the virtual environment (venv mode).
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
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Venv => "venv",
            Mode::Docker => "docker",
        }
    }
}

/// What the user asked to set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub mode: Mode,
    pub version: String,
    pub image: String,
    pub python: String,
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
        };
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
    })
}

fn write_managed(data_dir: &Path, managed: &Managed) -> Result<(), String> {
    let path = state_path(data_dir);
    std::fs::create_dir_all(root(data_dir)).map_err(|error| error.to_string())?;
    let text = serde_json::to_string_pretty(managed).map_err(|error| error.to_string())?;
    // Written whole and renamed, so a reader never sees half of it.
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text).map_err(|error| error.to_string())?;
    std::fs::rename(&temp, &path).map_err(|error| error.to_string())
}

/// What the daemon should start for `vllm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// This executable.
    Command(PathBuf),
    /// A container of this image, started by this `docker`.
    Docker { docker: PathBuf, image: String },
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
    let text = String::from_utf8_lossy(pending).into_owned();
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

/// Runs one step, reporting it and keeping the output's last lines for a failure.
fn step(
    runner: &dyn Runner,
    on: &mut dyn FnMut(Event),
    title: &str,
    program: &str,
    args: &[String],
    missing: &str,
) -> Result<(), SetupError> {
    on(Event::Step(format!(
        "{title}: {} {}",
        program,
        args.join(" ")
    )));
    let mut tail: Vec<String> = Vec::new();
    let result = runner.run(program, args, &mut |line| {
        if tail.len() == TAIL_LINES {
            tail.remove(0);
        }
        tail.push(line.to_string());
        on(Event::Output(line.to_string()));
    });
    match result {
        RunResult::Ok => Ok(()),
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
                .map(|image| format!(", {image}"))
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
    } else {
        Status {
            present: true,
            size_bytes: None,
            path: None,
            managed: Some(record),
        }
    }
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
    match record.mode {
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
    serve_args: &[String],
) -> Result<Vec<String>, String> {
    valid_image(image)?;
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

/// Removes the containers a daemon that was killed left behind: those named like ours whose
/// owner process (`is_alive`) is gone and is not `own_pid`. Returns the names removed. Failures
/// (no Docker, no permission) remove nothing.
pub fn sweep_stale_containers(
    docker: &Path,
    own_pid: u32,
    is_alive: &dyn Fn(u32) -> bool,
) -> Vec<String> {
    let listed = Command::new(docker)
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name=^{CONTAINER_PREFIX}"),
            "--format",
            "{{.Names}}",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(listed) = listed else {
        return Vec::new();
    };
    if !listed.status.success() {
        return Vec::new();
    }
    let mut removed = Vec::new();
    for name in String::from_utf8_lossy(&listed.stdout).lines().take(256) {
        let name = name.trim();
        let Some(owner) = container_owner(name) else {
            continue;
        };
        if owner == own_pid || is_alive(owner) {
            continue;
        }
        let status = Command::new(docker)
            .args(["rm", "-f", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if status.is_ok_and(|status| status.success()) {
            removed.push(name.to_string());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

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
            &serve,
        )
        .unwrap();
        let text = args.join(" ");
        assert!(
            text.starts_with(
                "run --rm --pull=never --name imperium-vllm-77-3-8123 --gpus all --ipc=host --security-opt no-new-privileges -e VLLM_NO_USAGE_STATS=1 -e DO_NOT_TRACK=1 -p 127.0.0.1:8123:8123 "
            ),
            "{text}"
        );
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
        assert!(docker_run_args("--privileged", "n", 1, &serve).is_err());
        assert!(docker_run_args("img", "n", 1, &[]).is_err());
        let comma: Vec<String> = ["serve", "/a,b"].map(String::from).to_vec();
        assert!(docker_run_args("img", "n", 1, &comma).is_err());
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
        assert!(
            matches!(resolve(dir.path(), ""), Launch::Command(p) if p != PathBuf::from("vllm"))
        );
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

    /// A `docker` that lists the given names for `ps` and appends the arguments of any other call
    /// to `log`.
    #[cfg(unix)]
    pub(crate) fn fake_docker(dir: &Path, names: &[&str], log: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let listing: String = names.iter().map(|name| format!("echo {name}\n")).collect();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = ps ]; then\n{listing}exit 0\nfi\necho \"$@\" >> {}\n",
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
        );
        // 555 is this process, 4242 is another daemon that is still running.
        let removed = sweep_stale_containers(&docker, 555, &|pid| pid == 4242);
        assert_eq!(removed, vec!["imperium-vllm-999999999-0-8000".to_string()]);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "rm -f imperium-vllm-999999999-0-8000"
        );
        // No Docker, nothing swept.
        assert!(sweep_stale_containers(&dir.path().join("none"), 1, &|_| false).is_empty());
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
