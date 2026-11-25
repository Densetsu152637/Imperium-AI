//! Helpers shared by the integration tests. Every wait is bounded by [`BOUND`].
#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use imperium_pty::{Event, Pty, Spec};

/// The longest any wait lasts.
pub const BOUND: Duration = Duration::from_secs(10);

/// A spec with this process's whole environment and, on Linux, the test supervisor binary.
pub fn spec(program: impl Into<OsString>, args: &[&str], cwd: &Path) -> Spec {
    let mut spec = Spec::new(program, cwd);
    spec.args = args.iter().map(OsString::from).collect();
    spec.env = std::env::vars_os().collect();
    spec.env.push(("TERM".into(), "xterm-256color".into()));
    #[cfg(target_os = "linux")]
    {
        spec.supervisor = Some(imperium_pty::Supervisor {
            exe: env!("CARGO_BIN_EXE_imperium-pty-supervisor").into(),
            args: Vec::new(),
            flags: Vec::new(),
        });
    }
    spec
}

/// The same, with extra flags for the Linux supervisor (its test-support hooks).
#[cfg(target_os = "linux")]
pub fn spec_with_flags(
    program: impl Into<OsString>,
    args: &[&str],
    cwd: &Path,
    flags: &[&str],
) -> Spec {
    let mut spec = spec(program, args, cwd);
    if let Some(supervisor) = spec.supervisor.as_mut() {
        supervisor.flags = flags.iter().map(OsString::from).collect();
    }
    spec
}

/// A fresh working folder with a long, distinctive name (Windows may show a short name for its
/// parents, never for the last component).
pub fn work_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("imperium-pty-cwd-")
        .tempdir()
        .expect("a temporary folder")
}

pub fn output(pty: &Pty) -> String {
    String::from_utf8_lossy(&pty.replay(0).data).into_owned()
}

/// Waits until the terminal's output holds `needle`; the output so far.
pub fn wait_for(pty: &Pty, needle: &str) -> String {
    let deadline = Instant::now() + BOUND;
    loop {
        let text = output(pty);
        if text.contains(needle) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; output so far: {text:?}"
        );
        sleep(Duration::from_millis(25));
    }
}

/// Waits until `done` is true.
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(25));
    }
}

/// The number between `prefix` and `suffix` in `text` (the typed command shows `$!`, the
/// result shows digits, so only the result matches).
pub fn number_between(text: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let from = text.find(prefix)? + prefix.len();
    let rest = &text[from..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(suffix) {
        // The typed text and the result both start with the prefix; look further on.
        return number_between(rest, prefix, suffix);
    }
    digits.parse().ok()
}

/// Waits for the number printed between `prefix` and `suffix`.
pub fn wait_for_number(pty: &Pty, prefix: &str, suffix: &str) -> u32 {
    let deadline = Instant::now() + BOUND;
    loop {
        let text = output(pty);
        if let Some(number) = number_between(&text, prefix, suffix) {
            return number;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {prefix}<number>{suffix}; output so far: {text:?}"
        );
        sleep(Duration::from_millis(25));
    }
}

/// Waits for a pid file to hold a number.
pub fn wait_for_pid_file(path: &Path) -> u32 {
    let deadline = Instant::now() + BOUND;
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        sleep(Duration::from_millis(25));
    }
}

/// Whether a process is running (a zombie is not).
#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                let rest = stat.rsplit(')').next()?.trim().to_owned();
                rest.chars().next()
            })
            .is_some_and(|state| state != 'Z')
    }
    #[cfg(not(target_os = "linux"))]
    {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }
}

#[cfg(unix)]
pub fn wait_gone(what: &str, pid: u32) {
    wait_until(&format!("{what} (pid {pid}) to end"), || !alive(pid));
}

/// Field `index` after the command name in `/proc/<pid>/stat` (0 is the state, 1 the parent, 2
/// the process group, 3 the session).
#[cfg(target_os = "linux")]
pub fn stat_field(pid: u32, index: usize) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(index).map(str::to_owned)
}

/// The live processes of session `sid`.
#[cfg(target_os = "linux")]
pub fn session_members(sid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").expect("/proc").flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if stat_field(pid, 3).is_some_and(|session| session == sid.to_string())
            && stat_field(pid, 0).is_some_and(|state| state != "Z")
        {
            found.push(pid);
        }
    }
    found
}

/// A program on `PATH` (tests only; the product always uses an absolute path of its own).
pub fn find_program(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[cfg(windows)]
pub fn system32(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&root).join("System32").join(name)
}

/// What a stuck process is doing (for a failure message): its status, wait channel and children.
pub fn describe(pid: Option<u32>) -> String {
    let Some(pid) = pid else {
        return "no pid".into();
    };
    #[cfg(target_os = "linux")]
    {
        let read =
            |name: &str| std::fs::read_to_string(format!("/proc/{pid}/{name}")).unwrap_or_default();
        let status: String = read("status")
            .lines()
            .filter(|line| {
                ["State", "PPid", "SigPnd", "SigBlk", "SigIgn", "SigCgt"]
                    .iter()
                    .any(|key| line.starts_with(key))
            })
            .collect::<Vec<_>>()
            .join("; ");
        let children = read(&format!("task/{pid}/children"));
        format!(
            "pid {pid}: {status}; wchan {}; children [{children}]",
            read("wchan")
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        format!("pid {pid}")
    }
}

/// The cursor-position request ConPTY sends at start-up and waits an answer to.
const CURSOR_QUERY: &[u8] = b"[6n";

/// Starts a terminal and plays the part of the emulator that answers a cursor-position request
/// (ConPTY holds back the shell until it is answered; xterm.js does it in the app).
pub fn spawn_pty(spec: Spec) -> Arc<Pty> {
    let pty = Arc::new(Pty::spawn(spec).expect("the terminal starts"));
    let weak = Arc::downgrade(&pty);
    let (_, replay) = pty.subscribe(0, move |event| {
        if let Event::Data { bytes, .. } = event
            && bytes
                .windows(CURSOR_QUERY.len())
                .any(|part| part == CURSOR_QUERY)
            && let Some(pty) = weak.upgrade()
        {
            let _ = pty.write(b"[1;1R");
        }
    });
    if replay
        .data
        .windows(CURSOR_QUERY.len())
        .any(|part| part == CURSOR_QUERY)
    {
        let _ = pty.write(b"[1;1R");
    }
    pty
}
