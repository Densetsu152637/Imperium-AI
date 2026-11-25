//! Test binary: applies the worker's confinement to itself, then tries what a program must not be
//! able to do and prints one `name=outcome` line per attempt. Built only with `test-support`.
//!
//! Arguments: `--file PATH` (a readable file outside the worker), `--port N` (a loopback listener
//! the test holds open), `--pid N` (another process of the same user), `--memory-mb N`.
#![allow(unsafe_code)]

use std::{
    io::Write as _,
    net::TcpStream,
    process::{Command, ExitCode},
};

use imperium_script_worker::{EXIT_CONFINEMENT_FAILED, confine, options::Options};

struct Probe {
    file: String,
    port: u16,
    pid: u32,
    /// A descriptor the test left open, not close-on-exec, for the worker to inherit (-1: none).
    fd: i32,
}

fn report(name: &str, outcome: impl std::fmt::Display) {
    println!("{name}={outcome}");
}

fn denied<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    match result {
        Ok(_) => "allowed".to_owned(),
        Err(err) => format!("denied ({err})"),
    }
}

/// `open` when `fstat` works on the descriptor, `closed` on `EBADF`, `none` without one.
#[cfg(not(unix))]
fn fd_state(_fd: i32) -> String {
    "none".to_owned()
}

#[cfg(unix)]
fn fd_state(fd: i32) -> String {
    if fd < 0 {
        return "none".to_owned();
    }
    // SAFETY: only `fstat`ed for the duration of the call, never closed or owned here; a number
    // that is not open gives `EBADF`.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
    match rustix::fs::fstat(borrowed) {
        Ok(_) => "open".to_owned(),
        Err(err) if err == rustix::io::Errno::BADF => "closed".to_owned(),
        Err(err) => format!("error ({err})"),
    }
}

fn main() -> ExitCode {
    let mut options = Options::parse(std::iter::empty()).expect("defaults parse");
    let mut probe = Probe {
        file: String::new(),
        port: 0,
        pid: 0,
        fd: -1,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().unwrap_or_default();
        match arg.as_str() {
            "--file" => probe.file = value,
            "--port" => probe.port = value.parse().unwrap_or(0),
            "--pid" => probe.pid = value.parse().unwrap_or(0),
            "--fd" => probe.fd = value.parse().unwrap_or(-1),
            "--memory-mb" => options.memory_mb = value.parse().unwrap_or(256),
            "--force-confinement-failure" => options.force_failure = Some(value),
            _ => {
                eprintln!("unknown argument {arg}");
                return ExitCode::from(64);
            }
        }
    }

    report("inherited_fd_before", fd_state(probe.fd));
    match confine::apply(&confine::Settings::from(&options)) {
        Ok(applied) => report("confined", applied.applied.join("; ")),
        Err(err) => {
            eprintln!("confinement failed: {err}");
            return ExitCode::from(EXIT_CONFINEMENT_FAILED);
        }
    }

    report("inherited_fd_after", fd_state(probe.fd));
    report("open_file", denied(std::fs::read(&probe.file)));
    report(
        "connect",
        denied(TcpStream::connect(("127.0.0.1", probe.port))),
    );
    report("spawn", denied(Command::new("/usr/bin/true").status()));
    report(
        "thread",
        denied(std::thread::spawn(|| 1_u32).join().map_err(|_| "panic")),
    );
    platform(&probe);
    let _ = std::io::stdout().flush();
    ExitCode::SUCCESS
}

#[cfg(target_os = "linux")]
fn platform(probe: &Probe) {
    use rustix::process::{Pid, Resource, getrlimit, test_kill_process};

    // Signal 0 to another process of the same user succeeds without confinement.
    let other = Pid::from_raw(i32::try_from(probe.pid).unwrap_or(0));
    report(
        "kill_other",
        other.map_or_else(|| "no pid".to_owned(), |pid| denied(test_kill_process(pid))),
    );

    report(
        "rlimit_data",
        getrlimit(Resource::Data).current.unwrap_or(u64::MAX),
    );

    // SAFETY: raw system calls with arguments that cannot touch memory (a null pointer and zero
    // size for clone3, numbers only otherwise); the point is to see the kernel's answer.
    unsafe {
        let ret = libc::syscall(libc::SYS_clone3, 0_usize, 0_usize);
        report("clone3", format!("ret={ret} errno={}", errno()));

        let mut buf = [0_u8; 8];
        let ret = libc::syscall(libc::SYS_getrandom, buf.as_mut_ptr(), buf.len(), 0_u32);
        report("getrandom", ret);

        #[cfg(target_arch = "x86_64")]
        {
            // x32 ABI: the same call numbers with bit 30 set. getpid is 39.
            let ret = libc::syscall(0x4000_0000_i64 + 39);
            report("x32", format!("ret={ret} errno={}", errno()));
        }
    }
}

#[cfg(target_os = "linux")]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn platform(_probe: &Probe) {
    use std::ffi::{CString, c_char};

    unsafe extern "C" {
        static bootstrap_port: u32;
        fn bootstrap_look_up(bp: u32, service_name: *const c_char, sp: *mut u32) -> i32;
    }

    let service = CString::new("com.apple.pasteboard.1").expect("no NUL");
    let mut port = 0_u32;
    // SAFETY: `service` is NUL-terminated and `port` a valid out-pointer; `bootstrap_port` is the
    // process's bootstrap port, a plain integer read once.
    let status = unsafe { bootstrap_look_up(bootstrap_port, service.as_ptr(), &raw mut port) };
    report(
        "mach_lookup",
        if status == 0 {
            "allowed".to_owned()
        } else {
            format!("denied (status {status})")
        },
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform(_probe: &Probe) {}
