//! Stage 1 acceptance checks of the Linux supervisor and the plan's stage notes.
#![cfg(target_os = "linux")]

mod common;

use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use common::{
    BOUND, alive, alive_threads, find_program, session_members, spec, spec_with_flags, stat_field,
    wait_for, wait_for_number, wait_for_pid_file, wait_gone, wait_until, work_dir,
};
use imperium_pty::supervisor::{EXIT_DAEMON_GONE, EXIT_MISSING_SYSCALL, process_start_time};
use imperium_pty::{DaemonRef, Pty};
use rustix::process::{Pid, Signal, kill_process};

const SHELL: &str = "/bin/sh";
const BASH: &str = "/bin/bash";
const BASH_ARGS: [&str; 3] = ["--norc", "--noprofile", "-i"];
/// A background job in a session of its own that writes its pid to the file named by `$0`, then
/// the shell waits, so the terminal keeps running.
const SETSID_JOB: &str = "setsid sh -c 'echo $$ > \"$1\"; exec sleep 60' sh \"$0\" & sleep 60";

fn pid_of(raw: u32) -> Pid {
    Pid::from_raw(i32::try_from(raw).unwrap()).unwrap()
}

#[test]
fn a_job_that_calls_setsid_is_gone_after_end() {
    let dir = work_dir();
    let pty = Pty::spawn(spec(BASH, &BASH_ARGS, dir.path())).unwrap();
    pty.write(b"setsid sh -c 'echo SPID:$$:E; exec sleep 60' &\n")
        .unwrap();
    let job = wait_for_number(&pty, "SPID:", ":E");
    let supervisor = pty.pid().unwrap();
    // The job really left the terminal's session, so only the supervisor can find it.
    assert_ne!(stat_field(job, 3), Some(supervisor.to_string()));
    assert!(alive(job));
    pty.end();
    wait_gone("the setsid job", job);
}

/// Ending a terminal signals only what the terminal started. A process that shares this test
/// harness's session and process group (as every step of a CI job shares its runner's, and every
/// child of the daemon shares the daemon's) outlives the supervisor's kill-all and the daemon's
/// session sweep, unstopped. A background job, a `setsid` job and a fork loop give the kill-all
/// and the sweep work to do; the loop is bounded (300 rounds), so a supervisor that failed to end
/// it could not grow it for ever.
#[test]
fn ending_a_terminal_leaves_the_daemons_own_session_alone() {
    /// Kills and reaps the sibling on every way out of the test, a failed assertion included.
    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let dir = work_dir();
    let mut sibling = Reaped(
        Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let me = std::process::id();
    assert_eq!(
        stat_field(sibling.0.id(), 2),
        stat_field(me, 2),
        "same group"
    );
    assert_eq!(
        stat_field(sibling.0.id(), 3),
        stat_field(me, 3),
        "same session"
    );
    let pty = Pty::spawn(spec(BASH, &BASH_ARGS, dir.path())).unwrap();
    pty.write(
        b"sleep 60 & setsid sleep 60 & (for i in $(seq 300); do sleep 60 & sleep 0.01; done) & echo GO-$((6*7))\n",
    )
    .unwrap();
    wait_for(&pty, "GO-42");
    let supervisor = pty.pid().unwrap();
    wait_until("the loop to be forking", || {
        session_members(supervisor).len() > 5
    });
    pty.end();
    wait_until("the terminal's session to be empty", || {
        session_members(supervisor).is_empty()
    });
    // Late signals (a sweep still running) would land in this window.
    sleep(Duration::from_millis(500));
    let state = stat_field(sibling.0.id(), 0);
    assert!(
        matches!(state.as_deref(), Some("S" | "R")),
        "the harness's sibling was stopped or ended (state {state:?})"
    );
    assert!(sibling.0.try_wait().unwrap().is_none(), "the sibling ended");
}

#[test]
fn closing_the_pty_master_leaves_no_setsid_grandchild() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let dir = work_dir();
    let pid_file = dir.path().join("job.pid");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let me = std::process::id();
    let argv: Vec<std::ffi::OsString> = vec![
        env!("CARGO_BIN_EXE_imperium-pty-supervisor").into(),
        "--daemon".into(),
        format!("{me}:{}", process_start_time(me).unwrap()).into(),
        "--".into(),
        SHELL.into(),
        "-c".into(),
        SETSID_JOB.into(),
        pid_file.clone().into_os_string(),
    ];
    let mut command = CommandBuilder::from_argv(argv);
    command.cwd(dir.path());
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let job = wait_for_pid_file(&pid_file);
    assert!(alive(job));
    // What a daemon crash does: every descriptor of the master closes.
    drop(pair.master);
    wait_gone("the grandchild", job);
    wait_until("the supervisor to exit", || {
        child.try_wait().is_ok_and(|status| status.is_some())
    });
}

#[test]
fn killing_the_daemon_with_sigkill_ends_the_supervisor_the_shell_and_its_jobs() {
    let dir = work_dir();
    let pid_file = dir.path().join("job.pid");
    let mut standin = Command::new("sleep")
        .arg("300")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut spec = spec(
        SHELL,
        &["-c", SETSID_JOB, pid_file.to_str().unwrap()],
        dir.path(),
    );
    spec.daemon = Some(DaemonRef {
        pid: standin.id(),
        start_time: process_start_time(standin.id()).unwrap(),
    });
    let pty = Pty::spawn(spec).unwrap();
    let job = wait_for_pid_file(&pid_file);
    assert!(alive(job));
    assert!(pty.exit().is_none());
    standin.kill().unwrap();
    standin.wait().unwrap();
    assert!(pty.wait_exit(BOUND).is_some(), "the supervisor must end");
    wait_gone("the job", job);
    wait_gone("the supervisor", pty.pid().unwrap());
}

#[test]
fn a_wrong_start_time_makes_the_supervisor_exit_at_once() {
    let dir = work_dir();
    let mut standin = Command::new("sleep")
        .arg("300")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut spec = spec(SHELL, &["-c", "sleep 60"], dir.path());
    spec.daemon = Some(DaemonRef {
        pid: standin.id(),
        start_time: process_start_time(standin.id()).unwrap() + 1,
    });
    let started = Instant::now();
    let pty = Pty::spawn(spec).unwrap();
    let exit = pty.wait_exit(BOUND).expect("the supervisor exits");
    assert_eq!(i64::from(exit.code), i64::from(EXIT_DAEMON_GONE));
    assert!(started.elapsed() < BOUND);
    wait_for(&pty, "another process");
    standin.kill().unwrap();
    standin.wait().unwrap();
}

#[test]
fn sigterm_to_the_supervisor_ends_every_descendant() {
    let dir = work_dir();
    let pty = Pty::spawn(spec(BASH, &BASH_ARGS, dir.path())).unwrap();
    pty.write(b"sleep 60 & echo JOB:$!:END\n").unwrap();
    let job = wait_for_number(&pty, "JOB:", ":END");
    assert!(alive(job));
    kill_process(pid_of(pty.pid().unwrap()), Signal::TERM).unwrap();
    assert!(pty.wait_exit(BOUND).is_some());
    wait_gone("the job", job);
}

#[test]
fn a_fork_loop_is_gone_after_end_within_the_bound() {
    let dir = work_dir();
    let script = "while :; do sleep 60 & done";
    let pty = Pty::spawn(spec(SHELL, &["-c", script], dir.path())).unwrap();
    let supervisor = pty.pid().unwrap();
    wait_until("the loop to be forking", || {
        session_members(supervisor).len() > 20
    });
    let started = Instant::now();
    pty.end();
    assert!(
        started.elapsed() < BOUND,
        "end took {:?}",
        started.elapsed()
    );
    wait_until("the session to be empty", || {
        session_members(supervisor).is_empty()
    });
}

#[test]
fn no_zombie_is_left_under_the_supervisor_while_the_shell_runs() {
    let dir = work_dir();
    // The subshell leaves a background `sleep` behind; it is reparented to the supervisor (the
    // subreaper), which must reap it when it ends.
    let script = "(sleep 0.2 &); sleep 60";
    let pty = Pty::spawn(spec(SHELL, &["-c", script], dir.path())).unwrap();
    let supervisor = pty.pid().unwrap();
    sleep(Duration::from_millis(1500));
    let zombies: Vec<u32> = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| {
            stat_field(pid, 1) == Some(supervisor.to_string())
                && stat_field(pid, 0).as_deref() == Some("Z")
        })
        .collect();
    assert!(
        zombies.is_empty(),
        "zombies under the supervisor: {zombies:?}"
    );
    assert!(pty.exit().is_none());
    pty.end();
}

#[test]
fn the_supervisor_does_not_reap_for_the_daemon() {
    let dir = work_dir();
    let pty = Pty::spawn(spec(SHELL, &["-c", "sleep 60"], dir.path())).unwrap();
    // This process is not a subreaper, so its own child's status is still its own to wait for.
    let status = Command::new(SHELL).args(["-c", "exit 7"]).status().unwrap();
    assert_eq!(status.code(), Some(7));
    pty.end();
}

#[test]
fn the_shell_holds_no_descriptor_above_two() {
    let dir = work_dir();
    let pty = Pty::spawn(spec(SHELL, &["-c", "ls /proc/$$/fd"], dir.path())).unwrap();
    pty.wait_exit(BOUND).expect("the shell ends");
    wait_until("the listing", || {
        common::output(&pty).split_whitespace().count() >= 3
    });
    sleep(Duration::from_millis(200));
    let text = common::output(&pty);
    let mut fds: Vec<&str> = text.split_whitespace().collect();
    fds.sort_unstable();
    assert_eq!(fds, ["0", "1", "2"], "output: {text:?}");
}

#[test]
fn ctrl_c_is_ignored_by_a_shell_that_ignores_it() {
    let dir = work_dir();
    let script = "trap '' INT; echo UP; sleep 30";
    let pty = Pty::spawn(spec(SHELL, &["-c", script], dir.path())).unwrap();
    wait_for(&pty, "UP");
    sleep(Duration::from_millis(500));
    pty.write(b"\x03").unwrap();
    sleep(Duration::from_millis(1000));
    assert!(pty.exit().is_none(), "Ctrl+C ended a shell that ignores it");
    pty.end();
}

#[test]
fn ctrl_c_right_after_spawn_does_not_end_the_terminal() {
    let dir = work_dir();
    let trace = dir.path().join("trace.txt");
    // The handover of the terminal is held back for 1.5 s, so the Ctrl+C certainly arrives while
    // the supervisor's group is the foreground group and reaches only the supervisor. It is sent
    // as soon as the supervisor says its handlers are in place and the shell runs (before that,
    // a few milliseconds after `exec`, the default action would apply).
    let pty = Pty::spawn(spec_with_flags(
        SHELL,
        &["-c", "sleep 30"],
        dir.path(),
        &[
            "--test-delay-handover-ms",
            "1500",
            "--test-trace",
            trace.to_str().unwrap(),
        ],
    ))
    .unwrap();
    wait_until("the shell to start", || {
        std::fs::read_to_string(&trace).is_ok_and(|text| text.contains("shell started"))
    });
    pty.write(b"").unwrap();
    sleep(Duration::from_millis(2500));
    assert!(pty.exit().is_none(), "the early Ctrl+C ended the terminal");
    pty.end();
}

#[test]
fn ctrl_c_in_an_interactive_shell_ends_the_command_and_prompts_again() {
    let dir = work_dir();
    let pty = Pty::spawn(spec(BASH, &BASH_ARGS, dir.path())).unwrap();
    pty.write(b"sleep 30\n").unwrap();
    sleep(Duration::from_millis(1000));
    pty.write(b"\x03").unwrap();
    pty.write(b"echo up-$((20+22))\n").unwrap();
    // The shell did not inherit an ignored SIGINT: `sleep` ended and the next command ran,
    // long before the 30 s of the sleep.
    wait_for(&pty, "up-42");
    assert!(pty.exit().is_none());
    pty.end();
}

#[test]
fn a_late_handover_still_lets_bash_and_python_reach_a_prompt() {
    for (program, args, typed, expected) in [
        (
            Some(BASH.into()),
            BASH_ARGS.to_vec(),
            "echo up-$((20+22))\n",
            "up-42",
        ),
        (
            find_program("python3"),
            vec!["-q"],
            "print('up-' + str(6*7))\n",
            "up-42",
        ),
    ] {
        let Some(program) = program else {
            eprintln!("python3 is not installed; skipped");
            continue;
        };
        let dir = work_dir();
        // The shell starts in a background group and stops itself on its first read of the
        // terminal; the handover 800 ms later must continue it (SIGCONT after tcsetpgrp).
        let pty = Pty::spawn(spec_with_flags(
            program.as_os_str(),
            &args,
            dir.path(),
            &["--test-delay-handover-ms", "800"],
        ))
        .unwrap();
        pty.write(typed.as_bytes()).unwrap();
        wait_for(&pty, expected);
        pty.end();
    }
}

#[test]
fn tcsetpgrp_is_called_once_and_only_while_the_supervisor_is_foreground() {
    let dir = work_dir();
    let trace = dir.path().join("trace.txt");
    let pty = Pty::spawn(spec_with_flags(
        SHELL,
        &["-c", "sleep 30"],
        dir.path(),
        &["--test-trace", trace.to_str().unwrap()],
    ))
    .unwrap();
    wait_until("the handover", || {
        std::fs::read_to_string(&trace).is_ok_and(|text| text.contains("sigcont"))
    });
    sleep(Duration::from_millis(300));
    let lines: Vec<String> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        lines,
        [
            "shell started",
            "handover: foreground=true",
            "tcsetpgrp: ok",
            "sigcont: sent"
        ]
    );
    pty.end();
}

/// Review N10: a supervisor that is still ending its tree when `end` stops waiting for it (9 s)
/// is left alone by the session sweep inside its own cap, not killed as the session's leader
/// (which would hand what it has not ended yet to init). The stand-in lingers 14 s after ending
/// the tree; the sweep would have killed it at about 11 s.
#[test]
fn the_session_sweep_leaves_a_supervisor_that_is_still_ending_alone() {
    let dir = work_dir();
    let pty = Pty::spawn(spec_with_flags(
        SHELL,
        &["-c", "echo READY; sleep 60"],
        dir.path(),
        &["--test-linger-ms", "14000"],
    ))
    .unwrap();
    wait_for(&pty, "READY");
    let started = Instant::now();
    pty.end();
    let exit = pty
        .wait_exit(Duration::from_secs(20))
        .expect("the supervisor exits by itself");
    assert!(
        started.elapsed() >= Duration::from_secs(13),
        "it lingered until it was done ({:?})",
        started.elapsed()
    );
    assert_eq!(
        exit.signal, None,
        "the supervisor exited by itself, it was not killed: {exit:?}"
    );
}

#[test]
fn a_host_without_pidfd_open_fails_closed_with_a_clear_error() {
    let dir = work_dir();
    let pty = Pty::spawn(spec_with_flags(
        SHELL,
        &["-c", "sleep 60"],
        dir.path(),
        &["--test-no-pidfd"],
    ))
    .unwrap();
    let exit = pty.wait_exit(BOUND).expect("the supervisor refuses");
    assert_eq!(i64::from(exit.code), i64::from(EXIT_MISSING_SYSCALL));
    wait_for(&pty, "refuses pidfd_open");
}

/// A thread-group leader that calls `SYS_exit` while another thread lives shows state `Z` in
/// `/proc/<pid>/stat` and yet the process runs on; it must be walked through, signalled and counted
/// as live.
#[test]
fn a_zombie_leader_with_a_live_thread_is_ended() {
    let Some(python) = find_program("python3") else {
        eprintln!("python3 is not installed; skipped");
        return;
    };
    let dir = work_dir();
    let script = dir.path().join("zombie_leader.py");
    let pid_file = dir.path().join("zombie.pid");
    std::fs::write(
        &script,
        "import ctypes, os, platform, sys, threading, time\n\
         with open(sys.argv[1], 'w') as out:\n    out.write(str(os.getpid()))\n\
         threading.Thread(target=time.sleep, args=(120,)).start()\n\
         time.sleep(0.3)\n\
         number = {'x86_64': 60, 'aarch64': 93}[platform.machine()]\n\
         ctypes.CDLL(None).syscall(number, 0)\n",
    )
    .unwrap();
    let pty = Pty::spawn(spec(BASH, &BASH_ARGS, dir.path())).unwrap();
    pty.write(
        format!(
            "{} {} {} &\n",
            python.display(),
            script.display(),
            pid_file.display()
        )
        .as_bytes(),
    )
    .unwrap();
    let leader = wait_for_pid_file(&pid_file);
    // The reproduction: the leader is a zombie while its worker thread lives.
    wait_until("the leader to become a zombie", || {
        stat_field(leader, 0).as_deref() == Some("Z")
    });
    assert!(alive_threads(leader), "the worker thread must be running");
    pty.end();
    wait_until("the process to be gone", || !alive_threads(leader));
}

/// A `vfork` parent waits, in `D` state, for its child to `exec` or exit; if the child stops itself
/// first, the parent never shows `T` and a loop that waits for everything to freeze would wait
/// for ever. Everything must be killed all the same.
#[test]
fn a_vfork_parent_stuck_behind_a_stopped_child_is_ended() {
    let Some(cc) = find_program("cc").or_else(|| find_program("gcc")) else {
        eprintln!("no C compiler is installed; skipped");
        return;
    };
    let dir = work_dir();
    let source = dir.path().join("vfork_stuck.c");
    let helper = dir.path().join("vfork_stuck");
    std::fs::write(
        &source,
        "#include <signal.h>\n#include <stdio.h>\n#include <unistd.h>\n\
         int main(void) {\n\
         \x20   printf(\"VFORK:%d:E\\n\", (int)getpid());\n\
         \x20   fflush(stdout);\n\
         \x20   pid_t child = vfork();\n\
         \x20   if (child == 0) {\n\
         \x20       kill(getpid(), SIGSTOP);\n\
         \x20       _exit(0);\n\
         \x20   }\n\
         \x20   return 0;\n\
         }\n",
    )
    .unwrap();
    let compiled = Command::new(&cc)
        .arg("-w")
        .arg("-o")
        .arg(&helper)
        .arg(&source)
        .status()
        .unwrap();
    assert!(compiled.success(), "the helper did not compile");
    let pty = Pty::spawn(spec(SHELL, &["-c", helper.to_str().unwrap()], dir.path())).unwrap();
    let supervisor = pty.pid().unwrap();
    let parent = wait_for_number(&pty, "VFORK:", ":E");
    // Give the child time to stop itself; the parent then waits on it.
    sleep(Duration::from_millis(500));
    assert!(
        alive_threads(parent),
        "the vfork parent must be stuck, alive"
    );
    let started = Instant::now();
    pty.end();
    assert!(
        started.elapsed() < BOUND,
        "end took {:?}; a stuck descendant held the kill",
        started.elapsed()
    );
    wait_until("the tree to be gone", || {
        !alive_threads(parent) && session_members(supervisor).is_empty()
    });
}
