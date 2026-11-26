//! Test binary for Windows: launched through the daemon's confined launch, it checks the
//! confinement the worker checks, then tries what a program must not be able to do and prints
//! one `name=outcome` line per attempt. Built only with `test-support`.
//!
//! Arguments (each optional, `--read` repeatable): `--write PATH` (a file to create in the user's
//! profile), `--read PATH` (a file to read), `--pipe NAME` (the daemon's named pipe),
//! `--handle N --handle-len N` (a handle the test made inheritable before the launch, and the
//! length of its file), `--cred TARGET` (a Credential Manager entry), `--port N` (a loopback
//! listener the test holds open).
#![cfg_attr(windows, allow(unsafe_code))]

#[cfg(not(windows))]
fn main() {
    eprintln!("windows-probe runs on Windows only");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use std::io::Write as _;

    let mut write = None;
    let mut reads = Vec::new();
    let mut pipe = None;
    let mut handle = None;
    let mut handle_len = 0_u64;
    let mut cred = None;
    let mut port = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().unwrap_or_default();
        match arg.as_str() {
            "--write" => write = Some(value),
            "--read" => reads.push(value),
            "--pipe" => pipe = Some(value),
            "--handle" => handle = value.parse::<usize>().ok(),
            "--handle-len" => handle_len = value.parse().unwrap_or(0),
            "--cred" => cred = Some(value),
            "--port" => port = value.parse::<u16>().ok(),
            "--noop" => return std::process::ExitCode::SUCCESS,
            _ => {
                eprintln!("unknown argument {arg}");
                return std::process::ExitCode::from(64);
            }
        }
    }

    let options = imperium_script_worker::options::Options::parse(std::iter::empty())
        .expect("defaults parse");
    report(
        "confined",
        match imperium_script_worker::confine::apply(
            &imperium_script_worker::confine::Settings::from(&options),
        ) {
            Ok(applied) => applied.applied.join("; "),
            Err(err) => format!("failed ({err})"),
        },
    );
    report("env_count", std::env::vars_os().count());
    let cwd = std::env::current_dir().map(|dir| dir.display().to_string());
    report("cwd", cwd.as_deref().unwrap_or("unknown"));
    report(
        "cwd_empty",
        std::fs::read_dir(".").is_ok_and(|mut entries| entries.next().is_none()),
    );
    if let Some(path) = write {
        report(
            "write_profile",
            denied(std::fs::write(&path, b"from a Low process")),
        );
    }
    for (index, path) in reads.iter().enumerate() {
        report(&format!("read_{index}"), denied(std::fs::read(path)));
    }
    if let Some(name) = pipe {
        report(
            "pipe",
            denied(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(name),
            ),
        );
    }
    // A second process: the job allows one.
    let me = std::env::current_exe().expect("the probe knows its path");
    report(
        "spawn",
        denied(std::process::Command::new(me).arg("--noop").status()),
    );
    if let Some(raw) = handle {
        report("handle", handle_state(raw, handle_len));
    }
    if let Some(target) = cred {
        report("cred_read", cred_read(&target));
    }
    if let Some(port) = port {
        report(
            "connect",
            denied(std::net::TcpStream::connect(("127.0.0.1", port))),
        );
    }
    let _ = std::io::stdout().flush();
    std::process::ExitCode::SUCCESS
}

#[cfg(windows)]
fn report(name: &str, outcome: impl std::fmt::Display) {
    println!("{name}={outcome}");
}

#[cfg(windows)]
fn denied<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    match result {
        Ok(_) => "allowed".to_owned(),
        Err(err) => format!("denied ({err})"),
    }
}

/// `open` when the value is a handle to a file of the expected length, `closed` otherwise.
#[cfg(windows)]
fn handle_state(raw: usize, expected_len: u64) -> &'static str {
    use std::os::windows::io::FromRawHandle as _;
    // SAFETY: the value is only borrowed through a `File` that is never dropped (so never
    // closed); a value that is not an open handle makes the query fail.
    let file = std::mem::ManuallyDrop::new(unsafe {
        std::fs::File::from_raw_handle(raw as std::os::windows::io::RawHandle)
    });
    match file.metadata() {
        Ok(meta) if meta.len() == expected_len => "open",
        _ => "closed",
    }
}

/// Whether a Low process can read a Credential Manager entry.
#[cfg(windows)]
fn cred_read(target: &str) -> String {
    use windows_sys::Win32::Security::Credentials::{
        CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
    };
    let wide: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated; `credential` is a valid out-pointer, freed below.
    let read = unsafe { CredReadW(wide.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut credential) };
    if read == 0 {
        return format!("denied ({})", std::io::Error::last_os_error());
    }
    // SAFETY: allocated by CredReadW, freed once.
    unsafe { CredFree(credential.cast()) };
    "allowed".to_owned()
}
