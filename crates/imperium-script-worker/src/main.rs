//! `imperium-script`: the worker binary. Confines itself, then serves the `monty-proto` loop.

use std::process::ExitCode;

use imperium_script_worker::{
    EXIT_CONFINEMENT_FAILED, EXIT_USAGE, confine, options::Options, serve,
};

/// Bounds worker memory and ends the process with a dedicated exit code when the ceiling is
/// crossed. Only a binary may declare the allocator.
#[global_allocator]
static ALLOC: monty_alloc::LimitedAllocator = monty_alloc::LimitedAllocator;

fn main() -> ExitCode {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("imperium-script: {err}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    // Before the first read from stdin, and on the main thread, so the filter is in place before
    // any other thread exists.
    if let Err(err) = confine::apply(&confine::Settings::from(&options)) {
        eprintln!("imperium-script: confinement failed: {err}");
        return ExitCode::from(EXIT_CONFINEMENT_FAILED);
    }
    serve::run()
}
