//! The protocol loop: framed `monty-proto` requests on stdin, events on stdout.
//!
//! An Imperium-owned loop over `monty::MontyRun` (plan C1, F12). It replaces `monty-proto`'s
//! `worker` feature, which pulls in the type checker (ruff's `ty`, salsa and `zip`) that a program
//! never uses. It keeps the same framing and message types, but serves only what one program
//! needs, in order:
//!
//! 1. `Configure` (once): limits and the OS policy. Type checking, a first request riding on the
//!    `Configure`, streamed calls and a clock other than the wall clock are refused.
//! 2. `Feed` (once): the program. Inputs, a working folder and async host functions are refused.
//! 3. While the program runs: `ResumeCall` answers a `FunctionCall` or `OsCall`,
//!    `ResumeNameLookup` answers a `NameLookup`, and `AbortFeed` ends the program at a suspension.
//! 4. After the program ends, nothing but `Shutdown` or the end of stdin.
//!
//! `Pong` is accepted at any time and opens no turn. Every other request (`Dump`, `Load`, `Reset`,
//! `ResumeFutures`, `InstallDependencies`, a second `Feed`) is answered with a protocol violation
//! and changes nothing. The events this loop can send are `Ok` (to `Configure` and `Shutdown`),
//! `SessionRefused`, `Print`, `FunctionCall`, `OsCall`, `NameLookup`, `Complete`, `Error` and
//! `FatalError`; never `ResolveFutures`, `StreamedCalls`, `DumpResult`, `TypingError` or
//! `ShutdownDump`, so the daemon's allowlist treats those as a misbehaving worker.
//!
//! A worker that exits without a `FatalError` crashed, and the daemon treats it so. Stdout
//! carries only protocol frames.

use std::{borrow::Cow, io, mem, panic, process::ExitCode, thread};

use monty::{MontyRun, RunProgress};
use monty_proto::{
    FrameError, FrameReader, WireFunctionCall, check_protocol_version, exceeds_max_frame_len,
    ext_result_from_proto, os_call_to_proto, pb, write_frame,
};
use monty_types::{
    CompileOptions, ExcType, ExtFunctionResult, MontyException, NameLookupResult, OsFunctionCall,
    OsPolicy, PrintStream, PrintWriter, PrintWriterCallback, ResourceLimits, ResourceTracker,
    memory_limit_with_headroom,
};

/// BSD `sysexits.h` "remote error in protocol": the frame stream desynchronised.
const EX_PROTOCOL: u8 = 76;

/// Native stack for the worker thread. The interpreter's container `repr` and `==` recurse once
/// per nesting level up to the recursion limit; a fixed size gives every OS the same budget
/// (the memory is reserved, not committed).
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Printed text is sent once this much is buffered, and always before a suspension or the end.
const PRINT_FLUSH_BYTES: usize = 8 * 1024;

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

/// What the loop does after a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Continue,
    Exit(u8),
}

/// The loop: one request in, its events out, until EOF, `Shutdown` or a fatal error.
fn serve() -> ExitCode {
    install_panic_hook();
    // The memory baseline is taken on the pristine worker, before a request is decoded.
    apply_memory_budget(None);
    let mut reader = FrameReader::new(io::stdin().lock());
    let mut session = Session::Fresh;
    loop {
        let next = match reader.read::<pb::ClientRequest>() {
            Ok(Some(request)) => session.handle(request).unwrap_or(Next::Exit(3)),
            // Clean EOF at a frame boundary: the client closed stdin.
            Ok(None) => Next::Exit(0),
            // The frame arrived intact but its payload did not decode. The stream is still in
            // sync, so answer with a turn-ending error and keep serving.
            Err(FrameError::Decode(err)) => {
                match send(&violation(&format!("malformed request: {err}"))) {
                    Ok(()) => Next::Continue,
                    // Writing to stdout failed: the client is gone.
                    Err(_) => Next::Exit(3),
                }
            }
            Err(err) => {
                // The stream is desynchronised: unrecoverable by design.
                fatal(&format!("malformed request frame: {err}"));
                Next::Exit(EX_PROTOCOL)
            }
        };
        if let Next::Exit(code) = next {
            return ExitCode::from(code);
        }
    }
}

/// The settings `Configure` gave.
struct Settings {
    script_name: String,
    limits: ResourceLimits,
    os_policy: OsPolicy,
}

/// A suspended program, waiting for the host.
enum Suspension {
    Call(monty::FunctionCall),
    Os(monty::OsCall),
    Name(monty::NameLookup),
}

/// Where the one program of this worker is.
enum Session {
    /// Waiting for `Configure`.
    Fresh,
    /// Configured, waiting for the program.
    Configured(Box<Settings>),
    /// The program waits for the host's answer.
    Suspended(Box<Suspension>),
    /// The program is over; only `Shutdown` or EOF remain.
    Finished,
}

impl Session {
    /// Handles one request and sends its events. `Err` means stdout is gone.
    fn handle(&mut self, request: pb::ClientRequest) -> Result<Next, FrameError> {
        use pb::client_request::Kind;
        let Some(kind) = request.kind else {
            send(&violation("request has no kind"))?;
            return Ok(Next::Continue);
        };
        let event = match kind {
            Kind::Configure(configure) => {
                if let Err(refusal) = check_protocol_version(configure.protocol_version) {
                    fatal(&refusal);
                    return Ok(Next::Exit(4));
                }
                self.configure(configure)
            }
            Kind::Feed(feed) => self.feed(feed),
            Kind::ResumeCall(resume) => self.resume_call(resume),
            Kind::ResumeNameLookup(resume) => self.resume_name(resume),
            Kind::AbortFeed(abort) => self.abort(abort),
            Kind::Shutdown(_) => {
                send(&event(pb::worker_event::Kind::Ok(pb::Ok {})))?;
                return Ok(Next::Exit(0));
            }
            // A liveness answer; it opens no turn, so it gets no reply.
            Kind::Pong(_) => return Ok(Next::Continue),
            Kind::InstallDependencies(_) => violation("InstallDependencies is not served"),
            Kind::ResumeFutures(_) => violation("ResumeFutures is not served"),
            Kind::Dump(_) => violation("Dump is not served"),
            Kind::Load(_) => violation("Load is not served"),
            Kind::Reset(_) => violation("Reset is not served"),
        };
        send(&event)?;
        Ok(Next::Continue)
    }

    fn configure(&mut self, configure: pb::Configure) -> pb::WorkerEvent {
        if !matches!(self, Session::Fresh) {
            return violation("Configure after the session was configured");
        }
        if configure.type_check {
            return refused("type checking is not served");
        }
        if configure.first.is_some() {
            return refused("a first request inside Configure is not served");
        }
        if configure.accepts_streamed_calls {
            return refused("streamed calls are not served");
        }
        if configure
            .execution_clock
            .is_some_and(|clock| clock != pb::ExecutionClock::Wall as i32)
        {
            return refused("only the wall clock is served");
        }
        let os_policy = match configure.os_policy.map(OsPolicy::try_from) {
            None => OsPolicy::default(),
            Some(Ok(policy)) => policy,
            Some(Err(err)) => return refused(&format!("invalid os_policy: {err}")),
        };
        let limits = ResourceLimits::from(configure.limits.unwrap_or_default());
        // The allocator's ceiling is armed before any program is read.
        apply_memory_budget(limits.max_memory);
        *self = Session::Configured(Box::new(Settings {
            script_name: configure.script_name,
            limits,
            os_policy,
        }));
        event(pb::worker_event::Kind::Ok(pb::Ok {}))
    }

    fn feed(&mut self, feed: pb::Feed) -> pb::WorkerEvent {
        if !matches!(self, Session::Configured(_)) {
            return violation("Feed without a configured session that has not run a program");
        }
        if !feed.inputs.is_empty() || feed.values.is_some() {
            return violation("program inputs are not served");
        }
        if !feed.cwd.is_empty() {
            return violation("a working folder is not served");
        }
        if feed.stream_async_calls || !feed.async_functions.is_empty() {
            return violation("async host functions are not served");
        }
        let Session::Configured(settings) = mem::replace(self, Session::Finished) else {
            unreachable!("checked above");
        };
        let Settings {
            script_name,
            limits,
            os_policy,
        } = *settings;
        let mut print = Printer::default();
        let progress = match MontyRun::new(
            feed.code,
            &script_name,
            Vec::new(),
            CompileOptions::default(),
        ) {
            Ok(program) => program.with_os_policy(os_policy).start(
                Vec::new(),
                ResourceTracker::new(limits),
                PrintWriter::Callback(&mut print),
            ),
            Err(exc) => Err(exc),
        };
        self.drive(progress, &mut print)
    }

    fn resume_call(&mut self, resume: pb::ResumeCall) -> pb::WorkerEvent {
        let call_id = match self {
            Session::Suspended(suspension) => match suspension.as_ref() {
                Suspension::Call(call) => call.call_id,
                Suspension::Os(call) => call.call_id,
                Suspension::Name(_) => return violation("ResumeCall while a name lookup waits"),
            },
            _ => return violation("ResumeCall without a suspended call"),
        };
        if resume.call_id != call_id {
            return violation(&format!(
                "ResumeCall call_id {} does not match {call_id}",
                resume.call_id
            ));
        }
        let Some(result) = resume.result else {
            return violation("ResumeCall has no result");
        };
        if matches!(
            result.kind,
            Some(pb::ext_function_result::Kind::NotHandled(_))
        ) {
            return violation("NotHandled is not served");
        }
        let result = match ext_result_from_proto(result, resume.values) {
            Ok(
                result @ (ExtFunctionResult::Return(_)
                | ExtFunctionResult::Error(_)
                | ExtFunctionResult::NotFound(_)),
            ) => result,
            Ok(ExtFunctionResult::Future(_)) => return violation("futures are not served"),
            Ok(ExtFunctionResult::ModuleFile(_)) => {
                return violation("module files are not served");
            }
            Err(err) => return violation(&format!("invalid result: {err}")),
        };
        let Session::Suspended(suspension) = mem::replace(self, Session::Finished) else {
            unreachable!("checked above");
        };
        let mut print = Printer::default();
        let progress = match *suspension {
            Suspension::Call(call) => call.resume(result, PrintWriter::Callback(&mut print)),
            Suspension::Os(call) => call.resume(result, PrintWriter::Callback(&mut print)),
            Suspension::Name(_) => unreachable!("checked above"),
        };
        self.drive(progress, &mut print)
    }

    fn resume_name(&mut self, resume: pb::ResumeNameLookup) -> pb::WorkerEvent {
        let waiting = match self {
            Session::Suspended(suspension) => matches!(suspension.as_ref(), Suspension::Name(_)),
            _ => false,
        };
        if !waiting {
            return violation("ResumeNameLookup without a suspended name lookup");
        }
        let result = match NameLookupResult::try_from(resume) {
            Ok(result) => result,
            Err(err) => return violation(&format!("invalid result: {err}")),
        };
        let Session::Suspended(suspension) = mem::replace(self, Session::Finished) else {
            unreachable!("checked above");
        };
        let Suspension::Name(lookup) = *suspension else {
            unreachable!("checked above");
        };
        let mut print = Printer::default();
        let progress = lookup.resume(result, PrintWriter::Callback(&mut print));
        self.drive(progress, &mut print)
    }

    /// Ends the program at its suspension with the host's exception, which it cannot catch.
    fn abort(&mut self, abort: pb::AbortFeed) -> pb::WorkerEvent {
        if !matches!(self, Session::Suspended(_)) {
            return violation("AbortFeed without a suspended program");
        }
        let Some(exception) = abort.exception else {
            return violation("AbortFeed has no exception");
        };
        let exc = match MontyException::try_from(exception) {
            Ok(exc) => exc,
            Err(err) => return violation(&format!("invalid exception: {err}")),
        };
        let Session::Suspended(suspension) = mem::replace(self, Session::Finished) else {
            unreachable!("checked above");
        };
        let mut print = Printer::default();
        let progress = match *suspension {
            Suspension::Call(call) => call.abort(exc, PrintWriter::Callback(&mut print)),
            Suspension::Os(call) => call.abort(exc, PrintWriter::Callback(&mut print)),
            Suspension::Name(lookup) => lookup.abort(exc, PrintWriter::Callback(&mut print)),
        };
        self.drive(progress, &mut print)
    }

    /// Runs the program to its next turn-ending event. The state is `Finished` on entry; it
    /// becomes `Suspended` only when the event announces a suspension.
    fn drive(
        &mut self,
        mut progress: Result<RunProgress, MontyException>,
        print: &mut Printer,
    ) -> pb::WorkerEvent {
        let announced = loop {
            match progress {
                Ok(RunProgress::Complete(value)) => {
                    break event(pb::worker_event::Kind::Complete(value.into()));
                }
                Ok(RunProgress::FunctionCall(mut call)) => {
                    // The arguments travel in the event; the suspension does not need them to
                    // resume, and nothing here dumps it.
                    let announced =
                        event(pb::worker_event::Kind::FunctionCall(WireFunctionCall::new(
                            call.function_name.clone(),
                            mem::take(&mut call.args),
                            call.call_id,
                            call.object_id,
                            false,
                            call.position.clone(),
                        )));
                    if let Some(len) = exceeds_max_frame_len(&announced) {
                        progress = call.abort(
                            runtime_error(&format!(
                                "call arguments of {len} bytes are larger than a request may be"
                            )),
                            PrintWriter::Callback(&mut *print),
                        );
                        continue;
                    }
                    *self = Session::Suspended(Box::new(Suspension::Call(call)));
                    break announced;
                }
                Ok(RunProgress::OsCall(mut call)) => {
                    let function =
                        mem::replace(&mut call.function_call, OsFunctionCall::GetEnviron);
                    let announced = event(pb::worker_event::Kind::OsCall(os_call_to_proto(
                        call.call_id,
                        function,
                        false,
                        &call.position,
                    )));
                    if let Some(len) = exceeds_max_frame_len(&announced) {
                        progress = call.abort(
                            runtime_error(&format!(
                                "an OS request of {len} bytes is larger than a request may be"
                            )),
                            PrintWriter::Callback(&mut *print),
                        );
                        continue;
                    }
                    *self = Session::Suspended(Box::new(Suspension::Os(call)));
                    break announced;
                }
                Ok(RunProgress::NameLookup(lookup)) => {
                    // The program is never given a host object, so an attribute lookup on one
                    // cannot happen; answer it here as undefined rather than ask the host.
                    if lookup.object_id().is_some() {
                        progress = lookup.resume(
                            NameLookupResult::Undefined,
                            PrintWriter::Callback(&mut *print),
                        );
                        continue;
                    }
                    let announced = event(pb::worker_event::Kind::NameLookup(pb::NameLookup {
                        name: lookup.name.clone(),
                        object_id: None,
                        position: Some((&lookup.position).into()),
                    }));
                    *self = Session::Suspended(Box::new(Suspension::Name(lookup)));
                    break announced;
                }
                // No host call ever returns a future, so nothing can wait on one; should the
                // interpreter ask anyway, the program ends here instead of asking the host.
                Ok(RunProgress::ResolveFutures(pending)) => {
                    progress = pending.abort(
                        runtime_error("async host calls are not available in programs"),
                        PrintWriter::Callback(&mut *print),
                    );
                }
                Err(exc) => {
                    break event(pb::worker_event::Kind::Error(pb::Error {
                        exception: Some((&exc).into()),
                    }));
                }
            }
        };
        // Output printed before the event goes first.
        if let Err(exc) = print.flush() {
            eprintln!("imperium-script: {exc}");
        }
        announced
    }
}

/// Collects printed text and sends it as `Print` events: in chunks while the program prints, and
/// in full before any suspension or the end.
#[derive(Default)]
struct Printer {
    segments: Vec<pb::PrintSegment>,
    bytes: usize,
}

impl Printer {
    fn append(&mut self, stream: PrintStream, text: &str) -> Result<(), MontyException> {
        let stream = match stream {
            PrintStream::Stdout => pb::PrintStream::Stdout as i32,
            PrintStream::Stderr => pb::PrintStream::Stderr as i32,
        };
        match self.segments.last_mut() {
            Some(segment) if segment.stream == stream => segment.text.push_str(text),
            _ => self.segments.push(pb::PrintSegment {
                stream,
                text: text.to_owned(),
            }),
        }
        self.bytes += text.len();
        if self.bytes >= PRINT_FLUSH_BYTES {
            self.flush()
        } else {
            Ok(())
        }
    }

    fn flush(&mut self) -> Result<(), MontyException> {
        if self.segments.is_empty() {
            return Ok(());
        }
        self.bytes = 0;
        let segments = mem::take(&mut self.segments);
        send(&event(pb::worker_event::Kind::Print(pb::Print {
            segments: segments.into(),
        })))
        .map_err(|err| runtime_error(&format!("failed to send printed output: {err}")))
    }
}

impl PrintWriterCallback for Printer {
    fn stdout_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.append(PrintStream::Stdout, &output)
    }

    fn stdout_push(&mut self, end: char) -> Result<(), MontyException> {
        self.append(PrintStream::Stdout, end.encode_utf8(&mut [0; 4]))
    }

    fn stderr_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.append(PrintStream::Stderr, &output)
    }

    fn stderr_push(&mut self, end: char) -> Result<(), MontyException> {
        self.append(PrintStream::Stderr, end.encode_utf8(&mut [0; 4]))
    }
}

fn event(kind: pb::worker_event::Kind) -> pb::WorkerEvent {
    pb::WorkerEvent {
        kind: Some(kind),
        ..Default::default()
    }
}

fn runtime_error(message: &str) -> MontyException {
    MontyException::new(ExcType::RuntimeError, Some(message.to_owned()))
}

/// A recoverable protocol violation: a turn-ending error that changes nothing.
fn violation(message: &str) -> pb::WorkerEvent {
    event(pb::worker_event::Kind::Error(pb::Error {
        exception: Some((&runtime_error(&format!("protocol violation: {message}"))).into()),
    }))
}

/// A `Configure` this worker does not serve: no session exists afterwards.
fn refused(message: &str) -> pb::WorkerEvent {
    event(pb::worker_event::Kind::SessionRefused(pb::SessionRefused {
        message: message.to_owned(),
    }))
}

/// Frames one event on stdout (`write_frame` flushes it).
fn send(event: &pb::WorkerEvent) -> Result<(), FrameError> {
    write_frame(&mut io::stdout().lock(), event)
}

/// Sets the allocator's hard ceiling: none at start (which only takes the baseline), then the
/// program's `max_memory` once `Configure` gives it.
fn apply_memory_budget(max_memory: Option<usize>) {
    if monty_alloc::set_hard_limit(memory_limit_with_headroom(max_memory, false)).is_err() {
        // The binary did not install the allocator: refuse to serve with no memory ceiling.
        eprintln!("imperium-script: the limiting allocator is not installed");
        std::process::exit(70);
    }
}

/// Emits a best-effort `FatalError` event, duplicated to stderr, for an unrecoverable
/// condition. The worker exits right after.
fn fatal(message: &str) {
    eprintln!("imperium-script: fatal error: {message}");
    let _ = send(&event(pb::worker_event::Kind::FatalError(pb::FatalError {
        message: message.to_owned(),
    })));
}

/// Installs a panic hook that emits a best-effort `FatalError` frame before the default unwind,
/// giving the daemon a parseable last gasp for ordinary panics. Hard crashes (stack overflow,
/// allocator abort) bypass it: exiting without a `FatalError` means crashed.
fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = send(&event(pb::worker_event::Kind::FatalError(pb::FatalError {
            message: format!("worker panicked: {info}"),
        })));
        default_hook(info);
    }));
}
