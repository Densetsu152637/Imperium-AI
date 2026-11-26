//! macOS: a daemon-side poll of each unit's tree (plan L4). Darwin has no hard memory or CPU
//! limit a third-party program can set (jetsam limits need entitlements, `RLIMIT_AS` and
//! `RLIMIT_DATA` are not enforced), so every 100 ms (50 ms for the small CAS, script and hook
//! classes) the poll sums the physical footprint of the unit's process group and its
//! descendants, counts them, and past a cap stops the group (`SIGSTOP`, then `SIGKILL`).
//! Overshoot is bounded by the allocation rate times the interval. CPU caps are reported as not
//! enforced.
//!
//! Also here: [`Watchdog`], the code-mode worker's single-process RSS poll (#328).
//!
//! Reading another process's memory and listing processes are C calls, so this module, and only
//! this one on macOS, allows `unsafe`. Each block says why it is sound.
#![allow(unsafe_code)]

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rustix::process::{Pid, Signal};
use tokio::{
    process::{Child, Command},
    task::JoinHandle,
};

use crate::{Caps, Class, Config, Limit, LimitError, Mechanism, Shared, UnitSpec, Usage};

/// How often a unit's tree is polled.
const POLL: Duration = Duration::from_millis(100);
/// The interval for the small, short classes.
const FAST_POLL: Duration = Duration::from_millis(50);
/// How many process ids one listing reads.
const MAX_PIDS: usize = 4096;

pub(crate) struct Limits;

impl Limits {
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn new(_config: Config) -> Result<Self, LimitError> {
        Ok(Self)
    }

    pub(crate) fn mechanism(&self) -> Mechanism {
        Mechanism::Poll
    }

    /// Session aggregates on macOS are the daemon's own accounting (a later stage); the poll
    /// enforces per-unit caps.
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn set_session(&self, _session: &str, _caps: Caps) -> Result<(), LimitError> {
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn command(
        &self,
        spec: UnitSpec,
        program: &OsStr,
    ) -> Result<(Command, Prepared), LimitError> {
        Ok((Command::new(program), Prepared { spec }))
    }
}

pub(crate) struct Prepared {
    spec: UnitSpec,
}

impl Prepared {
    pub(crate) fn spawn(self, command: &mut Command) -> Result<(Child, Arc<UnitOs>), LimitError> {
        let child = command
            .spawn()
            .map_err(|err| LimitError::new("spawn", err))?;
        let pgid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .unwrap_or_default();
        let interval = match self.spec.class {
            Class::Cas | Class::Script | Class::Hook => FAST_POLL,
            _ => POLL,
        };
        Ok((
            child,
            Arc::new(UnitOs {
                shared: Shared::default(),
                caps: self.spec.caps,
                pgid,
                interval,
            }),
        ))
    }
}

pub(crate) struct UnitOs {
    pub(crate) shared: Shared,
    caps: Caps,
    /// The unit's process group, led by its main process.
    pgid: i32,
    interval: Duration,
}

impl UnitOs {
    pub(crate) fn usage(&self) -> Usage {
        use std::sync::atomic::Ordering::Relaxed;
        Usage {
            memory_bytes: self.shared.memory.load(Relaxed),
            peak_memory_bytes: self.shared.peak_memory.load(Relaxed),
            processes: self.shared.processes.load(Relaxed),
            peak_processes: self.shared.peak_processes.load(Relaxed),
            tasks: None,
            peak_tasks: None,
            cpu_time: None,
        }
    }

    pub(crate) fn not_enforced(&self) -> Vec<Limit> {
        let mut missing = Vec::new();
        if self.caps.cpu_percent.is_some() {
            missing.push(Limit::Cpu);
        }
        if self.caps.tasks.is_some() {
            missing.push(Limit::Tasks);
        }
        missing
    }

    pub(crate) fn monitor(self: &Arc<Self>) -> Monitor {
        Monitor(Arc::clone(self))
    }

    /// The unit's processes: its group and the descendants of its leader.
    fn members(&self) -> Vec<i32> {
        let mut members: BTreeSet<i32> = group_pids(self.pgid).into_iter().collect();
        let mut seen = BTreeSet::new();
        let mut queue = vec![self.pgid];
        while let Some(parent) = queue.pop() {
            for child in child_pids(parent) {
                if seen.insert(child) {
                    queue.push(child);
                }
            }
            if seen.len() > MAX_PIDS {
                break;
            }
        }
        members.extend(seen);
        members.into_iter().collect()
    }
}

pub(crate) struct Monitor(Arc<UnitOs>);

impl Monitor {
    pub(crate) fn interval(&self) -> Duration {
        self.0.interval
    }

    pub(crate) fn check(&mut self) -> Option<(Limit, u64)> {
        let members = self.0.members();
        let memory: u64 = members.iter().filter_map(|&pid| footprint(pid)).sum();
        let processes = u32::try_from(members.len()).unwrap_or(u32::MAX);
        self.0.shared.sample(memory, processes, 0);
        let caps = self.0.caps;
        if let Some(cap) = caps.memory_bytes.filter(|&cap| memory > cap) {
            return Some((Limit::Memory, cap));
        }
        if let Some(cap) = caps.processes.filter(|&cap| processes > cap) {
            return Some((Limit::Processes, u64::from(cap)));
        }
        None
    }

    pub(crate) async fn changed(&mut self) {
        std::future::pending::<()>().await;
    }

    /// Freezes the group and every descendant, then kills them. Called while the leader is
    /// unreaped, so the group id is still its own; a descendant is a child of a live process, so
    /// its id cannot be reused before its parent reaps it.
    pub(crate) fn kill(&mut self) {
        let members = self.0.members();
        for signal in [Signal::STOP, Signal::KILL] {
            if let Some(pgid) = Pid::from_raw(self.0.pgid) {
                let _ = rustix::process::kill_process_group(pgid, signal);
            }
            for &pid in &members {
                if let Some(pid) = Pid::from_raw(pid) {
                    let _ = rustix::process::kill_process(pid, signal);
                }
            }
        }
    }

    pub(crate) fn after_exit(&mut self) -> Option<(Limit, u64)> {
        None
    }

    /// After the leader is reaped: signals each member still in the group (a residual: a process
    /// that left the group with `setsid` and was reparented leaves the poll, L4).
    pub(crate) async fn kill_remaining(&mut self) {
        let Some(pgid) = Pid::from_raw(self.0.pgid) else {
            return;
        };
        for pid in group_pids(self.0.pgid) {
            if let Some(pid) = Pid::from_raw(pid)
                && rustix::process::getpgid(Some(pid)).ok() == Some(pgid)
            {
                let _ = rustix::process::kill_process(pid, Signal::KILL);
            }
        }
    }
}

/// Lists process ids with one of libproc's listing calls into a zeroed buffer. The calls return a
/// count or a byte count depending on the call and release, so the nonzero ids are taken from
/// the buffer itself.
fn list(call: impl FnOnce(*mut libc::c_void, libc::c_int) -> libc::c_int) -> Vec<i32> {
    let mut pids: Vec<libc::pid_t> = vec![0; MAX_PIDS];
    let bytes = libc::c_int::try_from(MAX_PIDS * size_of::<libc::pid_t>()).unwrap_or(0);
    if call(pids.as_mut_ptr().cast(), bytes) <= 0 {
        return Vec::new();
    }
    pids.into_iter().filter(|&pid| pid > 0).collect()
}

fn group_pids(pgid: i32) -> Vec<i32> {
    // SAFETY: the buffer is `size` bytes long and writable; the call writes at most that much.
    list(|buffer, size| unsafe { libc::proc_listpgrppids(pgid, buffer, size) })
}

fn child_pids(parent: i32) -> Vec<i32> {
    // SAFETY: as above.
    list(|buffer, size| unsafe { libc::proc_listchildpids(parent, buffer, size) })
}

/// The physical footprint of `pid`, or `None` when it cannot be read (the process is gone).
fn footprint(pid: i32) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: `info` is a buffer for exactly the structure the `RUSAGE_INFO_V2` flavour fills.
    let filled =
        unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
    if filled != 0 {
        return None;
    }
    // SAFETY: the call succeeded and filled the whole structure.
    Some(unsafe { info.assume_init() }.ri_phys_footprint)
}

/// How often the code-mode watchdog samples the worker's resident memory.
const WATCHDOG_INTERVAL: Duration = Duration::from_millis(50);

/// The code-mode worker's watchdog (#328): kills one process once its resident memory passes a
/// ceiling. It stops when dropped.
#[derive(Debug)]
pub struct Watchdog {
    tripped: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Watchdog {
    /// Watches process `pid`, a child the daemon has not reaped (so the id cannot be reused while
    /// it is watched), and kills it once its resident memory passes `ceiling` bytes.
    #[must_use]
    pub fn start(pid: u32, ceiling: u64) -> Self {
        let tripped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&tripped);
        let task = tokio::spawn(async move {
            let Ok(pid) = libc::pid_t::try_from(pid) else {
                return;
            };
            loop {
                match resident_bytes(pid) {
                    Some(bytes) if bytes > ceiling => {
                        flag.store(true, Ordering::SeqCst);
                        if let Some(pid) = Pid::from_raw(pid) {
                            let _ = rustix::process::kill_process(pid, Signal::KILL);
                        }
                        return;
                    }
                    Some(_) => {}
                    // The process is gone.
                    None => return,
                }
                tokio::time::sleep(WATCHDOG_INTERVAL).await;
            }
        });
        Self { tripped, task }
    }

    /// Whether the watchdog killed the worker.
    #[must_use]
    pub fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The resident memory of `pid`, or `None` when it cannot be read (the process is gone).
#[must_use]
pub fn resident_bytes(pid: libc::pid_t) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = libc::c_int::try_from(size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: `info` is a buffer of exactly `size` bytes for the structure the call fills in.
    let filled = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if filled != size {
        return None;
    }
    // SAFETY: the call filled the whole structure.
    Some(unsafe { info.assume_init() }.pti_resident_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_a_footprint_and_a_group() {
        let me = i32::try_from(std::process::id()).unwrap();
        assert!(footprint(me).is_some_and(|bytes| bytes > 0));
        assert!(resident_bytes(me).is_some_and(|bytes| bytes > 0));
        let pgid = rustix::process::getpgid(None).unwrap();
        assert!(group_pids(pgid.as_raw_nonzero().get()).contains(&me));
    }
}
