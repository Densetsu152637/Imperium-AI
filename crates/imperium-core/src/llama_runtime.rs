//! Getting and keeping a llama.cpp runtime, always with the user's yes. Nothing is downloaded
//! on its own: when local models are on and no usable runtime is there (or only a CPU build on a
//! machine with a GPU, or Imperium's build for an earlier release than it now pins), `decide`
//! says an install should be offered, and `runtime.status` lists the offer with its size and
//! source ([`crate::onboarding`]); the user approves it with `runtime.install` (the Local models
//! panel, or `imperium runtime install`). `runtime.update` installs llama.cpp's latest release
//! beside the build in use and switches to it once it works ([`update`]), or, for a
//! `llama-server` the user keeps outside Imperium's folders, in a sibling folder, pointing
//! `runtime.llamacpp.custom_path` at it ([`Route::Beside`]); `runtime.check_update` looks for
//! one at most once a day ([`check_update`]). A `llama-server` a package manager installed is
//! upgraded with that package manager when `runtime.llamacpp.update_external` says so. Nothing
//! happens while local models are off, and nothing at all when the app starts: the launcher
//! only asks the daemon before each llama.cpp model loads ([`Hooks`]).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, Weak};
use std::time::Duration;

use imperium_local::backend::missing_runtime;
use imperium_local::hardware::Hardware;
use imperium_local::process::RuntimeHooks;
use imperium_local::runtime::{
    AutoInstall, GITHUB_API, Place, Resolved, SOURCE_CUSTOM, SOURCE_DETECTED, SOURCE_PACK,
    activate, auto_install, build_number, fetch_latest, install_first_working,
    install_first_working_at, installed_ref, llama_cpp_ref, pinned_build, ref_build, releases_root,
    remove_earlier_releases, resolve, smoke_test,
};
use imperium_local::runtime_check::{self, home_dir};
use imperium_local::runtime_release::{
    Checked, beside_container, due, newer, now_s, plan, read_cache, update_packs, write_cache,
};
use imperium_local::runtime_update::{
    ExternalUpdate, InstallKind, external_update, install_kind, upgrade_command,
};
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    RUNTIME_UPDATE_PROGRESS, RuntimeInstallJob, RuntimeUpdateCheck, RuntimeUpdateJob,
    RuntimeUpdateOffer,
};

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
                // The server there serves this load; a package manager's upgrade may follow
                // for later loads when the user chose `update_external = "auto"`.
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
    // Nothing local installs while local models are off, not even to fix the runtime.
    if !settings.models.local.enabled {
        return AutoInstall::Keep;
    }
    let release = installed_ref(state.data_dir(), &resolved.server);
    auto_install(
        &settings.runtime.llamacpp,
        resolved,
        release.as_deref(),
        hardware,
        install_tried(state),
    )
}

/// No `llama-server` is at `program`: uses one found since, or waits for the installation the
/// user approved (if one is running), so the load that asked goes ahead with it. Without one
/// the load fails and says how to approve the download: nothing is downloaded unasked.
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
    if !install_running(&state) {
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
            None => format!(
                "{} Imperium does not download llama.cpp without your approval: approve it in \
                 Settings → Local models, or run `imperium runtime install`.",
                missing_runtime(&program)
            ),
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

/// A server is there; in the background, runs a package manager's upgrade when
/// `update_external = "auto"` (the user's standing choice). Nothing is downloaded otherwise.
async fn improve(state: Arc<DaemonState>) {
    if !state.settings().models.local.enabled {
        return;
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
                "Imperium's build of llama.cpp is older than {}. Choose Update to download the \
                 latest release (it asks first and keeps this build for rollback).",
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
    crate::onboarding::require_local_on(&state, "updating the llama.cpp runtime")?;
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

/// Where `runtime.update` puts the new build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// Under the data folder beside Imperium's other builds; the one in use stays for rollback.
    Managed,
    /// A new folder in this folder, next to the llama.cpp folder `custom_path` names.
    Beside(PathBuf),
    /// Updated another way; the text says how.
    Unsupported(String),
}

/// Where an update of the server `resolved` names goes. Imperium's own build and a server found
/// on this machine (which Imperium's build is then preferred over) update under the data
/// folder. A `custom_path` outside Imperium's folders is never modified: its update goes beside
/// it, or, for a package manager's or LM Studio's install, is left to them.
pub(crate) fn route(state: &DaemonState, resolved: &Resolved) -> Route {
    if resolved.source != SOURCE_CUSTOM {
        return Route::Managed;
    }
    let kind = kind(state, resolved);
    if let Some(command) = upgrade_command(&kind, cfg!(windows)) {
        return Route::Unsupported(format!(
            "{} was installed with {}; update it with `{}` (the Update button in Local models \
             runs it)",
            resolved.server.display(),
            kind.name(),
            command.join(" ")
        ));
    }
    match kind {
        InstallKind::Managed => Route::Unsupported(
            "runtime.llamacpp.custom_path names one of Imperium's own builds; run `imperium \
             runtime use` (with no path) to go back to the installed runtime, which `imperium \
             runtime update` keeps current"
                .into(),
        ),
        InstallKind::LmStudio => {
            Route::Unsupported("LM Studio's own llama.cpp backends are updated by LM Studio".into())
        }
        _ => match beside_container(&resolved.server) {
            Ok(container) => Route::Beside(container),
            Err(why) => Route::Unsupported(format!(
                "{why}; run `imperium runtime use` (with no path) to use Imperium's own build, \
                 which `imperium runtime update` keeps current"
            )),
        },
    }
}

/// When a check for a newer release last failed (Unix seconds; 0 for never), so a machine
/// without a connection is not asked again for an hour.
static CHECK_FAILED_AT: AtomicU64 = AtomicU64::new(0);
const RETRY_AFTER_FAILURE_S: u64 = 60 * 60;

fn check_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// The llama.cpp build number of the server in use: its release folder when Imperium installed
/// it, else its `--version`.
pub(crate) async fn current_build(
    state: &DaemonState,
    resolved: &Resolved,
    hardware: &Hardware,
) -> Option<u64> {
    if let Some(release) = installed_ref(state.data_dir(), &resolved.server) {
        return ref_build(&release);
    }
    if !resolved.server.is_file() {
        return None;
    }
    let check = match resolved.check.clone() {
        Some(check) => check,
        None => runtime_check::check(&resolved.server, hardware).await,
    };
    build_number(check.version.as_deref())
}

/// `runtime.check_update`: reads llama.cpp's latest release from GitHub when a check is due
/// (once a day, and only with `runtime.llamacpp.check_updates` on) or `force`d by the user, and
/// says what updating the server in use would download. Nothing is downloaded.
pub(crate) async fn check_update(
    state: Arc<DaemonState>,
    force: bool,
) -> Result<RuntimeUpdateCheck, ErrorObject> {
    crate::onboarding::require_local_on(&state, "checking for a llama.cpp update")?;
    let settings = state.settings();
    let enabled = settings.runtime.llamacpp.check_updates;
    let data_dir = state.data_dir().to_path_buf();
    let mut cached = read_cache(&data_dir);
    let now = now_s();
    let failed_at = CHECK_FAILED_AT.load(Ordering::Relaxed);
    let recently_failed = failed_at != 0 && now.saturating_sub(failed_at) < RETRY_AFTER_FAILURE_S;
    if force || (enabled && due(cached.as_ref(), now) && !recently_failed) {
        match fetch_latest(&check_client(), GITHUB_API).await {
            Ok(release) => {
                let checked = Checked {
                    checked_at: now,
                    release,
                };
                if let Err(error) = write_cache(&data_dir, &checked) {
                    tracing::debug!(%error, "the llama.cpp update check was not kept");
                }
                cached = Some(checked);
            }
            Err(error) => {
                CHECK_FAILED_AT.store(now, Ordering::Relaxed);
                if force {
                    return Err(ErrorObject::new(
                        codes::INTERNAL_ERROR,
                        format!("could not read llama.cpp's latest release: {error}"),
                    ));
                }
                tracing::debug!(%error, "no check for a llama.cpp update was possible");
            }
        }
    }
    let hardware = crate::onboarding::hardware(&state).await;
    let resolved = resolve(
        &settings.runtime.llamacpp,
        state.data_dir(),
        &hardware,
        home_dir().as_deref(),
    )
    .await;
    let current = current_build(&state, &resolved, &hardware).await;
    Ok(describe(
        &state,
        &resolved,
        current,
        &hardware,
        cached.as_ref(),
        enabled,
    ))
}

/// What the last check found, for the server `resolved` names at build `current`: the newest
/// release, whether it is newer, where an update would go and what it would download.
pub(crate) fn describe(
    state: &DaemonState,
    resolved: &Resolved,
    current: Option<u64>,
    hardware: &Hardware,
    checked: Option<&Checked>,
    enabled: bool,
) -> RuntimeUpdateCheck {
    let settings = state.settings();
    let route = route(state, resolved);
    let latest = checked.map(|checked| checked.release.tag_name.clone());
    let packs = update_packs(&settings.runtime.llamacpp.backend, hardware);
    let planned = checked.map(|checked| plan(&checked.release, &packs, GITHUB_API));
    let usable = planned.as_ref().and_then(|planned| planned.as_ref().ok());
    let newer_build = latest.as_deref().is_some_and(|tag| newer(tag, current));
    let supported = !matches!(route, Route::Unsupported(_));
    let target_dir = latest.as_deref().and_then(|tag| match &route {
        Route::Managed => Some(releases_root(state.data_dir()).join(tag)),
        Route::Beside(container) => Some(container.join(tag)),
        Route::Unsupported(_) => None,
    });
    let message = match (&route, &planned) {
        (Route::Unsupported(why), _) => Some(why.clone()),
        (_, None) if enabled => Some("No check for a newer llama.cpp has been made yet.".into()),
        (_, None) => Some(
            "Checking for updates is off (runtime.llamacpp.check_updates); `imperium runtime \
             update` still looks when you run it."
                .into(),
        ),
        (_, Some(Err(error))) => Some(format!(
            "llama.cpp {} cannot be installed here: {error}",
            latest.as_deref().unwrap_or("latest")
        )),
        (_, Some(Ok(_))) if !newer_build => Some(match current {
            Some(build) => format!("llama.cpp b{build} is up to date."),
            None => "No llama.cpp build is installed to update; install one first.".into(),
        }),
        _ => None,
    };
    RuntimeUpdateCheck {
        enabled,
        checked_at: checked.map(|checked| checked.checked_at),
        latest,
        current: current.map(|build| format!("b{build}")),
        available: newer_build && supported && usable.is_some(),
        target: match route {
            Route::Managed => "managed",
            Route::Beside(_) => "beside",
            Route::Unsupported(_) => "unsupported",
        }
        .into(),
        target_dir: target_dir.map(|dir| dir.display().to_string()),
        pack: usable.map(|usable| usable.pack.clone()),
        bytes: usable.map(|usable| usable.bytes),
        source: imperium_local::runtime_release::SOURCE.into(),
        message,
    }
}

/// `runtime.update`: installs llama.cpp's latest release beside the build in use, smoke-tests
/// it, and switches to it (for a `custom_path` outside Imperium's folders, in a sibling folder
/// that `custom_path` is then pointed at). It runs in the background, reporting
/// `runtime.install_progress`. The call is the user's yes; the old build or folder is left as
/// it was.
pub(crate) async fn update(state: Arc<DaemonState>) -> Result<RuntimeInstallJob, ErrorObject> {
    crate::onboarding::require_local_on(&state, "updating the llama.cpp runtime")?;
    let hardware = crate::onboarding::hardware(&state).await;
    let settings = state.settings();
    let resolved = resolve(
        &settings.runtime.llamacpp,
        state.data_dir(),
        &hardware,
        home_dir().as_deref(),
    )
    .await;
    let route = route(&state, &resolved);
    if let Route::Unsupported(why) = &route {
        return Err(ErrorObject::new(codes::INVALID_PARAMS, why.clone()));
    }
    let packs = update_packs(&settings.runtime.llamacpp.backend, &hardware);
    let job = match crate::onboarding::begin_job(&state, &packs[0]) {
        Ok(job) => job,
        Err(running) => return Ok(running),
    };
    let updating = Arc::clone(&state);
    tokio::spawn(async move {
        let state = updating;
        match run_update(&state, route, &hardware, &packs, &resolved).await {
            Ok(message) => crate::onboarding::finish_job(&state, true, message),
            Err(why) => {
                tracing::warn!(%why, "llama.cpp was not updated");
                crate::onboarding::finish_job(&state, false, why);
            }
        }
    });
    Ok(job)
}

/// The work of [`update`]; the text of its result is the job's message.
async fn run_update(
    state: &Arc<DaemonState>,
    route: Route,
    hardware: &Hardware,
    packs: &[String],
    resolved: &Resolved,
) -> Result<String, String> {
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let release = fetch_latest(&http, GITHUB_API)
        .await
        .map_err(|error| error.to_string())?;
    let data_dir = state.data_dir().to_path_buf();
    let _ = write_cache(
        &data_dir,
        &Checked {
            checked_at: now_s(),
            release: release.clone(),
        },
    );
    let current = current_build(state, resolved, hardware).await;
    if let Some(build) = current
        && !newer(&release.tag_name, current)
    {
        return Ok(format!(
            "llama.cpp b{build} is already up to date (the latest release is {})",
            release.tag_name
        ));
    }
    let planned = plan(&release, packs, GITHUB_API).map_err(|error| error.to_string())?;
    tracing::info!(
        release = %planned.release,
        pack = %planned.pack,
        bytes = planned.bytes,
        "updating llama.cpp"
    );
    let progress = crate::onboarding::job_progress(Arc::clone(state));
    // An update never trades a working GPU build for one that does not see the GPU.
    let verify = smoke_test(hardware, current.is_some());
    let installed = match route {
        Route::Managed => {
            let installed =
                install_first_working(&http, &release, &data_dir, packs, &verify, &progress)
                    .await
                    .map_err(|error| error.to_string())?;
            activate(&data_dir, &installed.release).map_err(|error| {
                format!(
                    "{} is installed but could not be switched to: {error}",
                    installed.release
                )
            })?;
            crate::onboarding::apply_server(state);
            installed
        }
        Route::Beside(container) => {
            let installed = install_first_working_at(
                &http,
                &release,
                &Place::Beside(container),
                packs,
                &verify,
                &progress,
            )
            .await
            .map_err(|error| error.to_string())?;
            crate::onboarding::use_custom_server(state, &installed.server)
                .map_err(|error| error.message)?;
            installed
        }
        Route::Unsupported(why) => return Err(why),
    };
    Ok(installed.server.display().to_string())
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
    fn nothing_installs_while_local_models_are_off() {
        let home = tempfile::tempdir().unwrap();
        let hardware = Hardware {
            gpus: Vec::new(),
            ram_total_bytes: 16 << 30,
            ram_available_bytes: 8 << 30,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
            vram_overridden: false,
        };
        let mut settings =
            imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
                .settings()
                .unwrap();
        // No runtime is installed, so with local models on the build would be installed.
        let missing = Resolved {
            server: home.path().join("nothing-here"),
            ..resolved(SOURCE_PACK, "linux-x64-cpu")
        };
        assert!(!settings.models.local.enabled, "off by default");
        let off = DaemonState::new(
            imperium_config::Paths::under(home.path()),
            settings.clone(),
            None,
        );
        assert_eq!(decide(&off, &missing, &hardware), AutoInstall::Keep);
        settings.models.local.enabled = true;
        let on = DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
        assert_eq!(decide(&on, &missing, &hardware), AutoInstall::Missing);
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

    fn quiet_state(home: &Path) -> Arc<DaemonState> {
        let mut settings =
            imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
                .settings()
                .unwrap();
        settings.models.local.enabled = true;
        settings.runtime.llamacpp.backend = "cpu".into();
        DaemonState::new(imperium_config::Paths::under(home), settings, None)
    }

    fn cpu_machine() -> Hardware {
        Hardware {
            gpus: Vec::new(),
            ram_total_bytes: 16 << 30,
            ram_available_bytes: 8 << 30,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
            vram_overridden: false,
        }
    }

    fn custom(server: &Path) -> Resolved {
        Resolved {
            source: SOURCE_CUSTOM,
            pack: "x".into(),
            server: server.to_path_buf(),
            check: None,
        }
    }

    #[test]
    fn a_folder_of_the_users_is_updated_beside_it_and_other_installs_are_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let state = quiet_state(home.path());
        let root = tempfile::tempdir().unwrap();
        let exe = format!("llama-server{}", std::env::consts::EXE_SUFFIX);
        let bin = root.path().join("llama.cpp").join("b12823").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(&exe), "mine").unwrap();
        assert_eq!(
            route(&state, &custom(&bin.join(&exe))),
            Route::Beside(root.path().join("llama.cpp"))
        );
        // A package manager's install is its own to upgrade.
        let scoop = root.path().join("scoop/apps/llama.cpp/current");
        std::fs::create_dir_all(&scoop).unwrap();
        std::fs::write(scoop.join(&exe), "scoop").unwrap();
        match route(&state, &custom(&scoop.join(&exe))) {
            Route::Unsupported(why) => assert!(why.contains("update llama.cpp"), "{why}"),
            other => panic!("{other:?}"),
        }
        // A llama-server that is not in a llama.cpp folder has nowhere to be updated into.
        let tools = root.path().join("mytools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(tools.join(&exe), "x").unwrap();
        assert!(matches!(
            route(&state, &custom(&tools.join(&exe))),
            Route::Unsupported(_)
        ));
        // Imperium's own build, and one found on this machine, update under the data folder.
        assert_eq!(
            route(&state, &resolved(SOURCE_PACK, "linux-x64-cpu")),
            Route::Managed
        );
        assert_eq!(
            route(&state, &resolved(SOURCE_DETECTED, "linux-x64-cpu")),
            Route::Managed
        );
        // A custom path naming one of Imperium's own builds is not an outside folder.
        let own = state
            .data_dir()
            .join("runtimes/llama.cpp/b12180/imperium-runtime-llama.cpp-linux-x64-cpu/bin");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join(&exe), "ours").unwrap();
        assert!(matches!(
            route(&state, &custom(&own.join(&exe))),
            Route::Unsupported(_)
        ));
    }

    /// The release asset of llama.cpp build `tag` for this machine's CPU build.
    fn host_asset(tag: &str) -> String {
        let platform = imperium_local::hardware::platform();
        let (os, arch) = platform.split_once('-').unwrap();
        match os {
            "windows" => format!("llama-{tag}-bin-win-cpu-{arch}.zip"),
            "macos" => format!("llama-{tag}-bin-macos-{arch}.tar.gz"),
            _ => format!("llama-{tag}-bin-ubuntu-{arch}.tar.gz"),
        }
    }

    fn checked(tag: &str, digest: Option<&str>) -> Checked {
        Checked {
            checked_at: 1_700_000_000,
            release: serde_json::from_value(serde_json::json!({
                "tag_name": tag,
                "assets": [{
                    "name": host_asset(tag),
                    "url": format!("{GITHUB_API}/repos/ggml-org/llama.cpp/releases/assets/7"),
                    "size": 42_000_000,
                    "digest": digest,
                }],
            }))
            .unwrap(),
        }
    }

    #[test]
    fn the_last_check_says_what_an_update_would_download_and_where_it_goes() {
        let home = tempfile::tempdir().unwrap();
        let state = quiet_state(home.path());
        let hardware = cpu_machine();
        let good = format!("sha256:{}", "a".repeat(64));
        // Imperium's own build, older than the latest release.
        let ours = resolved(SOURCE_PACK, "linux-x64-cpu");
        let found = describe(
            &state,
            &ours,
            Some(12180),
            &hardware,
            Some(&checked("b12900", Some(&good))),
            true,
        );
        assert!(found.available, "{found:?}");
        assert_eq!(found.latest.as_deref(), Some("b12900"));
        assert_eq!(found.current.as_deref(), Some("b12180"));
        assert_eq!(found.target, "managed");
        assert_eq!(found.bytes, Some(42_000_000));
        assert_eq!(found.source, "github.com/ggml-org/llama.cpp");
        assert!(found.target_dir.unwrap().ends_with("b12900"));
        // Up to date: nothing is offered, and it says so.
        let same = describe(
            &state,
            &ours,
            Some(12900),
            &hardware,
            Some(&checked("b12900", Some(&good))),
            true,
        );
        assert!(!same.available);
        assert!(same.message.unwrap().contains("up to date"));
        // A release whose asset has no digest cannot be verified, so it is not offered.
        let unsigned = describe(
            &state,
            &ours,
            Some(12180),
            &hardware,
            Some(&checked("b12900", None)),
            true,
        );
        assert!(!unsigned.available);
        assert!(unsigned.message.unwrap().contains("SHA-256"));
        // Before the first check, or with checking off, nothing is claimed.
        let none = describe(&state, &ours, Some(12180), &hardware, None, false);
        assert!(!none.available);
        assert!(none.message.unwrap().contains("check_updates"));
        // An update for the user's own folder says where its new folder would be.
        let root = tempfile::tempdir().unwrap();
        let exe = format!("llama-server{}", std::env::consts::EXE_SUFFIX);
        let bin = root.path().join("llama.cpp").join("b12823").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(&exe), "mine").unwrap();
        let beside = describe(
            &state,
            &custom(&bin.join(&exe)),
            Some(12823),
            &hardware,
            Some(&checked("b12900", Some(&good))),
            true,
        );
        assert!(beside.available, "{beside:?}");
        assert_eq!(beside.target, "beside");
        assert_eq!(
            Path::new(&beside.target_dir.unwrap()),
            root.path().join("llama.cpp").join("b12900")
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
