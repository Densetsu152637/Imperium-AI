//! Windows: nested Job Objects, assigned while suspended (plan L2).
//!
//! The hierarchy is `total → session → unit` (a unit with no session sits under the total, and
//! terminal and model units under nothing of ours). A job is nested by the first process
//! assigned to it, under the job that process is in at that moment, so every spawn assigns in
//! that order. The process is created suspended, assigned, and only then is its one thread
//! (found with a Toolhelp thread snapshot, assumption A3) resumed. Any failure ends it before it
//! runs.
//!
//! One completion port gets every job's messages. A memory limit on Windows makes allocations
//! fail rather than killing anything, so on `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` (and on the
//! active-process limit) the unit's job is terminated and the unit reported as limited. A session
//! or total limit stops that group's largest unit, not whichever process allocated last.
//!
//! Every Win32 call here is unsafe, so this module, and only this one on Windows, allows it.
//! Each block says why it is sound.
#![allow(unsafe_code)]

use std::{
    collections::HashMap,
    ffi::{OsStr, c_void},
    io,
    mem::size_of,
    ptr,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use tokio::process::{Child, Command};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        IO::{
            CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED,
            PostQueuedCompletionStatus,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
            JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT,
            JOB_OBJECT_MSG_JOB_MEMORY_LIMIT, JOB_OBJECT_UILIMIT_DESKTOP,
            JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
            JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
            JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
            JOB_OBJECT_UILIMIT_WRITECLIPBOARD, JOBOBJECT_ASSOCIATE_COMPLETION_PORT,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_UI_RESTRICTIONS,
            JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOBOBJECTINFOCLASS, JobObjectAssociateCompletionPortInformation,
            JobObjectBasicAccountingInformation, JobObjectBasicUIRestrictions,
            JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_SUSPENDED, GetCurrentProcess, OpenProcess, OpenThread,
            PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread, THREAD_SUSPEND_RESUME,
            TerminateProcess,
        },
    },
};

use crate::{Caps, Class, Config, Limit, LimitError, Mechanism, Shared, UnitSpec, Usage};

/// `JobObjectMemoryUsageInformation` (winnt.h): the job's committed memory now and at its peak.
/// windows-sys does not carry it.
const JOB_OBJECT_MEMORY_USAGE_INFORMATION: JOBOBJECTINFOCLASS = 28;

#[repr(C)]
#[derive(Default)]
struct JobMemoryUsage {
    job_memory: u64,
    peak_job_memory_used: u64,
}

/// The completion key that stops the port thread.
const SHUTDOWN: usize = 0;
/// The total job's key.
const TOTAL: usize = 1;
/// The first key handed out to sessions and units.
const FIRST_KEY: usize = 16;

/// How often a unit's use is sampled for its peaks.
const SAMPLE: Duration = Duration::from_millis(100);

/// An owned handle, closed when dropped.
#[derive(Debug)]
struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide value; the objects used here (jobs, processes, a
// completion port) may be used from any thread.
unsafe impl Send for Handle {}
// SAFETY: as above; every call made through a shared handle is thread-safe in Win32.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is owned by this value and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn check(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// An anonymous Job Object. Closing the last handle kills its processes when it was limited with
/// kill-on-close.
#[derive(Debug)]
pub struct Job(Handle);

impl Job {
    /// A new job with no limits and no name, so no other process can open it by name.
    ///
    /// # Errors
    /// When the job cannot be created.
    pub fn new() -> io::Result<Self> {
        // SAFETY: no attributes and no name.
        let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(Handle(job)))
    }

    /// The raw handle, for calls this wrapper does not cover.
    #[must_use]
    pub fn as_raw(&self) -> HANDLE {
        self.0.0
    }

    /// Sets the caps: the job's memory (the committed memory of the whole tree), its active
    /// processes and a hard CPU cap. Always kill-on-close and die-on-unhandled-exception; never
    /// breakaway unless `breakaway_ok` (only the probe's control uses it).
    ///
    /// # Errors
    /// When the limits are refused.
    pub fn limit(&self, caps: &Caps, breakaway_ok: bool) -> io::Result<()> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let mut flags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        if breakaway_ok {
            flags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        if let Some(bytes) = caps.memory_bytes {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
            limits.JobMemoryLimit = usize::try_from(bytes).unwrap_or(usize::MAX);
        }
        if let Some(processes) = caps.processes {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = processes;
        }
        limits.BasicLimitInformation.LimitFlags = flags;
        self.set(JobObjectExtendedLimitInformation, &limits)?;
        if let Some(percent) = caps.cpu_percent {
            let mut rate = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                    | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                ..Default::default()
            };
            // Hundredths of a percent of the whole machine.
            rate.Anonymous.CpuRate = u32::from(percent.clamp(1, 100)) * 100;
            self.set(JobObjectCpuRateControlInformation, &rate)?;
        }
        Ok(())
    }

    /// Every UI restriction (the code-mode worker's job, #328).
    ///
    /// # Errors
    /// When the restrictions are refused.
    pub fn restrict_ui(&self) -> io::Result<()> {
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        };
        self.set(JobObjectBasicUIRestrictions, &ui)
    }

    fn set<T>(&self, class: JOBOBJECTINFOCLASS, value: &T) -> io::Result<()> {
        // SAFETY: the job is open; `value` is the structure `class` names, with its size.
        check(unsafe {
            SetInformationJobObject(
                self.as_raw(),
                class,
                ptr::from_ref(value).cast(),
                size_of::<T>() as u32,
            )
        })
    }

    fn query<T: Default>(&self, class: JOBOBJECTINFOCLASS) -> io::Result<T> {
        let mut value = T::default();
        // SAFETY: the job is open; `value` is a writable structure of the size given.
        check(unsafe {
            QueryInformationJobObject(
                self.as_raw(),
                class,
                ptr::from_mut(&mut value).cast(),
                size_of::<T>() as u32,
                ptr::null_mut(),
            )
        })?;
        Ok(value)
    }

    /// Puts `process` (a handle with `PROCESS_SET_QUOTA` and `PROCESS_TERMINATE`) in the job.
    ///
    /// # Errors
    /// When the assignment is refused.
    pub fn assign(&self, process: HANDLE) -> io::Result<()> {
        // SAFETY: both handles are open for the call.
        check(unsafe { AssignProcessToJobObject(self.as_raw(), process) })
    }

    /// Ends every process in the job; does not wait.
    pub fn terminate(&self) {
        // SAFETY: the job handle this value owns.
        unsafe { TerminateJobObject(self.as_raw(), 1) };
    }

    /// The job's committed memory now and at its peak.
    #[must_use]
    pub fn memory(&self) -> (u64, u64) {
        if let Ok(usage) = self.query::<JobMemoryUsage>(JOB_OBJECT_MEMORY_USAGE_INFORMATION) {
            return (usage.job_memory, usage.peak_job_memory_used);
        }
        let peak = self
            .query::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(JobObjectExtendedLimitInformation)
            .map_or(0, |limits| limits.PeakJobMemoryUsed as u64);
        (peak, peak)
    }

    /// The job's CPU time and active processes.
    #[must_use]
    pub fn accounting(&self) -> (Duration, u32) {
        self.query::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>(JobObjectBasicAccountingInformation)
            .map_or((Duration::ZERO, 0), |info| {
                // 100-nanosecond units.
                let ticks = u64::try_from(info.TotalUserTime.saturating_add(info.TotalKernelTime))
                    .unwrap_or(0);
                (
                    Duration::from_nanos(ticks.saturating_mul(100)),
                    info.ActiveProcesses,
                )
            })
    }

    /// Whether the process `pid` is in this job (or a job nested in it).
    ///
    /// # Errors
    /// When the process cannot be opened (it is gone, for example).
    pub fn contains(&self, pid: u32) -> io::Result<bool> {
        // SAFETY: opens a query handle, owned below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = Handle(process);
        let mut result = 0;
        // SAFETY: both handles are open; `result` is a valid out-pointer.
        check(unsafe { IsProcessInJob(process.0, self.as_raw(), &raw mut result) })?;
        Ok(result != 0)
    }

    fn associate(&self, port: &Handle, key: usize) -> io::Result<()> {
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: ptr::without_provenance_mut::<c_void>(key),
            CompletionPort: port.0,
        };
        self.set(JobObjectAssociateCompletionPortInformation, &association)
    }
}

/// What a completion key names.
#[derive(Debug, Clone)]
enum Target {
    Total,
    Session(String),
    Unit(Weak<UnitOs>),
}

#[derive(Debug)]
struct Group {
    job: Job,
    caps: Caps,
}

#[derive(Debug)]
struct Inner {
    port: Handle,
    total: Group,
    sessions: Mutex<HashMap<String, Arc<Group>>>,
    keys: Mutex<HashMap<usize, Target>>,
    next_key: AtomicUsize,
}

#[derive(Debug)]
pub(crate) struct Limits {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl Limits {
    pub(crate) fn new(config: Config) -> Result<Self, LimitError> {
        // SAFETY: a new port, not associated with any file.
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(LimitError::new("port", io::Error::last_os_error()));
        }
        let port = Handle(port);
        let job = Job::new().map_err(|err| LimitError::new("job", err))?;
        job.limit(&config.total, false)
            .and_then(|()| job.associate(&port, TOTAL))
            .map_err(|err| LimitError::new("job", err))?;
        let inner = Arc::new(Inner {
            port,
            total: Group {
                job,
                caps: config.total,
            },
            sessions: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashMap::from([(TOTAL, Target::Total)])),
            next_key: AtomicUsize::new(FIRST_KEY),
        });
        let thread = {
            let inner = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("imperium-limits-port".to_owned())
                .spawn(move || port_loop(&inner))
                .map_err(|err| LimitError::new("port", err))?
        };
        Ok(Self {
            inner,
            thread: Some(thread),
        })
    }

    pub(crate) fn mechanism(&self) -> Mechanism {
        Mechanism::Job
    }

    pub(crate) fn set_session(&self, session: &str, caps: Caps) -> Result<(), LimitError> {
        let mut sessions = lock(&self.inner.sessions);
        if let Some(group) = sessions.get(session) {
            return group
                .job
                .limit(&caps, false)
                .map_err(|err| LimitError::new("session", err));
        }
        let group = self.inner.new_session(session, caps)?;
        sessions.insert(session.to_owned(), group);
        Ok(())
    }

    pub(crate) fn command(
        &self,
        spec: UnitSpec,
        program: &OsStr,
    ) -> Result<(Command, Prepared), LimitError> {
        let job = Job::new().map_err(|err| LimitError::new("job", err))?;
        let key = self.inner.next_key.fetch_add(1, Ordering::Relaxed);
        job.limit(&spec.caps, false)
            .and_then(|()| job.associate(&self.inner.port, key))
            .map_err(|err| LimitError::new("job", err))?;
        Ok((
            Command::new(program),
            Prepared {
                inner: Arc::clone(&self.inner),
                spec,
                job,
                key,
                fail_at: None,
            },
        ))
    }

    /// Whether `pid` is in the session's job (tests of the nesting order).
    pub(crate) fn session_contains(&self, session: &str, pid: u32) -> io::Result<bool> {
        let group = lock(&self.inner.sessions).get(session).cloned();
        group.map_or(Ok(false), |group| group.job.contains(pid))
    }

    /// Whether `pid` is in the total job.
    pub(crate) fn total_contains(&self, pid: u32) -> io::Result<bool> {
        self.inner.total.job.contains(pid)
    }
}

impl crate::Limits {
    /// Whether `pid` is in the session's job (Windows; tests of the nesting order).
    ///
    /// # Errors
    /// When the process cannot be opened.
    pub fn session_contains(&self, session: &str, pid: u32) -> io::Result<bool> {
        self.inner.session_contains(session, pid)
    }

    /// Whether `pid` is in the total job (Windows).
    ///
    /// # Errors
    /// When the process cannot be opened.
    pub fn total_contains(&self, pid: u32) -> io::Result<bool> {
        self.inner.total_contains(pid)
    }
}

impl Drop for Limits {
    fn drop(&mut self) {
        // SAFETY: the port is open until `inner` is dropped, which the thread's copy delays.
        unsafe { PostQueuedCompletionStatus(self.inner.port.0, 0, SHUTDOWN, ptr::null()) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Inner {
    fn new_session(&self, session: &str, caps: Caps) -> Result<Arc<Group>, LimitError> {
        let job = Job::new().map_err(|err| LimitError::new("session", err))?;
        let key = self.next_key.fetch_add(1, Ordering::Relaxed);
        job.limit(&caps, false)
            .and_then(|()| job.associate(&self.port, key))
            .map_err(|err| LimitError::new("session", err))?;
        lock(&self.keys).insert(key, Target::Session(session.to_owned()));
        Ok(Arc::new(Group { job, caps }))
    }

    fn session(&self, session: &str) -> Result<Arc<Group>, LimitError> {
        let mut sessions = lock(&self.sessions);
        if let Some(group) = sessions.get(session) {
            return Ok(Arc::clone(group));
        }
        let group = self.new_session(session, Caps::default())?;
        sessions.insert(session.to_owned(), Arc::clone(&group));
        Ok(group)
    }

    /// The live units, with the session each belongs to.
    fn units(&self) -> Vec<Arc<UnitOs>> {
        let mut keys = lock(&self.keys);
        keys.retain(|_, target| !matches!(target, Target::Unit(unit) if unit.strong_count() == 0));
        keys.values()
            .filter_map(|target| match target {
                Target::Unit(unit) => unit.upgrade(),
                _ => None,
            })
            .collect()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Receives every job's messages until [`SHUTDOWN`].
fn port_loop(inner: &Inner) {
    loop {
        let mut message = 0_u32;
        let mut key = 0_usize;
        let mut overlapped: *mut OVERLAPPED = ptr::null_mut();
        // SAFETY: the port is open while `inner` lives; every out-pointer is valid.
        let ok = unsafe {
            GetQueuedCompletionStatus(
                inner.port.0,
                &raw mut message,
                &raw mut key,
                &raw mut overlapped,
                u32::MAX,
            )
        };
        if ok == 0 || key == SHUTDOWN {
            return;
        }
        let limit = match message {
            JOB_OBJECT_MSG_JOB_MEMORY_LIMIT => true,
            JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT => false,
            _ => continue,
        };
        let target = lock(&inner.keys).get(&key).cloned();
        match target {
            Some(Target::Unit(unit)) => {
                if let Some(unit) = unit.upgrade() {
                    let (kind, value) = if limit {
                        (Limit::Memory, unit.caps.memory_bytes)
                    } else {
                        (Limit::Processes, unit.caps.processes.map(u64::from))
                    };
                    unit.shared.limit(kind, value.unwrap_or_default());
                    unit.job.terminate();
                }
            }
            Some(Target::Session(session)) => {
                let caps = lock(&inner.sessions)
                    .get(&session)
                    .map(|group| group.caps)
                    .unwrap_or_default();
                let members: Vec<_> = inner
                    .units()
                    .into_iter()
                    .filter(|unit| unit.session.as_deref() == Some(session.as_str()))
                    .collect();
                stop_largest(
                    &members,
                    limit,
                    &caps,
                    Limit::SessionMemory,
                    Limit::SessionProcesses,
                );
            }
            Some(Target::Total) => {
                let members: Vec<_> = inner
                    .units()
                    .into_iter()
                    .filter(|unit| unit.in_total)
                    .collect();
                stop_largest(
                    &members,
                    limit,
                    &inner.total.caps,
                    Limit::TotalMemory,
                    Limit::TotalProcesses,
                );
            }
            None => {}
        }
    }
}

/// Stops the group's largest unit: by committed memory for a memory limit, by active processes
/// for a process limit. Not the allocator, which may be small.
fn stop_largest(
    units: &[Arc<UnitOs>],
    memory: bool,
    caps: &Caps,
    memory_limit: Limit,
    process_limit: Limit,
) {
    let largest = units.iter().max_by_key(|unit| {
        if memory {
            unit.job.memory().0
        } else {
            u64::from(unit.job.accounting().1)
        }
    });
    if let Some(unit) = largest {
        if memory {
            unit.shared
                .limit(memory_limit, caps.memory_bytes.unwrap_or_default());
        } else {
            unit.shared.limit(
                process_limit,
                caps.processes.map(u64::from).unwrap_or_default(),
            );
        }
        unit.job.terminate();
    }
}

/// A test hook: the step at which [`Prepared::spawn`] fails instead of acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailAt {
    Assign,
    Resume,
}

pub(crate) struct Prepared {
    inner: Arc<Inner>,
    spec: UnitSpec,
    job: Job,
    key: usize,
    fail_at: Option<FailAt>,
}

impl Prepared {
    pub(crate) fn spawn(self, command: &mut Command) -> Result<(Child, Arc<UnitOs>), LimitError> {
        let Self {
            inner,
            spec,
            job,
            key,
            fail_at,
        } = self;
        // Terminal and model units sit outside the total (S2, L6).
        let in_total = !matches!(spec.class, Class::Terminal | Class::Model);
        let session = match (&spec.session, in_total) {
            (Some(name), true) => Some(inner.session(name)?),
            _ => None,
        };
        command.creation_flags(CREATE_SUSPENDED);
        let child = command
            .spawn()
            .map_err(|err| LimitError::new("spawn", err))?;
        let pid = child.id().unwrap_or_default();
        let raw = child
            .raw_handle()
            .ok_or_else(|| LimitError::new("spawn", "the process has no handle"))?;
        let process = duplicate(raw.cast()).map_err(|err| {
            // SAFETY: the child's own handle, still open; it has run nothing yet.
            unsafe { TerminateProcess(raw.cast(), 1) };
            LimitError::new("assign", err)
        })?;
        let started = (|| -> Result<Duration, LimitError> {
            if in_total {
                inner
                    .total
                    .job
                    .assign(process.0)
                    .map_err(|err| LimitError::new("assign", err))?;
            }
            if let Some(group) = &session {
                group
                    .job
                    .assign(process.0)
                    .map_err(|err| LimitError::new("assign", err))?;
            }
            if fail_at == Some(FailAt::Assign) {
                return Err(LimitError::new("assign", "forced to fail by the test hook"));
            }
            job.assign(process.0)
                .map_err(|err| LimitError::new("assign", err))?;
            if fail_at == Some(FailAt::Resume) {
                return Err(LimitError::new("resume", "forced to fail by the test hook"));
            }
            resume(pid)
        })();
        let snapshot = match started {
            Ok(snapshot) => snapshot,
            Err(err) => {
                // It may not be in its job yet, so end the process itself. Nothing of it ran.
                // SAFETY: our duplicate of the process handle, open.
                unsafe { TerminateProcess(process.0, 1) };
                return Err(err);
            }
        };
        let os = Arc::new(UnitOs {
            job,
            caps: spec.caps,
            session: spec.session.filter(|_| in_total),
            in_total,
            shared: Shared::default(),
            snapshot,
            _process: process,
        });
        lock(&inner.keys).insert(key, Target::Unit(Arc::downgrade(&os)));
        Ok((child, os))
    }
}

/// A duplicate of `process`, so the unit holds the process (and its id) as long as it lives.
fn duplicate(process: HANDLE) -> io::Result<Handle> {
    let mut copy: HANDLE = ptr::null_mut();
    // SAFETY: `process` is open; the copy goes to this process, with the same access.
    check(unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            process,
            GetCurrentProcess(),
            &raw mut copy,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    })?;
    Ok(Handle(copy))
}

/// Resumes the suspended process `pid`: finds its threads with a Toolhelp snapshot, insists
/// there is exactly one (A3), and resumes it. Returns how long the snapshot and its walk took.
fn resume(pid: u32) -> Result<Duration, LimitError> {
    let start = Instant::now();
    let threads = threads_of(pid).map_err(|err| LimitError::new("resume", err))?;
    let snapshot = start.elapsed();
    let [thread] = threads.as_slice() else {
        return Err(LimitError::new(
            "resume",
            format!(
                "a suspended process should have one thread, found {}",
                threads.len()
            ),
        ));
    };
    // SAFETY: opens a thread handle with only the access needed, owned below.
    let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, *thread) };
    if handle.is_null() {
        return Err(LimitError::new("resume", io::Error::last_os_error()));
    }
    let handle = Handle(handle);
    // SAFETY: the open thread handle.
    let previous = unsafe { ResumeThread(handle.0) };
    if previous == u32::MAX {
        return Err(LimitError::new("resume", io::Error::last_os_error()));
    }
    Ok(snapshot)
}

/// The ids of the threads of `pid`, from a snapshot of every thread on the machine.
pub(crate) fn threads_of(pid: u32) -> io::Result<Vec<u32>> {
    // SAFETY: a thread snapshot (the process id is ignored for threads), owned below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = Handle(snapshot);
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut threads = Vec::new();
    // SAFETY: the snapshot is open; `entry` has its size set as the call requires.
    let mut more = unsafe { Thread32First(snapshot.0, &raw mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            threads.push(entry.th32ThreadID);
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot.0, &raw mut entry) } != 0;
    }
    Ok(threads)
}

/// The OS side of a unit: its job, held as long as the unit lives (closing it kills the tree).
#[derive(Debug)]
pub(crate) struct UnitOs {
    job: Job,
    caps: Caps,
    session: Option<String>,
    in_total: bool,
    pub(crate) shared: Shared,
    snapshot: Duration,
    _process: Handle,
}

impl UnitOs {
    pub(crate) fn job(&self) -> &Job {
        &self.job
    }

    pub(crate) fn snapshot_time(&self) -> Duration {
        self.snapshot
    }

    pub(crate) fn usage(&self) -> Usage {
        let (memory, peak) = self.job.memory();
        let (cpu, active) = self.job.accounting();
        self.shared.sample(memory, active, 0);
        Usage {
            memory_bytes: memory,
            peak_memory_bytes: peak,
            processes: active,
            peak_processes: self.shared.peak_processes.load(Ordering::Relaxed),
            tasks: None,
            peak_tasks: None,
            cpu_time: Some(cpu),
        }
    }

    pub(crate) fn not_enforced(&self) -> Vec<Limit> {
        if self.caps.tasks.is_some() {
            vec![Limit::Tasks]
        } else {
            Vec::new()
        }
    }

    pub(crate) fn monitor(self: &Arc<Self>) -> Monitor {
        Monitor(Arc::clone(self))
    }
}

/// The job does the enforcing (the port thread terminates it); the monitor samples peaks.
pub(crate) struct Monitor(Arc<UnitOs>);

impl Monitor {
    pub(crate) fn interval(&self) -> Duration {
        SAMPLE
    }

    pub(crate) fn check(&mut self) -> Option<(Limit, u64)> {
        let _ = self.0.usage();
        None
    }

    pub(crate) async fn changed(&mut self) {
        std::future::pending::<()>().await;
    }

    pub(crate) fn kill(&mut self) {
        self.0.job.terminate();
    }

    pub(crate) fn after_exit(&mut self) -> Option<(Limit, u64)> {
        None
    }

    pub(crate) async fn kill_remaining(&mut self) {
        self.0.job.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits as Facade, UnitSpec};

    fn spec() -> UnitSpec {
        UnitSpec {
            class: Class::Command,
            caps: Caps {
                memory_bytes: Some(256 << 20),
                processes: Some(8),
                ..Caps::default()
            },
            session: Some("s".to_owned()),
            label: "test".to_owned(),
        }
    }

    /// A failed assignment or resume leaves no running process: the suspended process is ended
    /// before any of its code runs. It was already in the session's job, which shows it existed.
    #[tokio::test]
    async fn a_failed_assignment_leaves_no_process() {
        let limits = Facade::new(Config::default()).unwrap();
        for stage in [FailAt::Assign, FailAt::Resume] {
            let (mut command, mut prepared) =
                limits.inner.command(spec(), OsStr::new("cmd.exe")).unwrap();
            command.args(["/c", "ping -n 30 127.0.0.1 >NUL"]);
            prepared.fail_at = Some(stage);
            let err = prepared.spawn(&mut command).unwrap_err();
            assert_eq!(
                err.stage,
                if stage == FailAt::Assign {
                    "assign"
                } else {
                    "resume"
                }
            );
            let session = limits.inner.inner.session("s").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = session
                    .job
                    .query(JobObjectBasicAccountingInformation)
                    .unwrap();
                assert!(
                    info.TotalProcesses >= 1,
                    "the process never reached the session job"
                );
                if info.ActiveProcesses == 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "the refused process is still running"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    #[test]
    fn the_snapshot_finds_this_process_threads() {
        let threads = threads_of(std::process::id()).unwrap();
        assert!(!threads.is_empty());
    }
}

/// Test support: commits `bytes` of pagefile-backed section memory (`CreateFileMapping` on
/// `INVALID_HANDLE_VALUE`) and touches every page, so the `alloc --section` probe can see whether
/// a job's memory limit counts it (L15). The mapping is leaked on purpose.
///
/// # Errors
/// When the section cannot be created or mapped (as when a job limit refuses it).
#[cfg(feature = "test-support")]
pub fn commit_section(bytes: usize) -> io::Result<()> {
    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE,
    };
    let size = bytes as u64;
    // SAFETY: a pagefile-backed section with no name and no attributes.
    let section = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            ptr::null(),
            PAGE_READWRITE,
            (size >> 32) as u32,
            size as u32,
            ptr::null(),
        )
    };
    if section.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the section was just created with `bytes` bytes, mapped whole for writing.
    let view = unsafe { MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes) };
    if view.Value.is_null() {
        let err = io::Error::last_os_error();
        // SAFETY: the section handle created above.
        unsafe { CloseHandle(section) };
        return Err(err);
    }
    let base = view.Value.cast::<u8>();
    for offset in (0..bytes).step_by(4096) {
        // SAFETY: `offset` is inside the `bytes`-long view mapped for writing.
        unsafe { base.add(offset).write_volatile(1) };
    }
    Ok(())
}
