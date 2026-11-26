//! Stage 1 acceptance checks that hold on every OS: the real shell on a real pseudo terminal.

mod common;

#[cfg(unix)]
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use common::{BOUND, spec, wait_for, wait_until, work_dir};
#[cfg(unix)]
use imperium_pty::{Event, WriteError};
use imperium_pty::{Pty, SpawnError};

#[cfg(unix)]
const SHELL: &str = "/bin/sh";

#[cfg(windows)]
fn cmd() -> std::path::PathBuf {
    common::system32("cmd.exe")
}

#[cfg(windows)]
fn powershell() -> std::path::PathBuf {
    common::system32("WindowsPowerShell\\v1.0\\powershell.exe")
}

/// An interactive shell whose own answers cannot be mistaken for the typed command: the test
/// types `$((6*7))` or `%OS%` and waits for the expanded result.
fn spawn_shell(dir: &std::path::Path) -> std::sync::Arc<Pty> {
    #[cfg(unix)]
    let spec = spec(SHELL, &[], dir);
    #[cfg(windows)]
    let spec = spec(cmd(), &[], dir);
    common::spawn_pty(spec)
}

#[cfg(unix)]
const NEWLINE: &str = "\n";
#[cfg(windows)]
const NEWLINE: &str = "\r\n";

#[test]
fn an_echo_round_trip_reaches_the_shell_and_comes_back() {
    let dir = work_dir();
    let pty = spawn_shell(dir.path());
    #[cfg(unix)]
    let (typed, expected) = ("echo rt-$((6*7))", "rt-42");
    #[cfg(windows)]
    let (typed, expected) = ("echo rt-%OS%", "rt-Windows_NT");
    pty.write(format!("{typed}{NEWLINE}").as_bytes()).unwrap();
    // `contains`, not equality: ConPTY adds start-up sequences and the tty echoes the input.
    wait_for(&pty, expected);
    pty.end();
}

#[test]
fn the_working_folder_reaches_the_child() {
    let dir = work_dir();
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let pty = spawn_shell(dir.path());
    #[cfg(unix)]
    pty.write(b"pwd -P\n").unwrap();
    #[cfg(windows)]
    pty.write(b"cd\r\n").unwrap();
    wait_for(&pty, &name);
    pty.end();
}

#[test]
fn the_child_sees_the_size_it_was_given_and_a_resize() {
    let dir = work_dir();
    #[cfg(unix)]
    {
        let mut spec = spec(SHELL, &[], dir.path());
        (spec.cols, spec.rows) = (100, 30);
        let pty = common::spawn_pty(spec);
        pty.write(b"stty size\n").unwrap();
        wait_for(&pty, "30 100");
        pty.resize(120, 40).unwrap();
        pty.write(b"stty size\n").unwrap();
        wait_for(&pty, "40 120");
        pty.end();
    }
    #[cfg(windows)]
    {
        let mut spec = spec(powershell(), &["-NoLogo", "-NoProfile"], dir.path());
        (spec.cols, spec.rows) = (100, 30);
        let pty = common::spawn_pty(spec);
        let ask = "Write-Host ('SZ' + $Host.UI.RawUI.WindowSize.Width + 'x' + $Host.UI.RawUI.WindowSize.Height)\r";
        pty.write(ask.as_bytes()).unwrap();
        wait_for(&pty, "SZ100x30");
        pty.resize(120, 40).unwrap();
        pty.write(ask.as_bytes()).unwrap();
        wait_for(&pty, "SZ120x40");
        pty.end();
    }
}

#[cfg(unix)]
#[test]
fn a_background_job_dies_with_its_terminal() {
    let dir = work_dir();
    let pty = common::spawn_pty(spec(
        "/bin/bash",
        &["--norc", "--noprofile", "-i"],
        dir.path(),
    ));
    // `jobs -p`, not `$!`: bash 3.2 (macOS) takes the `!` of `$!` for history expansion.
    pty.write(
        b"sleep 60 & echo JOB:$(jobs -p):END
",
    )
    .unwrap();
    let job = common::wait_for_number(&pty, "JOB:", ":END");
    assert!(common::alive(job), "the job is running before the end");
    pty.end();
    common::wait_gone("the background job", job);
}

#[cfg(windows)]
#[test]
fn a_background_job_dies_with_its_terminal() {
    let dir = work_dir();
    let pty = common::spawn_pty(spec(cmd(), &[], dir.path()));
    pty.write(b"start /b ping -n 61 127.0.0.1 > nul\r\n")
        .unwrap();
    wait_until("the ping to start", || !common::ping_pids("61").is_empty());
    pty.end();
    wait_until("the ping to end", || common::ping_pids("61").is_empty());
}

#[cfg(windows)]
#[test]
fn the_job_alone_ends_what_is_in_it() {
    let dir = work_dir();
    let pty = common::spawn_pty(spec(cmd(), &[], dir.path()));
    pty.write(b"start /b ping -n 62 127.0.0.1 > nul\r\n")
        .unwrap();
    wait_until("the ping to start", || !common::ping_pids("62").is_empty());
    // The shell and what it started are in the job.
    let job = pty.job_process_ids().expect("the terminal has a job");
    assert!(
        job.contains(&pty.pid().expect("a pid")),
        "the shell is not in the job: {job:?}"
    );
    let ping = common::ping_pids("62")[0];
    assert!(job.contains(&ping), "the ping is not in the job: {job:?}");
    // Closing the job alone, with no walk of the process tree, ends them: this is what ends a
    // process the walk cannot reach (an orphan whose parent exited), and what a daemon crash does.
    pty.close_job_only();
    wait_until("the ping to end", || common::ping_pids("62").is_empty());
}

#[test]
fn the_exit_code_is_reported() {
    let dir = work_dir();
    #[cfg(unix)]
    let spec = spec(SHELL, &["-c", "exit 7"], dir.path());
    #[cfg(windows)]
    let spec = spec(cmd(), &["/c", "exit 7"], dir.path());
    let pty = common::spawn_pty(spec);
    let exit = pty.wait_exit(BOUND).unwrap_or_else(|| {
        panic!(
            "the shell never ended; output: {:?}; {}",
            common::output(&pty),
            common::describe(pty.pid())
        )
    });
    assert_eq!(exit.code, 7);
}

#[cfg(unix)]
#[test]
fn a_character_split_across_reads_arrives_whole() {
    let dir = work_dir();
    // F0 9F 98 80 is U+1F600, written in two pieces a third of a second apart.
    let script = "printf '\\360\\237'; sleep 0.3; printf '\\230\\200'; sleep 0.3";
    let pty = common::spawn_pty(spec(SHELL, &["-c", script], dir.path()));
    let chunks = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let sink = Arc::clone(&chunks);
    let (_, replay, _) = pty.subscribe(0, move |event| {
        if let Event::Data { bytes, .. } = event {
            sink.lock().unwrap().push(bytes.to_vec());
        }
    });
    assert!(std::str::from_utf8(&replay.data).is_ok());
    let mut all = replay.data;
    pty.wait_exit(BOUND).unwrap_or_else(|| {
        panic!(
            "the shell never ended; output: {:?}; {}",
            common::output(&pty),
            common::describe(pty.pid())
        )
    });
    wait_until("the output to be read", || pty.end_seq() >= 4);
    sleep(Duration::from_millis(200));
    for chunk in chunks.lock().unwrap().iter() {
        assert!(
            std::str::from_utf8(chunk).is_ok(),
            "a chunk split a character: {chunk:?}"
        );
        all.extend_from_slice(chunk);
    }
    assert!(String::from_utf8_lossy(&all).contains('\u{1F600}'));
}

#[cfg(unix)]
#[test]
fn a_two_mebibyte_burst_keeps_the_last_mebibyte_and_reports_the_drop() {
    let dir = work_dir();
    let script = "head -c 2097152 /dev/zero | tr '\\0' x; echo; echo BURST-END";
    let pty = common::spawn_pty(spec(SHELL, &["-c", script], dir.path()));
    wait_for(&pty, "BURST-END");
    let replay = pty.replay(0);
    assert!(pty.end_seq() >= 2 * 1024 * 1024);
    assert_eq!(replay.data.len(), imperium_pty::RING_CAPACITY);
    assert!(replay.dropped >= 1024 * 1024, "dropped {}", replay.dropped);
    assert_eq!(replay.from, replay.dropped);
}

#[cfg(unix)]
#[test]
fn a_full_input_queue_is_an_error_and_never_a_wait() {
    let dir = work_dir();
    // In raw mode the tty holds about 4 KiB of input; `sleep` never reads it, so the writer
    // thread stalls once that is full and the queue behind it fills.
    let script = "stty raw -echo; echo RAW-SET; exec sleep 60";
    let pty = common::spawn_pty(spec(SHELL, &["-c", script], dir.path()));
    wait_for(&pty, "RAW-SET");
    let chunk = vec![b'x'; 64 * 1024];
    let started = Instant::now();
    let mut refused = None;
    for count in 0..64 {
        if let Err(error) = pty.write(&chunk) {
            refused = Some((count, error));
            break;
        }
    }
    assert!(started.elapsed() < BOUND, "write must not block");
    assert_eq!(refused.map(|(_, error)| error), Some(WriteError::QueueFull));
    pty.end();
    assert_eq!(pty.write(b"x"), Err(WriteError::Closed));
}

#[test]
fn a_terminal_started_from_a_short_lived_thread_keeps_running() {
    let dir = work_dir();
    let path = dir.path().to_owned();
    #[cfg(unix)]
    let make = move || common::spawn_pty(spec(SHELL, &[], &path));
    #[cfg(windows)]
    let make = move || common::spawn_pty(spec(cmd(), &[], &path));
    // The thread that asked for the terminal ends; the spawner thread forked it, so it lives.
    let pty = std::thread::spawn(make).join().unwrap();
    sleep(Duration::from_millis(500));
    assert!(pty.exit().is_none(), "the terminal ended with the thread");
    #[cfg(unix)]
    pty.write(b"echo alive-$((6*7))\n").unwrap();
    #[cfg(windows)]
    pty.write(b"echo alive-%OS%\r\n").unwrap();
    #[cfg(unix)]
    wait_for(&pty, "alive-42");
    #[cfg(windows)]
    wait_for(&pty, "alive-Windows_NT");
    pty.end();
}

#[test]
fn a_bad_request_is_refused_before_anything_starts() {
    let dir = work_dir();
    #[cfg(unix)]
    let program = SHELL;
    #[cfg(windows)]
    let program = "cmd.exe";
    let mut tiny = spec(program, &[], dir.path());
    tiny.cols = 0;
    assert!(matches!(Pty::spawn(tiny), Err(SpawnError::BadSize)));
    let missing = spec(program, &[], &dir.path().join("not-there"));
    assert!(matches!(
        Pty::spawn(missing),
        Err(SpawnError::NotAFolder(_))
    ));
}

#[test]
fn the_environment_is_exactly_what_the_spec_says() {
    // A variable the test process has, left out of the spec's environment, is not in the shell's.
    #[cfg(unix)]
    let var = "HOME";
    #[cfg(windows)]
    let var = "USERNAME";
    assert!(
        std::env::var_os(var).is_some(),
        "the test needs {var} to be set in its own environment"
    );
    let dir = work_dir();
    #[cfg(unix)]
    let mut spec = spec(SHELL, &["-c", "echo \"ENV[${HOME-unset}]\""], dir.path());
    #[cfg(windows)]
    let mut spec = spec(cmd(), &["/c", "echo ENV[%USERNAME%]"], dir.path());
    spec.env
        .retain(|(key, _)| !key.to_string_lossy().eq_ignore_ascii_case(var));
    let pty = common::spawn_pty(spec);
    // An unset variable is empty to `sh` (here with a default) and left as typed by `cmd`.
    #[cfg(unix)]
    wait_for(&pty, "ENV[unset]");
    #[cfg(windows)]
    wait_for(&pty, "ENV[%USERNAME%]");
}
