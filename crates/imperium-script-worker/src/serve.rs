//! The protocol loop: framed `monty-proto` requests on stdin, events on stdout.
//!
//! A thin shell around `monty_proto::worker::Worker`, the state machine Monty's own
//! `monty subprocess` wraps (adapted from the `monty-runtime` crate, MIT). The worker is strictly
//! turn-based: one request in, zero or more `Print` events out, then one turn-ending event. A
//! worker that exits without a `FatalError` crashed, and the daemon treats it so. Stdout carries
//! only protocol frames.

use std::{io, panic, process::ExitCode, thread};

use monty_proto::{
    FrameError, FrameReader, pb,
    worker::{
        EventSink, HandleOutcome, SessionBudget, Worker, fatal_error_event, protocol_violation,
    },
    write_frame,
};
use monty_types::memory_limit_with_headroom;

/// BSD `sysexits.h` "remote error in protocol": the frame stream desynchronised.
const EX_PROTOCOL: u8 = 76;

/// Native stack for the worker thread. The interpreter's container `repr` and `==` recurse once
/// per nesting level up to the recursion limit; a fixed size gives every OS the same budget
/// (the memory is reserved, not committed).
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Runs [`serve`] on a thread with [`WORKER_STACK_SIZE`] of stack and returns its exit code. A
/// panic there has already written its `FatalError` frame in the panic hook, so it exits with 101
/// like an unwound main thread.
pub fn run() -> ExitCode {
    thread::Builder::new()
        .name("monty-worker".to_owned())
        .stack_size(WORKER_STACK_SIZE)
        .spawn(serve)
        .map_or_else(
            |err| {
                eprintln!("imperium-script: cannot start the worker thread: {err}");
                ExitCode::from(101)
            },
            |handle| handle.join().unwrap_or(ExitCode::from(101)),
        )
}

/// The loop: one request in, its events out, until EOF, `Shutdown` or a fatal error.
fn serve() -> ExitCode {
    install_panic_hook();
    let mut reader = FrameReader::new(io::stdin().lock());
    let mut worker = Worker::default();
    let mut sink = StdoutSink;
    // Set the memory baseline on the pristine worker: a session's first request is decoded
    // before its ceiling is armed and must not count as overhead.
    apply_budget(worker.session_budget());

    loop {
        match reader.read::<pb::ClientRequest>() {
            Ok(Some(request)) => {
                let outcome = worker.handle(request, &mut sink);
                apply_budget(worker.session_budget());
                match outcome {
                    Ok(HandleOutcome::Continue) => {}
                    Ok(HandleOutcome::Shutdown) => return ExitCode::SUCCESS,
                    // The worker emitted a FatalError (version skew) and cannot keep serving.
                    Ok(HandleOutcome::Fatal) => return ExitCode::from(4),
                    // An oversize event was rejected before any bytes hit the wire, so the
                    // stream is still in sync and the client can read a last gasp.
                    Err(FrameError::FrameTooLarge { len, max }) => {
                        fatal(
                            &worker,
                            &mut sink,
                            &format!(
                                "response frame of {len} bytes exceeds maximum of {max} bytes"
                            ),
                        );
                        return ExitCode::from(EX_PROTOCOL);
                    }
                    // Writing to stdout failed: the client is gone.
                    Err(_) => return ExitCode::from(3),
                }
            }
            // Clean EOF at a frame boundary: the client closed stdin.
            Ok(None) => return ExitCode::SUCCESS,
            // The frame arrived intact but its payload did not decode. The stream is still in
            // sync, so answer with a turn-ending error and keep serving.
            Err(FrameError::Decode(err)) => {
                if sink
                    .send(&protocol_violation(&format!("malformed request: {err}")))
                    .is_err()
                {
                    return ExitCode::from(3);
                }
            }
            Err(err) => {
                // The stream is desynchronised: unrecoverable by design.
                fatal(
                    &worker,
                    &mut sink,
                    &format!("malformed request frame: {err}"),
                );
                return ExitCode::from(EX_PROTOCOL);
            }
        }
    }
}

/// Sets the allocator's hard ceiling for `budget`, at start-up and after every request.
fn apply_budget(budget: SessionBudget) {
    let hard_memory_limit = memory_limit_with_headroom(budget.max_memory, budget.type_check);
    if monty_alloc::set_hard_limit(hard_memory_limit).is_err() {
        // The binary did not install the allocator: refuse to serve with no memory ceiling.
        eprintln!("imperium-script: the limiting allocator is not installed");
        std::process::exit(70);
    }
}

/// Writes framed worker events to stdout.
struct StdoutSink;

impl EventSink for StdoutSink {
    fn send(&mut self, event: &pb::WorkerEvent) -> Result<(), FrameError> {
        write_frame(&mut io::stdout(), event)
    }

    /// A session opened by its first request runs under its ceiling, not the unconfigured one.
    fn session_opened(&mut self, budget: SessionBudget) {
        apply_budget(budget);
    }
}

/// Emits a best-effort `FatalError` event, duplicated to stderr, for an unrecoverable condition
/// the shell detected. The worker exits right after.
fn fatal(worker: &Worker, sink: &mut impl EventSink, message: &str) {
    eprintln!("imperium-script: fatal error: {message}");
    let _ = sink.send(&worker.fatal_event(message));
}

/// Installs a panic hook that emits a best-effort `FatalError` frame before the default unwind,
/// giving the daemon a parseable last gasp for ordinary panics. Hard crashes (stack overflow,
/// allocator abort) bypass it: exiting without a `FatalError` means crashed.
fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = write_frame(
            &mut io::stdout(),
            &fatal_error_event(&format!("worker panicked: {info}")),
        );
        default_hook(info);
    }));
}
