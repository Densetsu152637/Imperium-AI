//! A stand-alone supervisor for this crate's tests (the daemon runs the same code through its
//! `pty-supervisor` subcommand). It is built beside the library and run only by the tests.

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(imperium_pty::supervisor::run(std::env::args_os().skip(1)));
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("the supervisor runs on Linux only");
    std::process::exit(2);
}
