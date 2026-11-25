//! Getting and keeping a llama.cpp runtime without being asked: llama.cpp's official build is
//! installed the first time a local model needs one and none usable is there (or only a CPU
//! build on a machine with a GPU, or Imperium's build for an earlier release than it now pins),
//! and a `llama-server` a package manager installed is upgraded with that package manager when
//! `runtime.llamacpp.update_external` says so. Nothing is downloaded when the app starts: the
//! launcher asks before each llama.cpp model loads ([`Hooks`]), and `runtime.status` asks when
//! the Local models panel opens.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, Weak};
use std::time::Duration;

use imperium_local::backend::missing_runtime;
use imperium_local::hardware::Hardware;
use imperium_local::process::RuntimeHooks;
use imperium_local::runtime::{
    AutoInstall, Resolved, SOURCE_CUSTOM, SOURCE_DETECTED, SOURCE_PACK, auto_install, build_number,
    installed_ref, llama_cpp_ref, pinned_build, remove_earlier_releases, resolve,
};
use imperium_local::runtime_check::home_dir;
use imperium_local::runtime_update::{
    ExternalUpdate, InstallKind, external_update, install_kind, upgrade_command,
};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{RUNTIME_UPDATE_PROGRESS, RuntimeUpdateJob, RuntimeUpdateOffer};

use crate::DaemonState;

/// How often a load waiting for an installation looks whether it has finished.
const INSTALL_POLL: Duration = Duration::from_millis(500);
/// How long a package manager's upgrade may take.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(900);
/// Lines of a package manager's output kept for the panel.
const OUTPUT_LINES: usize = 40;

/// What the launcher asks the daemon before and after a llama.cpp model loads.
#[derive(Debug)]
pub(crate) struct Hooks {
    state: Weak<DaemonState>,
    /// Whether a load with a server present has already looked for a better build this run.
    checked: AtomicBool,
}

impl Hooks {
    pub(crate) fn new(state: &Arc<DaemonState>) -> Arc<Self> {
        Arc::new(Hooks {
            state: Arc::downgrade(state),
            checked: AtomicBool::new(false),
        })
    }
}

impl RuntimeHooks for Hooks {
    fn ensure(
        &self,
        program: PathBuf,
        present: bool,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<PathBuf, String>> + Send>> {
        let state = self.state.upgrade();
        let first_check = present && !self.checked.swap(true, Ordering::SeqCst);
        Box::pin(async move {
            let Some(state) = state else {
                return Ok(program);
            };
            if present {
                // The server there serves this load; a better one is fetched for later loads.
                if first_check {
                    tokio::spawn(improve(state));
                }
                return Ok(program);
            }
            install_missing(state, program).await
        })
    }

    fn started(&self, program: &Path) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let data_dir = state.data_dir().to_path_buf();
        if installed_ref(&data_dir, program).as_deref() == Some(llama_cpp_ref()) {
            // The pinned build has loaded a model: the earlier one kept as a fallback goes.
            tokio::task::spawn_blocking(move || remove_earlier_releases(&data_dir, true));
        }
    }
}

/// Whether an installation was already tried since the daemon started.
fn install_tried(state: &DaemonState) -> bool {
    state
        .runtime_install
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
}

fn install_running(state: &DaemonState) -> bool {
    state
        .runtime_install
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|job| job.state == "running")
}

/// The [`AutoInstall`] decision for what `resolve` chose.
pub(crate) fn decide(state: &DaemonState, resolved: &Resolved, hardware: &Hardware) -> AutoInstall {
    let settings = state.settings();
    let release = installed_ref(state.data_dir(), &resolved.server);
    auto_install(
        &settings.runtime.llamacpp,
        resolved,
        release.as_deref(),
        hardware,
        install_tried(state),
    )
}

/// Starts installing the official build when [`decide`] says so; returns whether it did.
pub(crate) async fn install_if_needed(
    state: &Arc<DaemonState>,
    resolved: &Resolved,
    hardware: &Hardware,
) -> bool {
    let decision = decide(state, resolved, hardware);
    if !decision.installs() {
        return false;
    }
    tracing::info!(
        ?decision,
        server = %resolved.server.display(),
        "installing llama.cpp's build for this machine without being asked"
    );
    crate::onboarding::install_runtime(Arc::clone(state))
        .await
        .is_ok()
}

/// No `llama-server` is at `program`: uses one found since, or installs the official build and
/// waits for it, so the load that asked goes ahead with it.
async fn install_missing(state: Arc<DaemonState>, program: PathBuf) -> Result<PathBuf, String> {
    let hardware = crate::onboarding::hardware(&state).await;
    let settings = state.settings();
    let resolved = resolve(
        &settings.runtime.llamacpp,
        state.data_dir(),
        &hardware,
        home_dir().as_deref(),
    )
    .await;
    if resolved.server.is_file() {
        use_resolved(&state, &resolved);
        return Ok(resolved.server);
    }
    if !install_running(&state) && !install_if_needed(&state, &resolved, &hardware).await {
        let failed = state
            .runtime_install
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|job| job.state == "failed")
            .and_then(|job| job.message.clone());
        return Err(match failed {
            Some(why) => format!(
                "{} Installing llama.cpp's build failed: {why}",
                missing_runtime(&program)
            ),
            None => missing_runtime(&program),
        });
    }
    while install_running(&state) {
        tokio::time::sleep(INSTALL_POLL).await;
    }
    let server = state
        .local_runtime()
        .map_or_else(|| program.clone(), |local| local.server().1);
    if server.is_file() {
        return Ok(server);
    }
    let why = state
        .runtime_install
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|job| job.message.clone())
        .unwrap_or_else(|| "no reason given".into());
    Err(format!(
        "{} Installing llama.cpp's build failed: {why}",
        missing_runtime(&program)
    ))
}

/// Points the local runtime at `resolved` when it uses something else.
fn use_resolved(state: &DaemonState, resolved: &Resolved) {
    if let Some(local) = state.local_runtime()
        && local.server() != (resolved.label(), resolved.server.clone())
    {
        local.set_server(resolved.label(), resolved.server.clone());
    }
}

/// A server is there; in the background, installs a better build when it runs on the CPU only
/// or is Imperium's build of an earlier release, and runs a package manager's upgrade when
/// `update_external = "auto"`.
async fn improve(state: Arc<DaemonState>) {
    let hardware = crate::onboarding::hardware(&state).await;
    let settings = state.settings();
    let resolved = resolve(
        &settings.runtime.llamacpp,
        state.data_dir(),
        &hardware,
        home_dir().as_deref(),
    )
    .await;
    if install_if_needed(&state, &resolved, &hardware).await {
        return;
    }
    let version = resolved
        .check
        .as_ref()
        .and_then(|check| check.version.clone());
    let offer = offer(&state, &resolved, version.as_deref(), true);
    if let ExternalUpdate::Auto(_) = offer.1
        && let Err(error) = update_external(Arc::clone(&state)).await
    {
        tracing::info!(error = %error.message, "llama.cpp not updated by its package manager");
    }
}

/// How the server `resolved` names was installed.
pub(crate) fn kind(state: &DaemonState, resolved: &Resolved) -> InstallKind {
    if resolved.source == SOURCE_PACK || installed_ref(state.data_dir(), &resolved.server).is_some()
    {
        return InstallKind::Managed;
    }
    // A link (Homebrew's in /usr/local/bin, winget's in Links) reads as where it points.
    let real = std::fs::canonicalize(&resolved.server).unwrap_or_else(|_| resolved.server.clone());
    let managed = state.data_dir().join("runtimes");
    let by_link = install_kind(&resolved.server, &managed);
    match install_kind(&real, &managed) {
        InstallKind::Other => by_link,
        kind => kind,
    }
}

/// The install kind of the server in use, its update decision, and the offer shown for it.
/// `flags_ok`: its `--help` lists every flag Imperium passes (or that is not known).
pub(crate) fn offer(
    state: &DaemonState,
    resolved: &Resolved,
    version: Option<&str>,
    flags_ok: bool,
) -> (InstallKind, ExternalUpdate, Option<RuntimeUpdateOffer>) {
    let kind = kind(state, resolved);
    let settings = state.settings();
    let pinned = pinned_build();
    let build = build_number(version);
    if kind == InstallKind::Managed {
        let release = installed_ref(state.data_dir(), &resolved.server);
        let outdated = resolved.server.is_file()
            && release
                .as_deref()
                .is_some_and(|release| release != llama_cpp_ref());
        let offer = outdated.then(|| RuntimeUpdateOffer {
            action: "managed".into(),
            command: None,
            message: format!(
                "Imperium now uses llama.cpp {}; it installs it the next time a local model is \
                 needed (or choose Install), and keeps this build until the new one has loaded \
                 a model.",
                llama_cpp_ref()
            ),
        });
        return (kind, ExternalUpdate::None, offer);
    }
    if resolved.source != SOURCE_DETECTED && resolved.source != SOURCE_CUSTOM {
        return (kind, ExternalUpdate::None, None);
    }
    let decision = external_update(
        &kind,
        build,
        pinned,
        flags_ok,
        &settings.runtime.llamacpp.update_external,
        cfg!(windows),
    );
    let older = match (build, pinned) {
        (Some(build), Some(pinned)) if build < pinned => {
            format!("llama.cpp build {build} is older than {}", llama_cpp_ref())
        }
        _ => "This llama-server lacks flags Imperium passes".to_string(),
    };
    let offer = match &decision {
        ExternalUpdate::None => None,
        ExternalUpdate::Offer(command) | ExternalUpdate::Auto(command) => {
            Some(RuntimeUpdateOffer {
                action: "package_manager".into(),
                command: Some(command.join(" ")),
                message: format!("{older}. Update available through {}.", kind.name()),
            })
        }
        ExternalUpdate::UseManaged => Some(RuntimeUpdateOffer {
            action: "use_managed".into(),
            command: None,
            message: format!(
                "{older}. Imperium does not modify this folder; install Imperium's build to use \
                 it instead."
            ),
        }),
    };
    (kind, decision, offer)
}

/// Whether a model loaded with `server` is still running.
fn loaded_from(state: &DaemonState, server: &Path) -> bool {
    let Some(local) = state.local_runtime() else {
        return false;
    };
    local.server().1 == server && state.local_models().models.iter().any(|row| row.loaded)
}

fn set_job(state: &DaemonState, job: RuntimeUpdateJob) {
    *state
        .runtime_update
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(job.clone());
    state.announce(RUNTIME_UPDATE_PROGRESS, job);
}

/// `runtime.update_external`: runs the package manager's upgrade of the `llama-server` in use,
/// in the background, unless one is running, a model is loaded from that server, or no package
/// manager owns it.
pub(crate) async fn update_external(
    state: Arc<DaemonState>,
) -> Result<RuntimeUpdateJob, ErrorObject> {
    let invalid = |message: String| ErrorObject::new(codes::INVALID_PARAMS, message);
    if let Some(running) = state
        .runtime_update
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .filter(|job| job.state == "running")
    {
        return Ok(running.clone());
    }
    let hardware = crate::onboarding::hardware(&state).await;
    let settings = state.settings();
    let resolved = resolve(
        &settings.runtime.llamacpp,
        state.data_dir(),
        &hardware,
        home_dir().as_deref(),
    )
    .await;
    let server = resolved.server.clone();
    let kind = kind(&state, &resolved);
    let Some(command) = upgrade_command(&kind, cfg!(windows)) else {
        return Err(invalid(format!(
            "{} was not installed by a package manager Imperium can run (winget, scoop, \
             Chocolatey, Homebrew)",
            server.display()
        )));
    };
    if loaded_from(&state, &server) {
        return Err(invalid(
            "a model is loaded from this llama-server; update it once the local models are \
             unloaded"
                .into(),
        ));
    }
    let job = RuntimeUpdateJob {
        server_path: server.display().to_string(),
        command: command.join(" "),
        state: "running".into(),
        output: String::new(),
    };
    set_job(&state, job.clone());
    tracing::info!(command = %job.command, "updating llama.cpp with its package manager");
    let updating = Arc::clone(&state);
    let started = job.clone();
    tokio::spawn(async move {
        let state = updating;
        let (ok, output) = run(&command).await;
        if ok {
            crate::onboarding::apply_server(&state);
        }
        tracing::info!(ok, command = %started.command, "llama.cpp package-manager update ended");
        set_job(
            &state,
            RuntimeUpdateJob {
                state: if ok { "done" } else { "failed" }.into(),
                output,
                ..started
            },
        );
    });
    Ok(job)
}

/// Runs `command`; whether it succeeded and the last lines it printed.
async fn run(command: &[String]) -> (bool, String) {
    let Some((program, args)) = command.split_first() else {
        return (false, "no command".into());
    };
    let mut process = tokio::process::Command::new(program);
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    let child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return (false, format!("could not run {program}: {error}")),
    };
    match tokio::time::timeout(UPDATE_TIMEOUT, child.wait_with_output()).await {
        Err(_) => (
            false,
            format!(
                "{program} did not finish within {} s",
                UPDATE_TIMEOUT.as_secs()
            ),
        ),
        Ok(Err(error)) => (false, format!("{program} failed: {error}")),
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            (output.status.success(), tail(&text, OUTPUT_LINES))
        }
    }
}

/// The last `lines` non-empty lines of `text`.
fn tail(text: &str, lines: usize) -> String {
    let kept: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    kept[kept.len().saturating_sub(lines)..].join("\n")
}

/// The backend models run on, in words, for `runtime.status`.
pub(crate) fn backend_label(resolved: &Resolved, devices: &[(String, bool)]) -> String {
    if resolved.source == SOURCE_PACK {
        return pack_backend(&resolved.pack);
    }
    let gpu = devices
        .iter()
        .find(|(_, gpu)| *gpu)
        .map(|(id, _)| id.to_ascii_uppercase());
    match gpu {
        Some(id) if id.starts_with("CUDA") => "CUDA".into(),
        Some(id) if id.starts_with("VULKAN") => "Vulkan".into(),
        Some(id) if id.starts_with("ROCM") => "ROCm".into(),
        Some(id) if id.starts_with("MTL") || id.starts_with("METAL") => "Metal".into(),
        Some(id) => id
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .to_string(),
        None => "CPU".into(),
    }
}

/// `windows-x64-cuda13` as `CUDA 13`.
fn pack_backend(pack: &str) -> String {
    match pack.rsplit('-').next().unwrap_or_default() {
        "cuda13" => "CUDA 13".into(),
        "cuda12" => "CUDA 12".into(),
        "vulkan" => "Vulkan".into(),
        "rocm" => "ROCm".into(),
        "metal" => "Metal".into(),
        _ => "CPU".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(source: &'static str, pack: &str) -> Resolved {
        Resolved {
            source,
            pack: pack.into(),
            server: PathBuf::from("llama-server"),
            check: None,
        }
    }

    #[test]
    fn backends_are_named_from_the_build_or_the_devices() {
        let cuda = [("CUDA0".to_string(), true)];
        assert_eq!(
            backend_label(&resolved(SOURCE_PACK, "windows-x64-cuda13"), &[]),
            "CUDA 13"
        );
        assert_eq!(
            backend_label(&resolved(SOURCE_PACK, "linux-x64-cpu"), &[]),
            "CPU"
        );
        assert_eq!(
            backend_label(&resolved(SOURCE_DETECTED, "x"), &cuda),
            "CUDA"
        );
        assert_eq!(
            backend_label(&resolved(SOURCE_DETECTED, "x"), &[("Vulkan0".into(), true)]),
            "Vulkan"
        );
        assert_eq!(
            backend_label(&resolved(SOURCE_CUSTOM, "x"), &[("CPU".into(), false)]),
            "CPU"
        );
    }

    #[test]
    fn only_the_last_lines_of_output_are_kept() {
        let text = (0..50).map(|n| format!("line {n}\n")).collect::<String>();
        let kept = tail(&text, 3);
        assert_eq!(kept, "line 47\nline 48\nline 49");
        assert_eq!(tail("\n\n", 3), "");
    }
}
