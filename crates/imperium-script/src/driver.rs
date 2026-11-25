//! The protocol driver: launch, event allowlist, suspension budget, kill-on-end.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use monty_proto::{
    PROTOCOL_VERSION, decode_frame_with_budget, encode_framed_into, ext_result_to_proto, pb,
};
use monty_types::{
    ExcType, ExtFunctionResult, MontyException, MontyObject, NameLookupResult, OOM_EXIT_CODE,
    OsPolicy, ResourceLimits, SleepMode,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::timeout,
};

/// The largest event frame the driver accepts. A program's printed output and call arguments are
/// far below it; anything bigger is a misbehaving worker.
const MAX_EVENT_BYTES: u32 = 16 * 1024 * 1024;

/// Allocation ceiling for decoding one event.
const DECODE_BUDGET: usize = 64 * 1024 * 1024;

/// `Ping`s answered in one turn before the worker counts as misbehaving.
const MAX_PINGS: u32 = 64;

/// The most stderr text kept for a failure reason.
const STDERR_KEEP: usize = 4096;

/// Per-program limits (plan C5). The interpreter's own limits travel to the worker in
/// `Configure`; the driver enforces the rest.
#[derive(Debug, Clone)]
pub struct Limits {
    /// `script_memory_mb`: the interpreter's memory limit; the OS backstop is twice this.
    pub memory_mb: u64,
    /// `script_exec_secs`: interpreter time per program (paused while the host answers).
    pub exec: Duration,
    /// `script_suspensions`: suspensions of any kind (calls, name lookups, OS calls) allowed.
    pub suspensions: usize,
    /// Added to `exec` for the fixed `request_timeout` (5 s in the plan).
    pub request_slack: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_mb: 256,
            exec: Duration::from_secs(10),
            suspensions: 200,
            request_slack: Duration::from_secs(5),
        }
    }
}

impl Limits {
    /// How long the worker may take from a request to its next event.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        self.exec.saturating_add(self.request_slack)
    }
}

/// Why a program or its worker did not finish normally.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// The worker could not be started or confined; no program ran.
    #[error("code mode is unavailable: {0}")]
    Unavailable(String),
    /// The program raised (including its time and memory budgets as the interpreter reports them).
    #[error("{rendered}")]
    Program {
        /// The exception type name, such as `NameError`.
        exc_type: String,
        /// The exception message.
        message: String,
        /// The Python traceback text.
        rendered: String,
    },
    /// The worker's memory ceiling ended it.
    #[error("the program ran out of memory")]
    OutOfMemory,
    /// The program made more requests than `script_suspensions`.
    #[error("the program made too many requests")]
    TooManyRequests,
    /// The worker sent an event outside the allowlist or an undecodable frame; it was killed.
    #[error("the interpreter misbehaved: {0}")]
    Misbehaved(String),
    /// The worker exited without finishing.
    #[error("the program's interpreter stopped unexpectedly: {0}")]
    Crashed(String),
    /// The program used up its wall-clock budget while the driver waited for the worker; it was
    /// killed.
    #[error("the program ran out of time")]
    WallBudget,
    /// The worker did not answer within `request_timeout`; it was killed.
    #[error("the program's interpreter did not answer in time")]
    Unresponsive,
    /// The caller used the run wrongly (answering nothing, answering twice).
    #[error("{0}")]
    Misuse(String),
}

/// A call from the program to a host function.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// The function name the program called, as the driver issued it in a name lookup.
    pub function: String,
    /// Positional arguments.
    pub args: Vec<MontyObject>,
    /// Keyword arguments as `(key, value)`; keys are usually strings.
    pub kwargs: Vec<(MontyObject, MontyObject)>,
}

/// What a running program does next.
#[derive(Debug)]
pub enum ScriptEvent {
    /// Printed text (stdout and stderr, in order).
    Print(String),
    /// The program read a name it has not defined; answer with `resume_name`.
    NameLookup(String),
    /// The program called a host function; answer with `resume_value` or `resume_error`.
    Call(Call),
    /// The program ended and its worker is gone.
    Done(Result<(), ScriptError>),
}

/// Starts workers. Cheap to clone; clones share the worker slots.
#[derive(Debug, Clone)]
pub struct Launcher {
    worker: PathBuf,
    extra_args: Vec<OsString>,
    slots: Arc<Semaphore>,
}

impl Launcher {
    /// A launcher for the worker binary at `worker`, allowing `max_workers` at once
    /// (`script_workers`).
    pub fn new(worker: impl Into<PathBuf>, max_workers: usize) -> Self {
        Self {
            worker: worker.into(),
            extra_args: Vec::new(),
            slots: Arc::new(Semaphore::new(max_workers)),
        }
    }

    /// Extra worker arguments (test hooks).
    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.extra_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// The worker binary this launcher starts.
    #[must_use]
    pub fn worker(&self) -> &Path {
        &self.worker
    }

    /// Worker slots free right now.
    #[must_use]
    pub fn free_slots(&self) -> usize {
        self.slots.available_permits()
    }

    /// Starts a confined worker, configures it and feeds it `code`. Waits for a free slot first.
    /// Fails with [`ScriptError::Unavailable`], naming why, when the worker cannot confine itself.
    pub async fn start(&self, code: &str, limits: &Limits) -> Result<ScriptRun, ScriptError> {
        let permit = Arc::clone(&self.slots)
            .acquire_owned()
            .await
            .map_err(|_| ScriptError::Unavailable("the worker slots were closed".to_owned()))?;
        self.launch(code, limits, permit).await
    }

    #[cfg(unix)]
    async fn launch(
        &self,
        code: &str,
        limits: &Limits,
        permit: OwnedSemaphorePermit,
    ) -> Result<ScriptRun, ScriptError> {
        let started = Instant::now();
        let scratch = ScratchDir::create().map_err(|err| {
            ScriptError::Unavailable(format!("cannot make the worker's working folder: {err}"))
        })?;
        let mut command = Command::new(&self.worker);
        command
            .current_dir(&scratch.0)
            .arg("--memory-mb")
            .arg(limits.memory_mb.to_string())
            .args(&self.extra_args)
            // An empty environment: no tokens, no paths.
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|err| {
            ScriptError::Unavailable(format!(
                "cannot start the worker {}: {err}",
                self.worker.display()
            ))
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.start_kill();
            return Err(ScriptError::Unavailable(
                "the worker's pipes are missing".to_owned(),
            ));
        };
        let stderr_text = Arc::new(Mutex::new(String::new()));
        let stderr_task = tokio::spawn(collect_stderr(stderr, Arc::clone(&stderr_text)));

        let mut run = ScriptRun {
            child,
            stdin,
            stdout,
            stderr_text,
            stderr_task: Some(stderr_task),
            limits: limits.clone(),
            request_timeout: limits.request_timeout(),
            send_buf: Vec::new(),
            pending: None,
            call_name: String::new(),
            suspensions: 0,
            finished: false,
            startup: Duration::ZERO,
            _permit: permit,
            _scratch: scratch,
        };
        run.open(code, started).await?;
        Ok(run)
    }

    #[cfg(not(unix))]
    async fn launch(
        &self,
        _code: &str,
        _limits: &Limits,
        _permit: OwnedSemaphorePermit,
    ) -> Result<ScriptRun, ScriptError> {
        Err(ScriptError::Unavailable(
            "the Windows launch (restricted token, job object, labels) is stage 1b of the code-mode plan".to_owned(),
        ))
    }
}

async fn collect_stderr(mut stderr: tokio::process::ChildStderr, into: Arc<Mutex<String>>) {
    let mut buf = [0_u8; 512];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if let Ok(mut text) = into.lock()
                    && text.len() < STDERR_KEEP
                {
                    text.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Pending {
    Call(u32),
    Name,
}

enum ReadFailure {
    Timeout,
    /// The caller's deadline passed first.
    Deadline,
    Eof,
    Io,
    Frame(String),
}

/// One program in one worker. Dropping it kills the worker (`kill_on_drop` sends the signal and
/// does not wait); [`ScriptRun::finish`] and the program's end wait for the OS to confirm the exit.
#[derive(Debug)]
pub struct ScriptRun {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr_text: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    limits: Limits,
    request_timeout: Duration,
    send_buf: Vec<u8>,
    pending: Option<Pending>,
    /// The function name of the call waiting for an answer.
    call_name: String,
    suspensions: usize,
    finished: bool,
    startup: Duration,
    _permit: OwnedSemaphorePermit,
    /// The worker's working folder, empty, removed when the run is dropped.
    #[cfg(unix)]
    _scratch: ScratchDir,
}

/// An empty folder made for one worker to start in (plan C1: an empty working folder).
#[cfg(unix)]
#[derive(Debug)]
struct ScratchDir(PathBuf);

#[cfg(unix)]
impl ScratchDir {
    fn create() -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "imperium-script-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
}

#[cfg(unix)]
impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

impl ScriptRun {
    /// The worker's process id while it runs.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Time from spawning the worker to its answer to `Configure`: the cold start of plan A4.
    #[must_use]
    pub fn startup(&self) -> Duration {
        self.startup
    }

    /// Suspensions answered so far (calls, name lookups and OS calls).
    #[must_use]
    pub fn suspensions(&self) -> usize {
        self.suspensions
    }

    /// Kills the worker and waits for the OS to confirm it is gone.
    pub async fn finish(mut self) {
        self.kill_and_wait().await;
    }

    async fn kill_and_wait(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        self.finished = true;
    }

    async fn stderr_snapshot(&mut self) -> String {
        if let Some(task) = self.stderr_task.take() {
            let _ = timeout(Duration::from_secs(1), task).await;
        }
        self.stderr_text
            .lock()
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    }

    /// `Configure`, wait for `Ok`, then `Feed`.
    async fn open(&mut self, code: &str, started: Instant) -> Result<(), ScriptError> {
        let limits = ResourceLimits::default()
            .max_memory(
                usize::try_from(self.limits.memory_mb.saturating_mul(1024 * 1024))
                    .unwrap_or(usize::MAX),
            )
            .max_feed_duration(self.limits.exec)
            .max_suspensions(self.limits.suspensions)
            // Sleeping does nothing in a program, so the budget for it is nothing.
            .max_total_sleep(Duration::ZERO);
        // Keep the worker-side clock and entropy (datetime.now() and random work without
        // suspending); time.sleep() returns at once.
        let os_policy = OsPolicy {
            sleep: SleepMode::Zero,
            ..OsPolicy::default()
        };
        let configure = pb::Configure {
            script_name: "program.py".to_owned(),
            limits: Some((&limits).into()),
            os_policy: Some((&os_policy).into()),
            protocol_version: PROTOCOL_VERSION,
            monty_version: "imperium-script".to_owned(),
            ..Default::default()
        };
        let reason = match self
            .send(pb::client_request::Kind::Configure(configure))
            .await
        {
            Err(err) => Some(format!(
                "the worker did not accept its configuration: {err}"
            )),
            Ok(()) => match self.read_event(None).await {
                Ok(event) if matches!(event.kind, Some(pb::worker_event::Kind::Ok(_))) => None,
                Ok(_) => {
                    Some("the worker answered its configuration with something else".to_owned())
                }
                Err(ReadFailure::Timeout | ReadFailure::Deadline) => {
                    Some("the worker did not answer its configuration".to_owned())
                }
                Err(ReadFailure::Eof | ReadFailure::Io | ReadFailure::Frame(_)) => {
                    Some("the worker exited before it was configured".to_owned())
                }
            },
        };
        if let Some(reason) = reason {
            self.kill_and_wait().await;
            let stderr = self.stderr_snapshot().await;
            let reason = if stderr.is_empty() {
                reason
            } else {
                format!("{reason}: {stderr}")
            };
            return Err(ScriptError::Unavailable(reason));
        }
        self.startup = started.elapsed();

        let feed = pb::Feed {
            code: code.to_owned(),
            skip_type_check: true,
            ..Default::default()
        };
        if let Err(err) = self.send(pb::client_request::Kind::Feed(feed)).await {
            self.kill_and_wait().await;
            return Err(ScriptError::Crashed(format!(
                "could not send the program: {err}"
            )));
        }
        Ok(())
    }

    /// The program's next event. After `NameLookup` or `Call`, answer before calling again.
    /// Each wait for the worker is bounded by `request_timeout` but the call as a whole is not
    /// (it answers `OsCall`s and pings inside), so the engine should use
    /// [`next_until`](Self::next_until) with the program's wall deadline.
    pub async fn next(&mut self) -> ScriptEvent {
        self.next_until(None).await
    }

    /// As [`next`](Self::next), but the program ends with [`ScriptError::WallBudget`] and its
    /// worker is killed when `deadline` passes before an event the host must handle arrives. The
    /// engine passes the program's wall-clock deadline (plan C5); time the host spends answering
    /// calls between `next` calls is its own to account for.
    pub async fn next_until(&mut self, deadline: Option<Instant>) -> ScriptEvent {
        if self.finished {
            return ScriptEvent::Done(Err(ScriptError::Misuse(
                "the program has already ended".to_owned(),
            )));
        }
        if self.pending.is_some() {
            return self
                .end(Err(ScriptError::Misuse(
                    "the previous suspension was not answered".to_owned(),
                )))
                .await;
        }
        let mut pings = 0_u32;
        loop {
            let event = match self.read_event(deadline).await {
                Ok(event) => event,
                Err(failure) => {
                    let error = self.failure(failure).await;
                    return self.end(Err(error)).await;
                }
            };
            use pb::worker_event::Kind;
            let Some(kind) = event.kind else {
                return self.violation("an event of no known kind").await;
            };
            match kind {
                Kind::Print(print) => {
                    let text: String = print
                        .segments
                        .iter()
                        .map(|segment| segment.text.as_str())
                        .collect();
                    return ScriptEvent::Print(text);
                }
                Kind::FunctionCall(call) => {
                    if self.count_suspension() {
                        return self.abort_over_budget().await;
                    }
                    if call.object_id.is_some() {
                        return self
                            .violation("a call on a host object, which the program was never given")
                            .await;
                    }
                    let call_id = call.call_id;
                    let function = call.function_name.clone();
                    self.call_name.clone_from(&function);
                    let args = match call.into_call_args() {
                        Ok(args) => args,
                        Err(err) => {
                            return self
                                .violation(&format!("undecodable call arguments: {err}"))
                                .await;
                        }
                    };
                    let call = Call {
                        function,
                        args: args.args().map(|value| value.to_owned()).collect(),
                        kwargs: args
                            .kwargs()
                            .map(|(key, value)| (key.to_owned(), value.to_owned()))
                            .collect(),
                    };
                    self.pending = Some(Pending::Call(call_id));
                    return ScriptEvent::Call(call);
                }
                Kind::NameLookup(lookup) => {
                    if self.count_suspension() {
                        return self.abort_over_budget().await;
                    }
                    self.pending = Some(Pending::Name);
                    return ScriptEvent::NameLookup(lookup.name);
                }
                Kind::OsCall(os) => {
                    if self.count_suspension() {
                        return self.abort_over_budget().await;
                    }
                    // Always an error, never a value: a program has no files, environment or
                    // clock beyond the worker's own.
                    let exception = MontyException::new(
                        ExcType::OSError,
                        Some("not available in programs".to_owned()),
                    );
                    let (result, values) = ext_result_to_proto(ExtFunctionResult::Error(exception));
                    let resume = pb::ResumeCall {
                        call_id: os.call_id,
                        result: Some(result),
                        values,
                    };
                    if let Err(err) = self
                        .send(pb::client_request::Kind::ResumeCall(resume))
                        .await
                    {
                        let error = ScriptError::Crashed(format!(
                            "could not answer the interpreter: {err}"
                        ));
                        return self.end(Err(error)).await;
                    }
                }
                Kind::Ping(_) => {
                    pings += 1;
                    if pings > MAX_PINGS {
                        return self.violation("a flood of pings").await;
                    }
                    if let Err(err) = self.send(pb::client_request::Kind::Pong(pb::Pong {})).await {
                        let error = ScriptError::Crashed(format!(
                            "could not answer the interpreter: {err}"
                        ));
                        return self.end(Err(error)).await;
                    }
                }
                Kind::Complete(_) => return self.end(Ok(())).await,
                Kind::Error(error) => {
                    let result = match error.exception.map(MontyException::try_from) {
                        Some(Ok(exception)) => Err(ScriptError::Program {
                            exc_type: exception.exc_type().to_string(),
                            message: exception.message().unwrap_or_default().to_owned(),
                            rendered: exception.to_string(),
                        }),
                        Some(Err(err)) => Err(ScriptError::Misbehaved(format!(
                            "an undecodable exception: {err}"
                        ))),
                        None => Err(ScriptError::Misbehaved(
                            "an error event without an exception".to_owned(),
                        )),
                    };
                    return self.end(result).await;
                }
                Kind::FatalError(fatal) => {
                    return self.end(Err(ScriptError::Crashed(fatal.message))).await;
                }
                Kind::SessionRefused(refused) => {
                    return self
                        .end(Err(ScriptError::Unavailable(refused.message)))
                        .await;
                }
                Kind::Ok(_) => return self.violation("an acknowledgement outside Configure").await,
                Kind::ResolveFutures(_) => return self.violation("ResolveFutures").await,
                Kind::StreamedCalls(_) => return self.violation("StreamedCalls").await,
                Kind::DumpResult(_) => return self.violation("DumpResult").await,
                Kind::TypingError(_) => return self.violation("TypingError").await,
                Kind::Shutdown(_) => return self.violation("ShutdownDump").await,
            }
        }
    }

    /// Answers a call with a value.
    pub async fn resume_value(&mut self, value: MontyObject) -> Result<(), ScriptError> {
        let Some(Pending::Call(call_id)) = self.pending else {
            return Err(ScriptError::Misuse(
                "no call is waiting for an answer".to_owned(),
            ));
        };
        let mut resume = pb::ResumeCall::from(value);
        resume.call_id = call_id;
        self.answer(pb::client_request::Kind::ResumeCall(resume))
            .await
    }

    /// Answers a call by raising `RuntimeError(message)` in the program.
    pub async fn resume_error(&mut self, message: &str) -> Result<(), ScriptError> {
        let Some(Pending::Call(call_id)) = self.pending else {
            return Err(ScriptError::Misuse(
                "no call is waiting for an answer".to_owned(),
            ));
        };
        let exception = MontyException::new(ExcType::RuntimeError, Some(message.to_owned()));
        let (result, values) = ext_result_to_proto(ExtFunctionResult::Error(exception));
        let resume = pb::ResumeCall {
            call_id,
            result: Some(result),
            values,
        };
        self.answer(pb::client_request::Kind::ResumeCall(resume))
            .await
    }

    /// Answers a call to a name that is not a function the program may call: `NameError`.
    /// A program that calls a bare name reaches the host as a call by that name, with no name
    /// lookup before it, so the host decides at call time.
    pub async fn resume_not_found(&mut self) -> Result<(), ScriptError> {
        let Some(Pending::Call(call_id)) = self.pending else {
            return Err(ScriptError::Misuse(
                "no call is waiting for an answer".to_owned(),
            ));
        };
        let (result, values) =
            ext_result_to_proto(ExtFunctionResult::NotFound(self.call_name.clone()));
        let resume = pb::ResumeCall {
            call_id,
            result: Some(result),
            values,
        };
        self.answer(pb::client_request::Kind::ResumeCall(resume))
            .await
    }

    /// Answers a name lookup: a host function of that name, or `None` for `NameError`.
    pub async fn resume_name(&mut self, function: Option<&str>) -> Result<(), ScriptError> {
        let Some(Pending::Name) = self.pending else {
            return Err(ScriptError::Misuse(
                "no name lookup is waiting for an answer".to_owned(),
            ));
        };
        let result = match function {
            Some(name) => NameLookupResult::Value(MontyObject::function(name, None)),
            None => NameLookupResult::Undefined,
        };
        self.answer(pb::client_request::Kind::ResumeNameLookup(result.into()))
            .await
    }

    /// Ends the program from outside (stop, budget): the worker is killed.
    pub async fn abort(mut self) {
        self.kill_and_wait().await;
    }

    async fn answer(&mut self, kind: pb::client_request::Kind) -> Result<(), ScriptError> {
        self.pending = None;
        self.send(kind)
            .await
            .map_err(|err| ScriptError::Crashed(format!("could not answer the interpreter: {err}")))
    }

    /// Counts a suspension; true when it is over `script_suspensions`.
    fn count_suspension(&mut self) -> bool {
        self.suspensions += 1;
        self.suspensions > self.limits.suspensions
    }

    /// `AbortFeed` instead of an answer (N1): the program ends with "too many requests".
    async fn abort_over_budget(&mut self) -> ScriptEvent {
        let exception = MontyException::new(
            ExcType::RuntimeError,
            Some("the program made too many requests".to_owned()),
        );
        let abort = pb::AbortFeed {
            exception: Some((&exception).into()),
        };
        if self
            .send(pb::client_request::Kind::AbortFeed(abort))
            .await
            .is_ok()
        {
            // The worker's reply ends the turn; its content does not matter, the program is over.
            let _ = self.read_event(None).await;
        }
        self.end(Err(ScriptError::TooManyRequests)).await
    }

    /// A protocol violation: kill the worker, fail the program.
    async fn violation(&mut self, what: &str) -> ScriptEvent {
        self.end(Err(ScriptError::Misbehaved(what.to_owned())))
            .await
    }

    /// The program is over: the worker is killed whatever the outcome.
    async fn end(&mut self, result: Result<(), ScriptError>) -> ScriptEvent {
        self.pending = None;
        self.kill_and_wait().await;
        ScriptEvent::Done(result)
    }

    /// Turns a failed read into the program's error, classifying a dead worker.
    async fn failure(&mut self, failure: ReadFailure) -> ScriptError {
        match failure {
            ReadFailure::Timeout => ScriptError::Unresponsive,
            ReadFailure::Deadline => ScriptError::WallBudget,
            ReadFailure::Frame(what) => ScriptError::Misbehaved(what),
            ReadFailure::Eof | ReadFailure::Io => {
                let status = timeout(Duration::from_secs(2), self.child.wait())
                    .await
                    .ok()
                    .and_then(Result::ok);
                if status.and_then(|status| status.code()) == Some(OOM_EXIT_CODE) {
                    return ScriptError::OutOfMemory;
                }
                let stderr = self.stderr_snapshot().await;
                let status =
                    status.map_or_else(|| "no exit status".to_owned(), |status| status.to_string());
                ScriptError::Crashed(if stderr.is_empty() {
                    status
                } else {
                    format!("{status}: {stderr}")
                })
            }
        }
    }

    async fn send(&mut self, kind: pb::client_request::Kind) -> io::Result<()> {
        let request = pb::ClientRequest {
            kind: Some(kind),
            trace_parent: None,
        };
        encode_framed_into(&request, &mut self.send_buf)
            .map_err(|err| io::Error::other(err.to_string()))?;
        timeout(self.request_timeout, self.stdin.write_all(&self.send_buf))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the worker stopped reading"))??;
        Ok(())
    }

    async fn read_event(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<pb::WorkerEvent, ReadFailure> {
        let stdout = &mut self.stdout;
        let (wait, by_deadline) = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                (self.request_timeout.min(left), left < self.request_timeout)
            }
            None => (self.request_timeout, false),
        };
        let body = timeout(wait, async {
            let mut len = [0_u8; 4];
            stdout.read_exact(&mut len).await?;
            let len = u32::from_le_bytes(len);
            if len > MAX_EVENT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("an event frame of {len} bytes"),
                ));
            }
            let mut body = vec![0_u8; len as usize];
            stdout.read_exact(&mut body).await?;
            Ok::<_, io::Error>(body)
        })
        .await
        .map_err(|_| {
            if by_deadline {
                ReadFailure::Deadline
            } else {
                ReadFailure::Timeout
            }
        })?
        .map_err(|err| match err.kind() {
            io::ErrorKind::UnexpectedEof => ReadFailure::Eof,
            io::ErrorKind::InvalidData => ReadFailure::Frame(err.to_string()),
            _ => ReadFailure::Io,
        })?;
        decode_frame_with_budget::<pb::WorkerEvent>(&body, DECODE_BUDGET)
            .map_err(|err| ReadFailure::Frame(format!("an undecodable event: {err}")))
    }
}
