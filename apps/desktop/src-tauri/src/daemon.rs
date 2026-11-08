//! The daemon behind the window. Installers ship `imperiumd` beside the app; when no daemon is
//! running the app starts that one, and before an update replaces it the app stops it.

use std::fs::File;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use imperium_config::Paths;
use imperium_protocol::discovery::{DaemonEndpoint, read_endpoint};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

/// How long a freshly started daemon may take to start listening.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// What the interface needs to open its WebSocket to `imperiumd`.
#[derive(Serialize)]
pub struct DaemonConnection {
    url: String,
    token: String,
}

/// The running daemon's endpoint, starting the bundled daemon first when none answers.
#[tauri::command]
pub async fn daemon_endpoint<R: Runtime>(app: AppHandle<R>) -> Result<DaemonConnection, String> {
    let paths = Paths::from_env().map_err(|error| error.to_string())?;
    let endpoint_file = paths.daemon_endpoint_file();
    if let Some(endpoint) = live_endpoint(&endpoint_file) {
        return Ok(connection(endpoint));
    }
    let Some(daemon) = bundled_daemon() else {
        // Development builds and archive installs run the daemon themselves.
        let error = read_endpoint(&endpoint_file).err();
        return Err(error.map_or_else(
            || "imperiumd is not answering; start it with `imperiumd`.".to_string(),
            |error| error.to_string(),
        ));
    };
    start(&app, &daemon)?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
        if let Some(endpoint) = live_endpoint(&endpoint_file) {
            return Ok(connection(endpoint));
        }
    }
    Err(format!(
        "imperiumd did not start within {} seconds; see {}",
        START_TIMEOUT.as_secs(),
        log_file(&app).map_or_else(|| "its log".into(), |log| log.display().to_string()),
    ))
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

fn log_file<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path()
        .app_log_dir()
        .ok()
        .map(|dir| dir.join("imperiumd.log"))
}

/// Starts the bundled daemon in the background. It keeps running when the window closes, so
/// sessions and schedules carry on; its output goes to the app's log folder.
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

/// Stops the daemon if it is the one bundled with this app, so an update can replace it and the
/// new version starts with the new app. A daemon the user runs from elsewhere is left alone.
pub fn stop_bundled() {
    let (Some(daemon), Ok(paths)) = (bundled_daemon(), Paths::from_env()) else {
        return;
    };
    let Ok(endpoint) = read_endpoint(&paths.daemon_endpoint_file()) else {
        return;
    };
    let pid = sysinfo::Pid::from_u32(endpoint.pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    let Some(process) = system.process(pid) else {
        return;
    };
    let same = |path: &Path| std::fs::canonicalize(path).ok();
    if process
        .exe()
        .and_then(same)
        .is_some_and(|exe| Some(exe) == same(&daemon))
    {
        process.kill();
        process.wait();
    }
}
