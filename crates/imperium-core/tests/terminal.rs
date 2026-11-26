//! Stages 2a and 2b of `docs/plans/terminal-file-panes.md`: the daemon's terminal service, called
//! in process, and `terminal.*` over the protocol behind the desktop-only gate ([`protocol`]).
//!
//! This test binary has no libtest harness (`harness = false`): its `main` first runs
//! [`imperium_core::run_pty_supervisor_if_asked`], as the daemon's own `main` does, so on Linux a
//! terminal opened with the production spec re-runs **this executable** through
//! `/proc/self/exe pty-supervisor` and the supervisor really runs (review N7). One test also
//! starts the real `imperium-core` executable as the supervisor, so its `main` dispatch runs too.
//! Every wait is bounded. The daemon's log is captured at TRACE for the whole run, so a test can
//! check that no terminal byte reaches it.

#[path = "terminal/protocol.rs"]
mod protocol;

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use imperium_core::DaemonState;
use imperium_core::terminal::shell::{ShellSources, resolve_shell, shell_env};
use imperium_core::terminal::{Terminal, TerminalError, UNATTACHED_LIMIT};

const BOUND: Duration = Duration::from_secs(15);
/// ConPTY holds the shell back until its cursor-position request is answered (xterm.js does it
/// in the app).
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

type Test = fn() -> Pin<Box<dyn Future<Output = ()>>>;

macro_rules! tests {
    ($($name:path),* $(,)?) => {
        &[$((
            stringify!($name),
            (|| -> Pin<Box<dyn Future<Output = ()>>> { Box::pin($name()) }) as Test,
        )),*]
    };
}

/// Everything the daemon logged in this run.
static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct Capture;

impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        LOG.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The daemon's log so far, at every level.
fn captured_log() -> String {
    String::from_utf8_lossy(
        &LOG.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
    .into_owned()
}

fn main() {
    imperium_core::run_pty_supervisor_if_asked();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(protocol::HELPER) {
        protocol::run_helper(&args[1..]);
        return;
    }
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(|| Capture)
        .init();
    let all: &[(&str, Test)] = tests![
        a_terminal_starts_in_the_workspace_root_with_the_default_shell,
        the_daemon_executable_itself_runs_the_supervisor,
        the_fifth_in_a_session_and_the_seventeenth_in_the_daemon_are_refused,
        archiving_the_session_ends_its_terminal_and_background_job,
        deleting_the_session_ends_its_terminal_and_background_job,
        a_terminal_with_nothing_attached_for_an_hour_ends,
        daemon_shutdown_ends_every_terminal_and_background_job,
        closing_a_terminal_ends_it_and_its_background_job,
        an_environment_file_cannot_choose_the_shell_or_turn_terminals_off,
        terminals_turned_off_are_refused,
        a_remote_runtime_session_and_an_archived_session_are_refused,
        protocol::a_client_with_the_daemon_token_cannot_open_or_type_into_a_terminal,
        protocol::a_token_holder_claiming_a_desktop_lease_is_refused,
        protocol::the_gate_runs_once_per_connection,
        protocol::open_prints_the_workspace_root_and_a_resize_reaches_the_shell,
        protocol::a_closed_connection_is_detached_and_the_hour_rule_applies,
        protocol::the_sentinel_stays_in_the_terminal,
        protocol::attach_with_since_replays_exactly_what_was_missed,
        protocol::a_lagging_connection_is_told_and_reattaches,
        protocol::opening_while_archiving_or_stopping_leaves_nothing,
        protocol::a_real_child_with_the_spawn_mark_is_refused,
        protocol::a_command_at_the_os_sandbox_tier_cannot_open_the_terminal_device,
        protocol::no_command_inherits_a_terminal_while_terminals_open,
        protocol::a_token_holder_cannot_change_the_terminal_settings,
        protocol::a_server_refuses_every_terminal,
        the_path_search_never_returns_the_working_folder_or_the_session_folders,
        unix_shell_choice,
        windows_shell_choice_ignores_comspec_and_path,
        the_environment_drops_start_up_and_loader_variables_and_keeps_the_mark,
        the_shell_path_keeps_only_absolute_entries,
    ];
    if args.iter().any(|arg| arg == "--list") {
        for (name, _) in all {
            println!("{name}: test");
        }
        return;
    }
    let filter = args.iter().find(|arg| !arg.starts_with('-'));
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, test) in all {
        if filter.is_some_and(|filter| !name.contains(filter.as_str())) {
            continue;
        }
        ran += 1;
        println!("test {name} ...");
        let outcome = std::panic::catch_unwind(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("a runtime")
                .block_on(test());
        });
        match outcome {
            Ok(()) => println!("test {name} ... ok"),
            Err(_) => {
                println!("test {name} ... FAILED");
                failed.push(*name);
            }
        }
    }
    println!("{ran} run, {} failed", failed.len());
    if !failed.is_empty() {
        println!("failed: {failed:?}");
        std::process::exit(1);
    }
}

fn settings(extra: &str) -> imperium_config::Settings {
    imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            format!(
                "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n[workflow]\nenabled = false\n[memory]\nenabled = false\n{extra}"
            )
            .parse()
            .unwrap(),
        ),
    ])
    .settings()
    .unwrap()
}

fn toml_string(path: &Path) -> String {
    format!("{:?}", path.display().to_string())
}

/// A shell whose job control the tests know: `/bin/sh -i` on Unix, cmd on Windows.
fn known_shell() -> String {
    #[cfg(unix)]
    {
        "[terminal]\nshell = [\"/bin/sh\", \"-i\"]\n".to_owned()
    }
    #[cfg(windows)]
    {
        let cmd =
            Path::new(&std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
                .join("System32")
                .join("cmd.exe");
        format!("[terminal]\nshell = [{}]\n", toml_string(&cmd))
    }
}

struct Fixture {
    _home: tempfile::TempDir,
    project: tempfile::TempDir,
    state: Arc<DaemonState>,
}

fn fixture(extra: &str) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::Builder::new()
        .prefix("imperium-terminal-root-")
        .tempdir()
        .unwrap();
    let state = DaemonState::new(
        imperium_config::Paths::under(home.path()),
        settings(extra),
        None,
    );
    Fixture {
        _home: home,
        project,
        state,
    }
}

impl Fixture {
    /// The daemon's home: its configuration and data folders.
    fn home(&self) -> &Path {
        self._home.path()
    }

    fn session(&self) -> String {
        let (session, _) = self
            .state
            .create_session(&self.project.path().display().to_string(), None)
            .unwrap();
        session.id().to_owned()
    }

    async fn open(&self, session: &str) -> Arc<Terminal> {
        let info = self
            .state
            .open_terminal(session, 80, 24)
            .await
            .unwrap_or_else(|error| panic!("the terminal opens: {error}"));
        let terminal = self.state.terminals().get(&info.terminal_id).unwrap();
        answer_cursor_queries(&terminal);
        terminal
    }
}

fn answer_cursor_queries(terminal: &Arc<Terminal>) {
    let weak = Arc::downgrade(terminal);
    let asks = |bytes: &[u8]| {
        bytes
            .windows(CURSOR_QUERY.len())
            .any(|part| part == CURSOR_QUERY)
    };
    let (_, replay, _) = terminal.pty().subscribe(0, move |event| {
        if let imperium_pty::Event::Data { bytes, .. } = event
            && asks(bytes)
            && let Some(terminal) = weak.upgrade()
        {
            let _ = terminal.pty().write(b"\x1b[1;1R");
        }
    });
    if asks(&replay.data) {
        let _ = terminal.pty().write(b"\x1b[1;1R");
    }
}

fn output(terminal: &Terminal) -> String {
    String::from_utf8_lossy(&terminal.pty().replay(0).data).into_owned()
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for(terminal: &Terminal, needle: &str) -> String {
    let mut text = String::new();
    wait_until(&format!("{needle:?} in the output"), || {
        text = output(terminal);
        text.contains(needle)
    })
    .await;
    text
}

/// The number printed right after `prefix` and before `suffix` (the echoed command line holds
/// the prefix too, followed by no digits).
#[cfg(unix)]
fn number_after(text: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        rest = &rest[at + prefix.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && rest[digits.len()..].starts_with(suffix) {
            return digits.parse().ok();
        }
    }
    None
}

/// A background job of the terminal's shell that the test can watch.
struct Job {
    #[cfg(unix)]
    pid: u32,
    #[cfg(windows)]
    marker: String,
}

/// Starts a background job in a terminal running [`known_shell`].
async fn start_job(terminal: &Terminal, count: u32) -> Job {
    #[cfg(unix)]
    {
        let _ = count;
        // The job prints its own pid and becomes `sleep`: `jobs -p` in `$(...)` runs in a subshell
        // with no jobs (dash), and bash 3.2 takes the `!` of `$!` for history expansion.
        terminal
            .pty()
            .write(b"sh -c 'echo JOB:$$:END; exec sleep 60' &\n")
            .unwrap();
        let mut pid = None;
        wait_until("the job's pid", || {
            pid = number_after(&output(terminal), "JOB:", ":END");
            pid.is_some()
        })
        .await;
        let job = Job { pid: pid.unwrap() };
        assert!(job.alive(), "the job runs");
        job
    }
    #[cfg(windows)]
    {
        let marker = count.to_string();
        terminal
            .pty()
            .write(format!("start /b ping -n {marker} 127.0.0.1 > nul\r\n").as_bytes())
            .unwrap();
        let job = Job { marker };
        wait_until("the ping to start", || job.alive()).await;
        job
    }
}

impl Job {
    fn alive(&self) -> bool {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        #[cfg(unix)]
        {
            system
                .process(sysinfo::Pid::from_u32(self.pid))
                .is_some_and(|process| process.status() != sysinfo::ProcessStatus::Zombie)
        }
        #[cfg(windows)]
        {
            system.processes().values().any(|process| {
                process.name().eq_ignore_ascii_case("ping.exe")
                    && process
                        .cmd()
                        .iter()
                        .any(|argument| argument.to_string_lossy() == self.marker)
            })
        }
    }

    async fn wait_gone(&self) {
        wait_until("the background job to end", || !self.alive()).await;
    }
}

/// The pid of the PTY's child: the supervisor on Linux.
#[cfg(target_os = "linux")]
fn supervisor_exe(terminal: &Terminal) -> PathBuf {
    let pid = terminal.pty().pid().expect("a pid");
    std::fs::read_link(format!("/proc/{pid}/exe")).expect("the supervisor's executable")
}

async fn a_terminal_starts_in_the_workspace_root_with_the_default_shell() {
    let fixture = fixture("");
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    let info = terminal.info();
    assert!(info.shell.is_absolute(), "{info:?}");
    assert!(!info.shell.starts_with(fixture.project.path()));
    assert_eq!((info.cols, info.rows), (80, 24));
    // `pwd` in sh, bash, zsh and PowerShell alike. The last component is compared: Windows
    // may show a short name for a parent, and macOS a /private prefix.
    terminal.pty().write(b"pwd\r").unwrap();
    let name = fixture
        .project
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let text = wait_for(&terminal, &name).await;
    assert!(text.contains(&name));
    #[cfg(target_os = "linux")]
    assert_eq!(
        supervisor_exe(&terminal),
        std::env::current_exe().unwrap(),
        "the production spec re-runs this executable through /proc/self/exe"
    );
    // Sizes are clamped.
    terminal.resize(5000, 0).unwrap();
    assert_eq!((terminal.info().cols, terminal.info().rows), (1000, 1));
    fixture.state.stop_terminals(BOUND).await;
}

async fn the_daemon_executable_itself_runs_the_supervisor() {
    #[cfg(target_os = "linux")]
    {
        let daemon = PathBuf::from(env!("CARGO_BIN_EXE_imperium-core"));
        imperium_core::terminal::testing::use_supervisor_exe(Some(daemon.clone()));
        let fixture = fixture(&known_shell());
        let session = fixture.session();
        let terminal = fixture.open(&session).await;
        imperium_core::terminal::testing::use_supervisor_exe(None);
        assert_eq!(
            supervisor_exe(&terminal),
            std::fs::canonicalize(&daemon).unwrap()
        );
        terminal.pty().write(b"echo up-$((6*7))\n").unwrap();
        wait_for(&terminal, "up-42").await;
        let job = start_job(&terminal, 64).await;
        fixture.state.close_terminal(terminal.id()).await.unwrap();
        job.wait_gone().await;
    }
}

async fn the_fifth_in_a_session_and_the_seventeenth_in_the_daemon_are_refused() {
    let fixture = fixture(&known_shell());
    let first = fixture.session();
    for _ in 0..4 {
        fixture.open(&first).await;
    }
    assert!(matches!(
        fixture.state.open_terminal(&first, 80, 24).await,
        Err(TerminalError::SessionLimit)
    ));
    assert_eq!(fixture.state.terminals().list(&first).len(), 4);
    for _ in 0..3 {
        let session = fixture.session();
        for _ in 0..4 {
            fixture.open(&session).await;
        }
    }
    assert_eq!(fixture.state.terminals().ids().len(), 16);
    let another = fixture.session();
    assert!(matches!(
        fixture.state.open_terminal(&another, 80, 24).await,
        Err(TerminalError::DaemonLimit)
    ));
    fixture.state.stop_terminals(BOUND).await;
    assert!(fixture.state.terminals().ids().is_empty());
}

async fn archiving_the_session_ends_its_terminal_and_background_job() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    let job = start_job(&terminal, 61).await;
    fixture.state.archive(&session).unwrap();
    assert!(fixture.state.terminals().list(&session).is_empty());
    job.wait_gone().await;
    wait_until("the shell to end", || terminal.pty().exit().is_some()).await;
}

async fn deleting_the_session_ends_its_terminal_and_background_job() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    let job = start_job(&terminal, 62).await;
    fixture.state.delete_session(&session, false).unwrap();
    assert!(fixture.state.terminals().get(terminal.id()).is_none());
    job.wait_gone().await;
}

async fn a_terminal_with_nothing_attached_for_an_hour_ends() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    let job = start_job(&terminal, 63).await;
    let state = &fixture.state;
    // The opener is attached: no time ends it.
    let later = Instant::now() + UNATTACHED_LIMIT * 2;
    assert!(state.end_unattached_terminals(later).is_empty());
    // A second viewer comes and goes; one is still attached.
    state.attach_terminal(terminal.id()).unwrap();
    state.detach_terminal(terminal.id()).unwrap();
    assert!(state.end_unattached_terminals(later).is_empty());
    // The last one goes: kept for an hour on the daemon's clock, then ended.
    state.detach_terminal(terminal.id()).unwrap();
    let detached = Instant::now();
    let almost = detached + UNATTACHED_LIMIT - Duration::from_secs(60);
    assert!(state.end_unattached_terminals(almost).is_empty());
    assert!(state.terminals().get(terminal.id()).is_some());
    let past = detached + UNATTACHED_LIMIT + Duration::from_secs(60);
    assert_eq!(state.end_unattached_terminals(past), [terminal.id()]);
    assert!(state.terminals().get(terminal.id()).is_none());
    job.wait_gone().await;
}

async fn daemon_shutdown_ends_every_terminal_and_background_job() {
    let fixture = fixture(&known_shell());
    let first = fixture.session();
    let second = fixture.session();
    let one = fixture.open(&first).await;
    let two = fixture.open(&second).await;
    let jobs = [start_job(&one, 65).await, start_job(&two, 66).await];
    // The daemon's shutdown wait (5 s).
    let started = Instant::now();
    fixture.state.stop_terminals(Duration::from_secs(5)).await;
    assert!(started.elapsed() < Duration::from_secs(6));
    assert!(fixture.state.terminals().ids().is_empty());
    for job in &jobs {
        job.wait_gone().await;
    }
}

async fn closing_a_terminal_ends_it_and_its_background_job() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    let job = start_job(&terminal, 67).await;
    fixture.state.close_terminal(terminal.id()).await.unwrap();
    assert!(fixture.state.terminals().list(&session).is_empty());
    job.wait_gone().await;
    assert!(matches!(
        fixture.state.close_terminal(terminal.id()).await,
        Err(TerminalError::NotFound(_))
    ));
}

async fn an_environment_file_cannot_choose_the_shell_or_turn_terminals_off() {
    let fixture = fixture("");
    let planted = fixture.project.path().join("planted-shell");
    std::fs::write(&planted, "#!/bin/sh\necho planted\n").unwrap();
    let dir = fixture.project.path().join(".imperium");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[terminal]\nenabled = false\nshell = [{}]\n",
            toml_string(&planted)
        ),
    )
    .unwrap();
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    assert_ne!(terminal.info().shell, planted);
    fixture.state.stop_terminals(BOUND).await;
}

async fn terminals_turned_off_are_refused() {
    let fixture = fixture("[terminal]\nenabled = false\n");
    let session = fixture.session();
    assert!(matches!(
        fixture.state.open_terminal(&session, 80, 24).await,
        Err(TerminalError::Disabled)
    ));
}

async fn a_remote_runtime_session_and_an_archived_session_are_refused() {
    let fixture = fixture(&known_shell());
    fixture.state.use_runner(Arc::new(NoRemote));
    let mut connection = imperium_core::Connection::new(Arc::clone(&fixture.state));
    let call =
        |connection: &mut imperium_core::Connection, method: &str, params: serde_json::Value| {
            let frame =
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
            match connection
                .handle_text(&frame.to_string())
                .response
                .unwrap_or_else(|| panic!("no immediate reply to {method}"))
                .outcome
            {
                imperium_protocol::jsonrpc::Outcome::Result(value) => value,
                imperium_protocol::jsonrpc::Outcome::Error(error) => panic!("{error:?}"),
            }
        };
    call(
        &mut connection,
        "system.handshake",
        serde_json::json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    let env = call(
        &mut connection,
        "environments.create",
        serde_json::json!({"name": "Box", "folders": ["/srv/app"],
               "runtime": {"kind": "ssh", "host": "me@box:2222"}}),
    );
    let created = call(
        &mut connection,
        "session.create",
        serde_json::json!({"environment_id": env["id"]}),
    );
    let remote = created["session_id"].as_str().unwrap().to_owned();
    assert!(matches!(
        fixture.state.open_terminal(&remote, 80, 24).await,
        Err(TerminalError::Remote)
    ));

    let session = fixture.session();
    fixture.state.archive(&session).unwrap();
    assert!(matches!(
        fixture.state.open_terminal(&session, 80, 24).await,
        Err(TerminalError::Archived(_))
    ));
    assert!(matches!(
        fixture.state.open_terminal("no-such-session", 80, 24).await,
        Err(TerminalError::NoSession(_))
    ));
    assert!(fixture.state.terminals().ids().is_empty());
}

#[cfg(unix)]
const NEWLINE: &str = "\n";
#[cfg(windows)]
const NEWLINE: &str = "\r\n";

/// A command for [`known_shell`] whose output differs from its echo.
fn typed(marker: &str) -> String {
    if cfg!(windows) {
        format!("echo {marker}-%OS%")
    } else {
        format!("echo {marker}-$((6*7))")
    }
}

/// What [`typed`] prints once the shell evaluated it.
fn evaluated(marker: &str) -> String {
    if cfg!(windows) {
        format!("{marker}-Windows_NT")
    } else {
        format!("{marker}-42")
    }
}

/// A runner for remote runtimes that runs nothing and says it worked.
#[derive(Debug)]
struct NoRemote;

impl imperium_tools::Runner for NoRemote {
    fn run<'a>(
        &'a self,
        _spec: &'a imperium_tools::ProcessSpec,
        _stdin: Option<&'a [u8]>,
        _timeout: Duration,
    ) -> imperium_tools::runtime::RunFuture<'a> {
        Box::pin(async {
            Ok(imperium_tools::RunOutput {
                code: Some(0),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        })
    }
}

/// A file named `name` in `dir`, executable on Unix.
fn program(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

async fn the_path_search_never_returns_the_working_folder_or_the_session_folders() {
    let name = if cfg!(windows) {
        "myshell.exe"
    } else {
        "myshell"
    };
    let session = tempfile::tempdir().unwrap();
    let inside = session.path().join("bin");
    std::fs::create_dir_all(&inside).unwrap();
    program(session.path(), name);
    program(&inside, name);
    let elsewhere = tempfile::tempdir().unwrap();
    let wanted = program(elsewhere.path(), name);
    // `.`, an empty entry, the session's folder and a folder inside it come first.
    let path = std::env::join_paths([
        PathBuf::from("."),
        PathBuf::new(),
        session.path().to_path_buf(),
        inside.clone(),
        PathBuf::from("bin"),
        elsewhere.path().to_path_buf(),
    ])
    .unwrap();
    let sources = ShellSources {
        configured: vec!["myshell".into(), "--login".into()],
        session_folders: vec![session.path().to_path_buf()],
        path: Some(path),
        ..ShellSources::default()
    };
    let shell = resolve_shell(&sources).unwrap();
    assert_eq!(shell.program, wanted);
    assert_eq!(shell.args, ["--login"]);

    // With only those entries, nothing is found.
    let only_bad = std::env::join_paths([
        PathBuf::from("."),
        PathBuf::new(),
        session.path().to_path_buf(),
        inside,
    ])
    .unwrap();
    let sources = ShellSources {
        path: Some(only_bad),
        ..sources
    };
    assert!(resolve_shell(&sources).is_err());
    // A relative program is refused outright.
    for relative in ["./myshell", "bin/myshell", "C:myshell"] {
        let sources = ShellSources {
            configured: vec![relative.into()],
            ..sources.clone()
        };
        assert!(resolve_shell(&sources).is_err(), "{relative}");
    }
}

async fn unix_shell_choice() {
    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().unwrap();
        let account = program(dir.path(), "account-sh");
        let env = program(dir.path(), "env-sh");
        let listed = |shells: &[&PathBuf]| {
            Some(
                shells
                    .iter()
                    .map(|shell| format!("{}\n", shell.display()))
                    .collect::<String>(),
            )
        };
        let choose = |etc_shells: Option<String>| {
            resolve_shell(&ShellSources {
                account_shell: Some(account.clone()),
                env_shell: Some(env.clone()),
                etc_shells,
                ..ShellSources::default()
            })
            .unwrap()
        };
        // Both listed: the account's shell is taken over $SHELL, as a login shell.
        let both = choose(listed(&[&account, &env]));
        assert_eq!(both.program, account);
        assert_eq!(both.args, ["-l"]);
        // An account shell missing from /etc/shells is skipped for $SHELL.
        assert_eq!(choose(listed(&[&env])).program, env);
        // A $SHELL missing from /etc/shells is refused too: /bin/sh.
        assert_eq!(choose(listed(&[])).program, PathBuf::from("/bin/sh"));
        assert_eq!(choose(None).program, PathBuf::from("/bin/sh"));
    }
}

async fn windows_shell_choice_ignores_comspec_and_path() {
    #[cfg(windows)]
    {
        // A planted pwsh.exe and cmd.exe on PATH; ComSpec is not an input at all.
        let planted = tempfile::tempdir().unwrap();
        program(planted.path(), "pwsh.exe");
        program(planted.path(), "cmd.exe");
        let mut sources = ShellSources::from_system(Vec::new(), Vec::new());
        sources.path = Some(planted.path().as_os_str().to_owned());
        let shell = resolve_shell(&sources).unwrap();
        assert!(!shell.program.starts_with(planted.path()), "{shell:?}");
        let program_files = sources.program_files.clone().unwrap();
        let windows = sources.windows_dir.clone().unwrap();
        let installed = shell.program.starts_with(program_files.join("PowerShell"))
            || sources.app_paths_pwsh.as_ref() == Some(&shell.program);
        let legacy =
            shell.program == windows.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        assert!(installed || legacy, "{shell:?}");
        // The runner has PowerShell 7 installed.
        assert!(
            shell.program.ends_with("pwsh.exe"),
            "the installed pwsh.exe is found: {shell:?}"
        );
        // Without PowerShell 7, Windows PowerShell.
        let legacy_only = ShellSources {
            program_files: None,
            app_paths_pwsh: None,
            ..sources
        };
        assert_eq!(
            resolve_shell(&legacy_only).unwrap().program,
            windows.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
        );
    }
}

async fn the_environment_drops_start_up_and_loader_variables_and_keeps_the_mark() {
    let inherited = [
        "ENV",
        "BASH_ENV",
        "ZDOTDIR",
        "PROMPT_COMMAND",
        "NODE_OPTIONS",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "IMPERIUM_SECRETS_PASSPHRASE_FILE",
        "IMPERIUM_DAEMON__PORT",
        "PYTHONSTARTUP",
        "PERL5OPT",
        "RUBYOPT",
        "JAVA_TOOL_OPTIONS",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_GLOBAL",
        "IMPERIUM_SPAWNED_BY_DAEMON",
        "TERM",
        "PWD",
        "IMPERIUM_HOME",
        "GH_TOKEN",
        "HOME",
    ]
    .map(|name| -> (OsString, OsString) { (name.into(), format!("inherited-{name}").into()) });
    let cwd = Path::new("/work/here");
    let env = shell_env(inherited, "session-7", cwd);
    let value = |name: &str| {
        let found: Vec<_> = env
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .collect();
        assert!(found.len() <= 1, "{name} is set once: {found:?}");
        found.into_iter().next()
    };
    for gone in [
        "ENV",
        "BASH_ENV",
        "ZDOTDIR",
        "PROMPT_COMMAND",
        "NODE_OPTIONS",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "IMPERIUM_SECRETS_PASSPHRASE_FILE",
        "IMPERIUM_DAEMON__PORT",
        "PYTHONSTARTUP",
        "PERL5OPT",
        "RUBYOPT",
        "JAVA_TOOL_OPTIONS",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_GLOBAL",
    ] {
        assert_eq!(value(gone), None, "{gone}");
    }
    // Windows: `cmd` and `CreateProcess` do not look in the working folder for a bare program.
    let no_current = value("NoDefaultCurrentDirectoryInExePath");
    if cfg!(windows) {
        assert_eq!(no_current.as_deref(), Some("1"));
    } else {
        assert_eq!(no_current, None);
    }
    assert_eq!(value("IMPERIUM_SPAWNED_BY_DAEMON").as_deref(), Some("1"));
    assert_eq!(value("IMPERIUM_SESSION_ID").as_deref(), Some("session-7"));
    assert_eq!(value("TERM").as_deref(), Some("xterm-256color"));
    assert_eq!(value("COLORTERM").as_deref(), Some("truecolor"));
    assert_eq!(value("TERM_PROGRAM").as_deref(), Some("Imperium"));
    assert!(value("TERM_PROGRAM_VERSION").is_some());
    assert_eq!(value("PWD"), Some(cwd.display().to_string()));
    // The user's own: their shell, outside every isolation tier and permission rule.
    assert_eq!(value("GH_TOKEN").as_deref(), Some("inherited-GH_TOKEN"));
    assert_eq!(value("HOME").as_deref(), Some("inherited-HOME"));
    assert_eq!(
        value("IMPERIUM_HOME").as_deref(),
        Some("inherited-IMPERIUM_HOME")
    );
}

/// `PATH` keeps only its absolute entries: an empty or relative one is searched from the
/// working folder, the repository.
async fn the_shell_path_keeps_only_absolute_entries() {
    let absolute: Vec<PathBuf> = if cfg!(windows) {
        vec![
            PathBuf::from(r"C:\Windows\System32"),
            PathBuf::from(r"D:\tools"),
        ]
    } else {
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/opt/tools/bin")]
    };
    let mut entries = vec![PathBuf::from("."), PathBuf::new(), PathBuf::from("bin")];
    entries.insert(1, absolute[0].clone());
    entries.push(absolute[1].clone());
    if cfg!(windows) {
        // Drive-relative and rooted-without-a-drive entries depend on the working folder too.
        entries.push(PathBuf::from(r"C:relative"));
        entries.push(PathBuf::from(r"\rooted"));
    }
    let path = std::env::join_paths(&entries).unwrap();
    // On Windows the name is `Path`, in any case.
    let name = if cfg!(windows) { "Path" } else { "PATH" };
    let env = shell_env([(OsString::from(name), path)], "s", Path::new("/w"));
    let kept: Vec<_> = env
        .iter()
        .filter(|(key, _)| key == name)
        .map(|(_, value)| std::env::split_paths(value).collect::<Vec<_>>())
        .collect();
    assert_eq!(kept, [absolute]);
}
