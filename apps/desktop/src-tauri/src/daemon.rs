//! The daemon behind the window. Installers ship `imperiumd` beside the app. The app connects to
//! a running daemon or starts that one, and lets the user see, start, stop, and restart it, read
//! its log, and choose whether it keeps running after the app closes. Before an update replaces
//! the bundled daemon, the app stops it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use imperium_client::Client;
use imperium_config::Paths;
use imperium_protocol::discovery::{DaemonEndpoint, read_endpoint};
use imperium_protocol::methods::{Peer, Shutdown, ShutdownParams};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

/// How long a freshly started daemon may take to start listening.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a daemon asked to stop may take before it is ended.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the stop request itself may take (connect, handshake, reply).
const ASK_TIMEOUT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// The most of the log the interface is sent.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// What the interface needs to open its WebSocket to `imperiumd`.
#[derive(Serialize)]
pub struct DaemonConnection {
    url: String,
    token: String,
}

/// What the interface shows about the daemon.
#[derive(Serialize)]
pub struct DaemonStatus {
    running: bool,
    pid: Option<u32>,
    url: Option<String>,
    /// Whether the running daemon is the one installed with this app.
    bundled: bool,
    /// Whether this install has a daemon it can start.
    can_start: bool,
    /// The user stopped it here; the app does not start it again until asked.
    stopped_by_user: bool,
    keep_running: bool,
    log_path: Option<String>,
}

/// The saved choice of whether the daemon outlives the window.
#[derive(Serialize, Deserialize)]
struct Preferences {
    keep_running: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { keep_running: true }
    }
}

/// Native state: whether the user has stopped the daemon from this app.
#[derive(Default)]
pub struct DaemonControl {
    stopped_by_user: AtomicBool,
}

/// The running daemon's endpoint, starting the bundled daemon first when none answers (unless
/// the user stopped it).
#[tauri::command]
pub async fn daemon_endpoint<R: Runtime>(
    app: AppHandle<R>,
    control: tauri::State<'_, DaemonControl>,
) -> Result<DaemonConnection, String> {
    let endpoint_file = endpoint_file()?;
    if let Some(endpoint) = live_endpoint(&endpoint_file) {
        return Ok(connection(endpoint));
    }
    if control.stopped_by_user.load(Ordering::Relaxed) {
        return Err("imperiumd is stopped. Start it from here or in Settings.".into());
    }
    let Some(daemon) = bundled_daemon() else {
        // Development builds and archive installs run the daemon themselves.
        let error = read_endpoint(&endpoint_file).err();
        return Err(error.map_or_else(
            || "imperiumd is not answering; start it with `imperiumd`.".to_string(),
            |error| error.to_string(),
        ));
    };
    start_and_wait(&app, &daemon, &endpoint_file)
        .await
        .map(connection)
}

#[tauri::command]
pub fn daemon_status<R: Runtime>(
    app: AppHandle<R>,
    control: tauri::State<'_, DaemonControl>,
) -> Result<DaemonStatus, String> {
    status(&app, &control)
}

/// Starts the bundled daemon if none is running.
#[tauri::command]
pub async fn daemon_start<R: Runtime>(
    app: AppHandle<R>,
    control: tauri::State<'_, DaemonControl>,
) -> Result<DaemonStatus, String> {
    control.stopped_by_user.store(false, Ordering::Relaxed);
    let endpoint_file = endpoint_file()?;
    if live_endpoint(&endpoint_file).is_none() {
        let daemon = bundled_daemon()
            .ok_or("This install has no imperiumd to start; run `imperiumd` yourself.")?;
        start_and_wait(&app, &daemon, &endpoint_file).await?;
    }
    status(&app, &control)
}

/// Stops the running daemon, whichever started it, and keeps it stopped until started again.
#[tauri::command]
pub async fn daemon_stop<R: Runtime>(
    app: AppHandle<R>,
    control: tauri::State<'_, DaemonControl>,
) -> Result<DaemonStatus, String> {
    control.stopped_by_user.store(true, Ordering::Relaxed);
    stop(app.package_info().version.to_string(), false).await?;
    status(&app, &control)
}

/// Stops the running daemon and starts the bundled one.
#[tauri::command]
pub async fn daemon_restart<R: Runtime>(
    app: AppHandle<R>,
    control: tauri::State<'_, DaemonControl>,
) -> Result<DaemonStatus, String> {
    let daemon = bundled_daemon()
        .ok_or("This install has no imperiumd to start; restart it where you started it.")?;
    stop(app.package_info().version.to_string(), false).await?;
    control.stopped_by_user.store(false, Ordering::Relaxed);
    start_and_wait(&app, &daemon, &endpoint_file()?).await?;
    status(&app, &control)
}

/// The end of the bundled daemon's log.
#[tauri::command]
pub fn daemon_log<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    let Some(path) = log_file(&app) else {
        return Ok(String::new());
    };
    let Ok(mut file) = File::open(&path) else {
        return Ok(String::new());
    };
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    let start = length.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    // Drop a partial first line when the tail starts mid-file.
    Ok(match (start > 0, text.find('\n')) {
        (true, Some(newline)) => text[newline + 1..].to_string(),
        _ => text.into_owned(),
    })
}

/// Chooses whether the daemon keeps running after the app closes.
#[tauri::command]
pub fn daemon_set_keep_running<R: Runtime>(
    app: AppHandle<R>,
    control: tauri::State<'_, DaemonControl>,
    keep_running: bool,
) -> Result<DaemonStatus, String> {
    let file = preferences_file(&app).ok_or("no settings folder")?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let bytes =
        serde_json::to_vec(&Preferences { keep_running }).map_err(|error| error.to_string())?;
    std::fs::write(file, bytes).map_err(|error| error.to_string())?;
    status(&app, &control)
}

/// As the app exits: stops the bundled daemon unless the user keeps it running.
pub fn on_exit<R: Runtime>(app: &AppHandle<R>) {
    if !preferences(app).keep_running {
        let version = app.package_info().version.to_string();
        let _ = tauri::async_runtime::block_on(stop(version, true));
    }
}

/// Stops the daemon if it is the one bundled with this app, so an update can replace it and the
/// new version starts with the new app. A daemon the user runs from elsewhere is left alone.
pub async fn stop_bundled<R: Runtime>(app: &AppHandle<R>) {
    let _ = stop(app.package_info().version.to_string(), true).await;
}

/// [`stop_bundled`] from outside the async runtime (the exit handler, synchronous commands).
pub fn stop_bundled_blocking<R: Runtime>(app: &AppHandle<R>) {
    tauri::async_runtime::block_on(stop_bundled(app));
}

fn status<R: Runtime>(app: &AppHandle<R>, control: &DaemonControl) -> Result<DaemonStatus, String> {
    let endpoint = live_endpoint(&endpoint_file()?);
    let bundled = bundled_daemon();
    Ok(DaemonStatus {
        running: endpoint.is_some(),
        pid: endpoint.as_ref().map(|endpoint| endpoint.pid),
        bundled: endpoint
            .as_ref()
            .zip(bundled.as_deref())
            .is_some_and(|(endpoint, daemon)| runs(endpoint.pid, daemon)),
        url: endpoint.map(|endpoint| endpoint.url),
        can_start: bundled.is_some(),
        stopped_by_user: control.stopped_by_user.load(Ordering::Relaxed),
        keep_running: preferences(app).keep_running,
        log_path: log_file(app).map(|path| path.display().to_string()),
    })
}

/// Asks the running daemon to stop and waits for it to exit, ending it if it does not. With
/// `only_bundled`, a daemon that is not this app's own is left running.
async fn stop(version: String, only_bundled: bool) -> Result<(), String> {
    let Some(endpoint) = live_endpoint(&endpoint_file()?) else {
        return Ok(());
    };
    if only_bundled && !bundled_daemon().is_some_and(|daemon| runs(endpoint.pid, &daemon)) {
        return Ok(());
    }
    let peer = Peer {
        name: "imperium-desktop".into(),
        version,
    };
    let ask = async {
        let mut client = Client::connect(&endpoint, peer).await.ok()?;
        client.call::<Shutdown>(ShutdownParams {}).await.ok()
    };
    let _ = tokio::time::timeout(ASK_TIMEOUT, ask).await;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !alive(endpoint.pid) {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
    end(endpoint.pid)
}

fn endpoint_file() -> Result<PathBuf, String> {
    Paths::from_env()
        .map(|paths| paths.daemon_endpoint_file())
        .map_err(|error| error.to_string())
}

fn connection(endpoint: DaemonEndpoint) -> DaemonConnection {
    DaemonConnection {
        url: endpoint.url,
        token: endpoint.token,
    }
}

/// The endpoint file's daemon, if it accepts connections. A stale file left by a daemon that
/// was killed points at a port nothing listens on.
fn live_endpoint(file: &Path) -> Option<DaemonEndpoint> {
    let endpoint = read_endpoint(file).ok()?;
    let address: SocketAddr = endpoint
        .url
        .strip_prefix("ws://")?
        .split('/')
        .next()?
        .parse()
        .ok()?;
    TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok()?;
    Some(endpoint)
}

/// `imperiumd` installed beside the app, if this is an installed build.
fn bundled_daemon() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let daemon = exe.with_file_name(format!("imperiumd{}", std::env::consts::EXE_SUFFIX));
    daemon.is_file().then_some(daemon)
}

fn process(system: &mut sysinfo::System, pid: u32) -> Option<&sysinfo::Process> {
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid)
}

fn alive(pid: u32) -> bool {
    process(&mut sysinfo::System::new(), pid).is_some()
}

/// Whether process `pid` is running the executable at `daemon`.
fn runs(pid: u32, daemon: &Path) -> bool {
    let mut system = sysinfo::System::new();
    let same = |path: &Path| std::fs::canonicalize(path).ok();
    process(&mut system, pid)
        .and_then(|process| process.exe())
        .and_then(same)
        .is_some_and(|exe| Some(exe) == same(daemon))
}

/// Ends a daemon that did not stop when asked. Only an `imperiumd` process is ended, in case
/// the endpoint file's process id now belongs to something else.
fn end(pid: u32) -> Result<(), String> {
    let mut system = sysinfo::System::new();
    let Some(process) = process(&mut system, pid) else {
        return Ok(());
    };
    if !process.name().to_string_lossy().starts_with("imperiumd") {
        return Err(format!("process {pid} is not imperiumd; not ending it"));
    }
    process.kill();
    process.wait();
    Ok(())
}

fn log_file<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_log_dir()
        .ok()
        .map(|dir| dir.join("imperiumd.log"))
}

fn preferences_file<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("daemon.json"))
}

fn preferences<R: Runtime>(app: &AppHandle<R>) -> Preferences {
    preferences_file(app)
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Starts the bundled daemon and waits until it accepts connections.
async fn start_and_wait<R: Runtime>(
    app: &AppHandle<R>,
    daemon: &Path,
    endpoint_file: &Path,
) -> Result<DaemonEndpoint, String> {
    start(app, daemon)?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        if let Some(endpoint) = live_endpoint(endpoint_file) {
            return Ok(endpoint);
        }
    }
    Err(format!(
        "imperiumd did not start within {} seconds; see {}",
        START_TIMEOUT.as_secs(),
        log_file(app).map_or_else(|| "its log".into(), |log| log.display().to_string()),
    ))
}

/// Starts the bundled daemon in the background; its output goes to the app's log folder.
fn start<R: Runtime>(app: &AppHandle<R>, daemon: &Path) -> Result<(), String> {
    let log = log_file(app).and_then(|log| {
        std::fs::create_dir_all(log.parent()?).ok()?;
        File::options().create(true).append(true).open(log).ok()
    });
    let mut command = Command::new(daemon);
    command.stdin(Stdio::null()).stdout(Stdio::null());
    command.stderr(log.map_or_else(Stdio::null, Stdio::from));
    // Bundled workflow packs and pets live in the resource folder, apart from the binaries.
    if let Ok(resources) = app.path().resource_dir() {
        command.env("IMPERIUM_RESOURCE_DIR", resources);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .map(drop)
        .map_err(|error| format!("could not start {}: {error}", daemon.display()))
}
