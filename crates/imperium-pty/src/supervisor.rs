//! The Linux per-terminal supervisor (decision D5 of `docs/plans/terminal-file-panes.md`).
//!
//! The PTY's child is the daemon's own executable re-run as
//! `imperium-core pty-supervisor --daemon <pid>:<start time> -- <shell> <args>`. The supervisor
//! is the PTY's session leader and a child subreaper, so a process that leaves the shell's
//! session (`setsid`) or whose parent exits is reparented to it, found by the walk of `/proc`
//! and ended with the rest. It ends the terminal when the master closes (`SIGHUP`), when the
//! daemon says so (`SIGTERM`), when the daemon dies (a pidfd) or when the shell exits.
//!
//! Everything here uses safe wrappers (`rustix`, `signal-hook`, `std`); there is no `pre_exec`.
//! It runs before the daemon's runtime, logging or configuration exist and starts no thread.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::OwnedFd;
use rustix::io::Errno;
use rustix::process::{
    Pid, PidfdFlags, Signal, WaitOptions, WaitStatus, getpgrp, getpid, kill_process_group,
    pidfd_open, pidfd_send_signal, set_child_subreaper, wait,
};
use rustix::termios::{tcgetpgrp, tcsetpgrp};
use signal_hook::SigId;
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGTSTP, SIGTTIN, SIGTTOU};
use signal_hook::flag;
use signal_hook::low_level::pipe;

/// The hidden subcommand of the daemon's executable that runs the supervisor.
pub const SUBCOMMAND: &str = "pty-supervisor";

/// The supervisor was started wrongly.
pub const EXIT_USAGE: i32 = 120;
/// The host refuses a syscall the supervisor needs (`pidfd_open`): it fails closed.
pub const EXIT_MISSING_SYSCALL: i32 = 121;
/// The daemon it was started for is gone, or its pid is now another process's.
pub const EXIT_DAEMON_GONE: i32 = 122;
/// Something else in the set-up failed.
pub const EXIT_SETUP: i32 = 124;

/// How long `SIGHUP` gets before `SIGKILL`.
const HUP_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);

/// A start-up failure: what to say and what to exit with.
struct Failure {
    code: i32,
    message: String,
}

fn failure(code: i32, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

/// Says something on the terminal; the supervisor never panics over a closed one.
fn say(message: &str) {
    let _ = writeln!(io::stderr(), "pty-supervisor: {message}");
}

/// Fault injection for the tests; every method does nothing without the `test-support` feature.
#[derive(Default)]
struct Hooks {
    #[cfg(feature = "test-support")]
    delay_handover: Duration,
    #[cfg(feature = "test-support")]
    no_pidfd: bool,
    #[cfg(feature = "test-support")]
    trace: Option<std::path::PathBuf>,
}

#[cfg(feature = "test-support")]
impl Hooks {
    /// Takes a test flag (and its value) off the arguments; whether it was one.
    fn accept(&mut self, flag: &str, rest: &mut dyn Iterator<Item = OsString>) -> bool {
        match flag {
            "--test-delay-handover-ms" => {
                let millis = rest
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .and_then(|value| value.parse().ok());
                self.delay_handover = Duration::from_millis(millis.unwrap_or(0));
                true
            }
            "--test-no-pidfd" => {
                self.no_pidfd = true;
                true
            }
            "--test-trace" => {
                self.trace = rest.next().map(std::path::PathBuf::from);
                self.trace.is_some()
            }
            _ => false,
        }
    }

    fn delay_handover(&self) {
        sleep(self.delay_handover);
    }

    fn pidfd_open(&self, pid: Pid) -> rustix::io::Result<OwnedFd> {
        if self.no_pidfd {
            return Err(Errno::NOSYS);
        }
        pidfd_open(pid, PidfdFlags::empty())
    }

    fn trace(&self, event: &str) {
        if let Some(path) = &self.trace
            && let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(file, "{event}");
        }
    }
}

#[cfg(not(feature = "test-support"))]
impl Hooks {
    fn accept(&mut self, _flag: &str, _rest: &mut dyn Iterator<Item = OsString>) -> bool {
        false
    }

    fn delay_handover(&self) {}

    fn pidfd_open(&self, pid: Pid) -> rustix::io::Result<OwnedFd> {
        pidfd_open(pid, PidfdFlags::empty())
    }

    fn trace(&self, _event: &str) {}
}

struct Config {
    daemon_pid: u32,
    daemon_start: u64,
    program: OsString,
    args: Vec<OsString>,
    hooks: Hooks,
}

fn usage(message: &str) -> Failure {
    failure(
        EXIT_USAGE,
        format!("{message} (usage: {SUBCOMMAND} --daemon <pid>:<start> -- <program> [args])"),
    )
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Config, Failure> {
    let mut args = args.into_iter();
    let mut daemon = None;
    let mut hooks = Hooks::default();
    loop {
        let Some(arg) = args.next() else {
            return Err(usage("missing `--` and the program"));
        };
        match arg.to_str() {
            Some("--") => break,
            Some("--daemon") => {
                let value = args.next().and_then(|value| value.into_string().ok());
                let parsed = value.as_deref().and_then(|value| {
                    let (pid, start) = value.split_once(':')?;
                    Some((pid.parse::<u32>().ok()?, start.parse::<u64>().ok()?))
                });
                daemon = Some(parsed.ok_or_else(|| usage("bad --daemon value"))?);
            }
            Some(other) if hooks.accept(other, &mut args) => {}
            _ => return Err(usage("unknown argument")),
        }
    }
    let (daemon_pid, daemon_start) = daemon.ok_or_else(|| usage("missing --daemon"))?;
    let program = args.next().ok_or_else(|| usage("missing the program"))?;
    Ok(Config {
        daemon_pid,
        daemon_start,
        program,
        args: args.collect(),
        hooks,
    })
}

/// The fields of `/proc/<pid>/stat` the supervisor needs.
pub(crate) struct Stat {
    pub(crate) state: char,
    pub(crate) ppid: i32,
    pub(crate) session: i32,
    pub(crate) start: u64,
}

fn parse_stat(stat: &str) -> Option<Stat> {
    // The command name (field 2) may hold spaces and parentheses; everything after the last
    // `)` is plain: state (field 3), parent (4), group (5), session (6) ... start time (22), so
    // index 0, 1, 2, 3 and 19 here.
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    fields.next()?;
    let session = fields.next()?.parse().ok()?;
    let start = fields.nth(15)?.parse().ok()?;
    Some(Stat {
        state,
        ppid,
        session,
        start,
    })
}

pub(crate) fn read_stat(pid: i32) -> Option<Stat> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// The state of every thread of `pid`. A thread-group leader that called `SYS_exit` while other
/// threads live shows state `Z` in `/proc/<pid>/stat`, yet the process runs on in its other
/// threads; only its tasks tell.
pub(crate) fn thread_states(pid: i32) -> Vec<char> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .flatten()
        .filter_map(|task| {
            let stat = std::fs::read_to_string(task.path().join("stat")).ok()?;
            parse_stat(&stat).map(|stat| stat.state)
        })
        .collect()
}

/// Whether the process still runs (or is stopped) in any thread: not a zombie, or a zombie
/// leader with a live thread.
pub(crate) fn is_live(pid: i32, state: char) -> bool {
    state != 'Z' || thread_states(pid).iter().any(|&state| state != 'Z')
}

/// Whether the process is live and not stopped (so it may still fork).
pub(crate) fn is_running(pid: i32, state: char) -> bool {
    if state == 'Z' {
        thread_states(pid)
            .iter()
            .any(|&state| !matches!(state, 'Z' | 'T' | 't'))
    } else {
        !matches!(state, 'T' | 't')
    }
}

/// A process's start time (field 22 of `/proc/<pid>/stat`, in clock ticks after boot): with the
/// pid, it names one process for as long as the machine is up.
pub fn process_start_time(pid: u32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    parse_stat(&stat)
        .map(|stat| stat.start)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unreadable /proc stat"))
}

/// Whether this host lets a supervisor start: `pidfd_open` (Linux 5.3 and later) can answer
/// `ENOSYS`, or `EPERM` under a container's seccomp filter. The terminal is then refused.
pub fn check_host() -> Result<(), String> {
    match pidfd_open(getpid(), PidfdFlags::empty()) {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("pidfd_open failed: {error}")),
    }
}

/// Runs the supervisor with the arguments after the subcommand; the process's exit code.
pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    match supervise(args) {
        Ok(code) => code,
        Err(failure) => {
            say(&failure.message);
            failure.code
        }
    }
}

fn pid_of(raw: i32) -> Option<Pid> {
    Pid::from_raw(raw)
}

fn catch(result: io::Result<SigId>) -> Result<(), Failure> {
    result
        .map(|_| ())
        .map_err(|error| failure(EXIT_SETUP, format!("could not catch a signal: {error}")))
}

fn supervise(args: impl IntoIterator<Item = OsString>) -> Result<i32, Failure> {
    let config = parse(args)?;
    let me = getpid();
    set_child_subreaper(Some(me))
        .map_err(|error| failure(EXIT_SETUP, format!("could not become a subreaper: {error}")))?;

    // Signals: caught and discarded, never `SIG_IGN`. An ignored disposition is inherited across
    // `exec`, and a shell that starts with `SIGINT` ignored keeps it ignored for every command,
    // so Ctrl+C would stop working; a handler resets to the default on `exec`. No signal mask is
    // changed (a blocked mask is inherited too), which is why there is no `signalfd`. The
    // handlers are in place before the shell starts, so no `SIGCHLD` is missed.
    let discarded = Arc::new(AtomicBool::new(false));
    let end = Arc::new(AtomicBool::new(false));
    let (wake, wake_write) =
        UnixStream::pair().map_err(|error| failure(EXIT_SETUP, error.to_string()))?;
    wake.set_nonblocking(true)
        .map_err(|error| failure(EXIT_SETUP, error.to_string()))?;
    for signal in [SIGINT, SIGQUIT, SIGTSTP, SIGTTOU, SIGTTIN] {
        catch(flag::register(signal, Arc::clone(&discarded)))?;
    }
    // The terminal ends only on SIGHUP (the master closed) and SIGTERM (the daemon's `end`), the
    // daemon's death or the shell's exit. The flag says which; the pipe wakes the poll.
    for signal in [SIGHUP, SIGTERM] {
        catch(flag::register(signal, Arc::clone(&end)))?;
        let writer = wake_write
            .try_clone()
            .map_err(|error| failure(EXIT_SETUP, error.to_string()))?;
        catch(pipe::register(signal, writer))?;
    }
    catch(pipe::register(SIGCHLD, wake_write))?;

    // The daemon's death: its pidfd turns readable. Opened first, then the start time is read,
    // so a pid reused before the open shows as a different start time.
    let hooks = &config.hooks;
    let daemon_pid = pid_of(i32::try_from(config.daemon_pid).unwrap_or(0))
        .ok_or_else(|| usage("bad daemon pid"))?;
    let pidfd = match hooks.pidfd_open(daemon_pid) {
        Ok(fd) => fd,
        Err(Errno::SRCH) => return Err(failure(EXIT_DAEMON_GONE, "the daemon is already gone")),
        Err(error) => {
            return Err(failure(
                EXIT_MISSING_SYSCALL,
                format!(
                    "this host refuses pidfd_open ({error}); terminals need Linux 5.3 or later \
                     and a seccomp profile that allows it, so none can start here"
                ),
            ));
        }
    };
    match process_start_time(config.daemon_pid) {
        Ok(start) if start == config.daemon_start => {}
        _ => {
            return Err(failure(
                EXIT_DAEMON_GONE,
                "the daemon's pid is gone or now another process's",
            ));
        }
    }

    let child = Command::new(&config.program)
        .args(&config.args)
        .process_group(0)
        .spawn()
        .map_err(|error| {
            let code = if error.kind() == io::ErrorKind::NotFound {
                127
            } else {
                126
            };
            failure(
                code,
                format!("could not start {:?}: {error}", config.program),
            )
        })?;
    let shell = pid_of(i32::try_from(child.id()).unwrap_or(0))
        .ok_or_else(|| failure(EXIT_SETUP, "the shell has no pid"))?;

    hand_over_terminal(shell, hooks);

    let mut status = None;
    loop {
        let mut fds = [
            PollFd::new(&pidfd, PollFlags::IN),
            PollFd::new(&wake, PollFlags::IN),
        ];
        // A timeout as well as the self-pipe: a wake-up that goes missing then costs 200 ms, not
        // the terminal's end.
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 200_000_000,
        };
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(error) => {
                say(&format!("poll failed: {error}"));
                break;
            }
        }
        let daemon_gone = !fds[0].revents().is_empty();
        if !fds[1].revents().is_empty() {
            drain(&wake);
        }
        reap(shell, &mut status);
        if daemon_gone || end.load(Ordering::SeqCst) || status.is_some() {
            break;
        }
    }
    kill_all(me, shell, &mut status);
    Ok(status.unwrap_or(128 + 9))
}

/// Makes the shell's process group the terminal's foreground group, then continues it.
///
/// This is the supervisor's **only** `tcsetpgrp`, and it is made only while the supervisor's own
/// group is the terminal's foreground group (as it is at start, being the session leader): a
/// `tcsetpgrp` from a background group would raise `SIGTTOU`. It is done here in the parent
/// because a child-side hook needs `pre_exec`, which is `unsafe`. The shell starts in a
/// background group, so an interactive shell (or `python3 -q`) can stop itself with `SIGTTIN` or
/// `SIGTTOU` before the call lands; `SIGCONT` to its group afterwards resumes it. A Ctrl+C typed
/// in the moment before the call reaches only the supervisor's group, which discards it. The
/// supervisor reads, writes and changes nothing on the terminal after this.
fn hand_over_terminal(shell: Pid, hooks: &Hooks) {
    // Handlers are in place and the shell runs: a Ctrl+C from here on reaches only us.
    hooks.trace("shell started");
    hooks.delay_handover();
    let tty = io::stdin();
    match tcgetpgrp(&tty) {
        Ok(foreground) if foreground == getpgrp() => {
            hooks.trace("handover: foreground=true");
            match tcsetpgrp(&tty, shell) {
                Ok(()) => hooks.trace("tcsetpgrp: ok"),
                // The shell may already be gone; the loop sees its exit.
                Err(error) => hooks.trace(&format!("tcsetpgrp: {error}")),
            }
        }
        Ok(_) => hooks.trace("handover: foreground=false"),
        Err(error) => hooks.trace(&format!("handover: tcgetpgrp {error}")),
    }
    let _ = kill_process_group(shell, Signal::CONT);
    hooks.trace("sigcont: sent");
}

/// Empties the self-pipe.
fn drain(mut wake: &UnixStream) {
    let mut sink = [0u8; 64];
    while matches!(wake.read(&mut sink), Ok(count) if count > 0) {}
}

fn exit_code(status: WaitStatus) -> i32 {
    status
        .exit_status()
        .or_else(|| status.terminating_signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

/// Reaps every child that ended (`wait4(-1, WNOHANG)` until nothing is left), noting the
/// shell's status. It never blocks.
fn reap(shell: Pid, status: &mut Option<i32>) {
    loop {
        match wait(WaitOptions::NOHANG) {
            Ok(Some((pid, found))) => {
                if pid == shell {
                    *status = Some(exit_code(found));
                }
            }
            Err(Errno::INTR) => {}
            Ok(None) | Err(_) => break,
        }
    }
}

/// A descendant seen by the walk: its pid, the start time that tells it from a later process with
/// the same pid, and whether it is live and whether it is running (see [`is_live`]).
struct Target {
    pid: i32,
    start: u64,
    live: bool,
    running: bool,
}

/// Every descendant of `root`: children by parent pid, recursively. Zombies are kept in the
/// parent map and walked through (a zombie leader may have live threads and live children), and
/// each target says whether it is live.
fn descendants(root: i32) -> Vec<Target> {
    let mut children: HashMap<i32, Vec<(i32, Stat)>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        // A process that ended since the listing is skipped.
        if let Some(stat) = read_stat(pid) {
            children.entry(stat.ppid).or_default().push((pid, stat));
        }
    }
    let mut found = Vec::new();
    let mut queue = vec![root];
    while let Some(parent) = queue.pop() {
        for (pid, stat) in children.remove(&parent).unwrap_or_default() {
            queue.push(pid);
            found.push(Target {
                pid,
                start: stat.start,
                live: is_live(pid, stat.state),
                running: is_running(pid, stat.state),
            });
        }
    }
    found
}

/// The live descendants of `root`.
fn live_descendants(root: i32) -> Vec<Target> {
    let mut found = descendants(root);
    found.retain(|target| target.live);
    found
}

/// Signals `target` if it is still the process the walk saw.
///
/// Pid reuse between the walk and the signal (the plan's stage note 4) is closed, not accepted:
/// a pidfd is opened for whatever holds the pid now, the start time is read again, and the
/// signal goes through the pidfd only if the start time is the one the walk recorded. A process
/// that ended and whose pid was reused has a later start time, and an open pidfd cannot follow
/// the pid to another process. A zombie leader is signalled too: a signal to a thread group
/// reaches its live threads.
fn send_signal(target: &Target, signal: Signal) {
    let Some(pid) = pid_of(target.pid) else {
        return;
    };
    let Ok(pidfd) = pidfd_open(pid, PidfdFlags::empty()) else {
        return;
    };
    if read_stat(target.pid).is_some_and(|stat| stat.start == target.start) {
        let _ = pidfd_send_signal(&pidfd, signal);
    }
}

/// Ends every descendant: `SIGHUP` (and `SIGCONT`, so a stopped one gets it) and up to 2 s of
/// reaping; then, until nothing is left, **freeze, then kill**: every running descendant gets
/// `SIGSTOP` and the walk is repeated at once until none runs (a stopped process cannot fork, so
/// a fork bomb is overtaken), then all get `SIGKILL`. There is no bound after the grace: the
/// supervisor does not exit while a live descendant remains (the daemon's `end` then falls back
/// to its session sweep, which can also kill the supervisor). A future form is `cgroup.kill` of
/// a cgroup of the terminal's own, which kills the whole tree atomically where the host
/// delegates a cgroup to the user.
fn kill_all(me: Pid, shell: Pid, status: &mut Option<i32>) {
    let root = me.as_raw_pid();
    let first = live_descendants(root);
    if first.is_empty() {
        reap(shell, status);
        return;
    }
    for target in &first {
        send_signal(target, Signal::HUP);
        send_signal(target, Signal::CONT);
    }
    let deadline = Instant::now() + HUP_WAIT;
    loop {
        reap(shell, status);
        if live_descendants(root).is_empty() || Instant::now() >= deadline {
            break;
        }
        sleep(POLL);
    }
    loop {
        reap(shell, status);
        let left = live_descendants(root);
        if left.is_empty() {
            break;
        }
        // Freeze what still runs and look again at once, with no sleep: the walk finds the
        // children it forked meanwhile.
        let running: Vec<&Target> = left.iter().filter(|target| target.running).collect();
        if !running.is_empty() {
            for target in running {
                send_signal(target, Signal::STOP);
            }
            continue;
        }
        for target in &left {
            send_signal(target, Signal::KILL);
        }
        // Everything is frozen and killed; give the kernel a moment to end and reap it.
        sleep(Duration::from_millis(1));
    }
    reap(shell, status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_time_is_field_22_even_when_the_name_has_parentheses_and_spaces() {
        let stat = "123 (we ird) name)) S 1 123 123 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 \
                    987654 4096 10 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        let parsed = parse_stat(stat).expect("parses");
        assert_eq!(
            (parsed.state, parsed.ppid, parsed.session, parsed.start),
            ('S', 1, 123, 987_654)
        );
    }

    #[test]
    fn this_process_has_a_start_time() {
        assert!(process_start_time(std::process::id()).unwrap() > 0);
    }

    #[test]
    fn arguments_are_parsed_and_a_bad_one_is_refused() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        let Ok(config) = parse(args(&["--daemon", "42:99", "--", "/bin/sh", "-c", "true"])) else {
            panic!("a good command line parses");
        };
        assert_eq!((config.daemon_pid, config.daemon_start), (42, 99));
        assert_eq!(config.program, OsString::from("/bin/sh"));
        assert_eq!(config.args, args(&["-c", "true"]));
        for bad in [
            &["--", "/bin/sh"][..],
            &["--daemon", "x:1", "--", "/bin/sh"],
            &["--daemon", "1:2"],
            &["--daemon", "1:2", "--"],
            &["--bogus", "--", "/bin/sh"],
        ] {
            assert!(parse(args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_signal_goes_only_to_the_process_the_walk_saw() {
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep starts");
        let pid = i32::try_from(child.id()).unwrap();
        let start = process_start_time(child.id()).unwrap();
        // A different start time is a pid that was reused since the walk: nothing is sent.
        send_signal(
            &Target {
                pid,
                start: start + 1,
                live: true,
                running: true,
            },
            Signal::KILL,
        );
        sleep(Duration::from_millis(200));
        assert!(
            child.try_wait().unwrap().is_none(),
            "a process with another start time was signalled"
        );
        send_signal(
            &Target {
                pid,
                start,
                live: true,
                running: true,
            },
            Signal::KILL,
        );
        assert!(!child.wait().unwrap().success());
    }
}
