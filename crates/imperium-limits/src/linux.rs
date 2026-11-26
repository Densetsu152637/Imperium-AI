//! Linux: a cgroup per unit in a delegated subtree, or rlimits and a tree poll (plan L3).
//!
//! Every unit starts through the launcher ([`crate::launcher`]), in its own process group. With
//! a subtree the launcher joins the unit's cgroup before `exec`: the kernel enforces
//! `memory.max` (with `memory.oom.group`, so the whole unit goes), `pids.max` and `cpu.max`, and
//! inotify on `memory.events` and `pids.events` reports what happened. Without one, the launcher
//! sets `RLIMIT_DATA` (per process) and raises its OOM score, and a 250 ms poll of the process
//! group sums PSS and counts processes and threads, stopping the group past a cap.

use std::{
    collections::HashSet,
    ffi::OsStr,
    os::fd::OwnedFd,
    sync::{Arc, Mutex},
    time::Duration,
};

use rustix::process::{Pid, Signal};
use tokio::{
    io::unix::AsyncFd,
    process::{Child, Command},
};

use crate::{
    Caps, Class, Config, Launcher, Limit, LimitError, Mechanism, Shared, UnitSpec, Usage,
    cgroup::{Tree, UnitCgroup, field},
};

/// How often a cgroup unit is sampled for its peaks.
const CGROUP_SAMPLE: Duration = Duration::from_millis(100);
/// How often the fallback polls a unit's tree (L3).
const POLL: Duration = Duration::from_millis(250);
/// How much an unconfined unit's OOM score is raised in the fallback.
const OOM_ADJ: i32 = 500;

enum Mode {
    Cgroup(Arc<Tree>),
    Fallback,
}

pub(crate) struct Limits {
    mode: Mode,
    launcher: Launcher,
    sessions: Mutex<HashSet<String>>,
}

/// A session's cgroup name: its id with anything but `[A-Za-z0-9_-]` replaced.
fn session_dir(session: &str) -> String {
    let clean: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    format!("total/s-{clean}")
}

impl Limits {
    pub(crate) fn new(config: Config) -> Result<Self, LimitError> {
        let launcher = config
            .launcher
            .ok_or_else(|| LimitError::new("launcher", "Linux units need a launcher"))?;
        let mode = match &config.cgroup_root {
            Some(root) => Mode::Cgroup(Arc::new(
                Tree::open(root, &config.total).map_err(|err| LimitError::new("cgroup", err))?,
            )),
            None => Mode::Fallback,
        };
        Ok(Self {
            mode,
            launcher,
            sessions: Mutex::new(HashSet::new()),
        })
    }

    pub(crate) fn mechanism(&self) -> Mechanism {
        match self.mode {
            Mode::Cgroup(_) => Mechanism::Cgroup,
            Mode::Fallback => Mechanism::RlimitPoll,
        }
    }

    pub(crate) fn set_session(&self, session: &str, caps: Caps) -> Result<(), LimitError> {
        if let Mode::Cgroup(tree) = &self.mode {
            tree.group(&session_dir(session), &caps)
                .map_err(|err| LimitError::new("session", err))?;
        }
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.to_owned());
        Ok(())
    }

    pub(crate) fn command(
        &self,
        spec: UnitSpec,
        program: &OsStr,
    ) -> Result<(Command, Prepared), LimitError> {
        let mut command = Command::new(&self.launcher.program);
        command
            .args(&self.launcher.args)
            .arg("--class")
            .arg(spec.class.as_str());
        // Terminal and model units sit outside the total (S2, L6).
        let in_total = !matches!(spec.class, Class::Terminal | Class::Model);
        let cgroup = match &self.mode {
            Mode::Cgroup(tree) => {
                let parent = match (&spec.session, in_total) {
                    (Some(session), true) => {
                        let known = self
                            .sessions
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .contains(session);
                        if !known {
                            self.set_session(session, Caps::default())?;
                        }
                        session_dir(session)
                    }
                    (None, true) => "total".to_owned(),
                    (_, false) => ".".to_owned(),
                };
                let unit = tree
                    .unit(&parent, spec.class.as_str(), &spec.caps)
                    .map_err(|err| LimitError::new("cgroup", err))?;
                command.arg("--cgroup").arg(&unit.path);
                Some(unit)
            }
            Mode::Fallback => {
                if let Some(bytes) = spec.caps.memory_bytes {
                    command.arg("--rlimit-data").arg(bytes.to_string());
                }
                if spec.class != Class::Terminal {
                    command.arg("--oom-adj").arg(OOM_ADJ.to_string());
                }
                None
            }
        };
        command.arg("--").arg(program);
        let cpu = matches!(&self.mode, Mode::Cgroup(tree) if tree.cpu);
        Ok((
            command,
            Prepared {
                spec,
                cgroup,
                cpu,
                in_session: in_total,
            },
        ))
    }
}

pub(crate) struct Prepared {
    spec: UnitSpec,
    cgroup: Option<UnitCgroup>,
    cpu: bool,
    in_session: bool,
}

impl Prepared {
    pub(crate) fn spawn(self, command: &mut Command) -> Result<(Child, Arc<UnitOs>), LimitError> {
        let child = command
            .spawn()
            .map_err(|err| LimitError::new("spawn", err))?;
        let pid = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .unwrap_or_default();
        let kind = match self.cgroup {
            Some(cgroup) => Kind::Cgroup(cgroup),
            None => Kind::Fallback { pgid: pid },
        };
        Ok((
            child,
            Arc::new(UnitOs {
                shared: Shared::default(),
                caps: self.spec.caps,
                in_session: self.in_session && self.spec.session.is_some(),
                cpu: self.cpu,
                kind,
            }),
        ))
    }
}

enum Kind {
    Cgroup(UnitCgroup),
    /// The launcher's process group, whose leader is the unit's main process.
    Fallback {
        pgid: i32,
    },
}

pub(crate) struct UnitOs {
    pub(crate) shared: Shared,
    caps: Caps,
    in_session: bool,
    cpu: bool,
    kind: Kind,
}

impl UnitOs {
    pub(crate) fn usage(&self) -> Usage {
        use std::sync::atomic::Ordering::Relaxed;
        let cpu_time = match &self.kind {
            Kind::Cgroup(cgroup) => {
                let read = |name: &str| cgroup.read(name).ok();
                let memory = read("memory.current")
                    .and_then(|text| text.trim().parse().ok())
                    .unwrap_or(0);
                let tasks = read("pids.current")
                    .and_then(|text| text.trim().parse().ok())
                    .unwrap_or(0);
                let processes = u32::try_from(cgroup.procs().len()).unwrap_or(u32::MAX);
                self.shared.sample(memory, processes, tasks);
                if let Some(peak) = read("memory.peak").and_then(|text| text.trim().parse().ok()) {
                    self.shared.peak_memory.fetch_max(peak, Relaxed);
                }
                if let Some(peak) = read("pids.peak").and_then(|text| text.trim().parse().ok()) {
                    self.shared.peak_tasks.fetch_max(peak, Relaxed);
                }
                read("cpu.stat")
                    .and_then(|text| field(&text, "usage_usec"))
                    .map(Duration::from_micros)
            }
            Kind::Fallback { .. } => None,
        };
        Usage {
            memory_bytes: self.shared.memory.load(Relaxed),
            peak_memory_bytes: self.shared.peak_memory.load(Relaxed),
            processes: self.shared.processes.load(Relaxed),
            peak_processes: self.shared.peak_processes.load(Relaxed),
            tasks: Some(self.shared.tasks.load(Relaxed)),
            peak_tasks: Some(self.shared.peak_tasks.load(Relaxed)),
            cpu_time,
        }
    }

    pub(crate) fn not_enforced(&self) -> Vec<Limit> {
        let mut missing = Vec::new();
        if self.caps.cpu_percent.is_some() && !self.cpu {
            missing.push(Limit::Cpu);
        }
        // A cgroup counts tasks; the process count is the fallback poll's.
        if self.caps.processes.is_some() && matches!(self.kind, Kind::Cgroup(_)) {
            missing.push(Limit::Processes);
        }
        missing
    }

    pub(crate) fn monitor(self: &Arc<Self>) -> Monitor {
        let watch = match &self.kind {
            Kind::Cgroup(cgroup) => cgroup.watch().ok().and_then(crate::notify::register),
            Kind::Fallback { .. } => None,
        };
        Monitor {
            os: Arc::clone(self),
            watch,
        }
    }
}

pub(crate) struct Monitor {
    os: Arc<UnitOs>,
    watch: Option<AsyncFd<OwnedFd>>,
}

impl Monitor {
    pub(crate) fn interval(&self) -> Duration {
        match self.os.kind {
            Kind::Cgroup(_) => CGROUP_SAMPLE,
            Kind::Fallback { .. } => POLL,
        }
    }

    pub(crate) fn check(&mut self) -> Option<(Limit, u64)> {
        let caps = self.os.caps;
        match &self.os.kind {
            Kind::Cgroup(cgroup) => {
                let _ = self.os.usage();
                // The kernel stops a memory overrun itself (oom.group); a refused fork only fails
                // the fork, so the daemon stops the unit.
                let refused = cgroup
                    .read("pids.events")
                    .ok()
                    .and_then(|text| field(&text, "max"))
                    .unwrap_or(0);
                (refused > 0).then(|| (Limit::Tasks, caps.tasks.map(u64::from).unwrap_or(0)))
            }
            Kind::Fallback { pgid } => {
                let tree = group_members(*pgid);
                let memory: u64 = tree.iter().map(|member| member.pss).sum();
                let processes = u32::try_from(tree.len()).unwrap_or(u32::MAX);
                let tasks: u32 = tree.iter().map(|member| member.threads).sum();
                self.os.shared.sample(memory, processes, tasks);
                if caps.memory_bytes.is_some_and(|cap| memory > cap) {
                    return Some((Limit::Memory, caps.memory_bytes.unwrap_or(0)));
                }
                if caps.processes.is_some_and(|cap| processes > cap) {
                    return Some((Limit::Processes, caps.processes.map(u64::from).unwrap_or(0)));
                }
                if caps.tasks.is_some_and(|cap| tasks > cap) {
                    return Some((Limit::Tasks, caps.tasks.map(u64::from).unwrap_or(0)));
                }
                None
            }
        }
    }

    pub(crate) async fn changed(&mut self) {
        let Some(watch) = &self.watch else {
            return std::future::pending().await;
        };
        let failed = match watch.readable().await {
            Ok(mut guard) => {
                // Drain the events; what changed is read from the files themselves.
                let mut buf = [0_u8; 1024];
                while rustix::io::read(watch.get_ref(), &mut buf[..]).is_ok_and(|n| n > 0) {}
                guard.clear_ready();
                false
            }
            Err(_) => true,
        };
        if failed {
            self.watch = None;
            std::future::pending::<()>().await;
        }
    }

    pub(crate) fn kill(&mut self) {
        match &self.os.kind {
            Kind::Cgroup(cgroup) => cgroup.kill(),
            Kind::Fallback { pgid } => {
                // Called only while the leader is unreaped: the group id is still its own.
                if let Some(pgid) = Pid::from_raw(*pgid) {
                    let _ = rustix::process::kill_process_group(pgid, Signal::KILL);
                }
            }
        }
    }

    pub(crate) fn after_exit(&mut self) -> Option<(Limit, u64)> {
        let Kind::Cgroup(cgroup) = &self.os.kind else {
            return None;
        };
        let _ = self.os.usage();
        let caps = self.os.caps;
        let memory = cgroup.read("memory.events").unwrap_or_default();
        if field(&memory, "oom_kill").unwrap_or(0) > 0 {
            // The unit's own limit counts in its `oom`; an ancestor's (the session or the total)
            // does not.
            return Some(if field(&memory, "oom").unwrap_or(0) > 0 {
                (Limit::Memory, caps.memory_bytes.unwrap_or(0))
            } else if self.os.in_session {
                (Limit::SessionMemory, 0)
            } else {
                (Limit::TotalMemory, 0)
            });
        }
        let refused = cgroup
            .read("pids.events")
            .ok()
            .and_then(|text| field(&text, "max"))
            .unwrap_or(0);
        (refused > 0).then(|| (Limit::Tasks, caps.tasks.map(u64::from).unwrap_or(0)))
    }

    pub(crate) async fn kill_remaining(&mut self) {
        match &self.os.kind {
            Kind::Cgroup(cgroup) => {
                cgroup.kill();
                for _ in 0..100 {
                    if cgroup.procs().is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            Kind::Fallback { pgid } => {
                // The leader is reaped, so the group id could in principle be reused: signal each
                // member found now whose group is still this one.
                for member in group_members(*pgid) {
                    if let Some(pid) = Pid::from_raw(member.pid)
                        && rustix::process::getpgid(Some(pid)).ok() == Pid::from_raw(*pgid)
                    {
                        let _ = rustix::process::kill_process(pid, Signal::KILL);
                    }
                }
            }
        }
    }
}

struct Member {
    pid: i32,
    pss: u64,
    threads: u32,
}

/// The processes of group `pgid`, from `/proc/<pid>/stat`, with their PSS and threads.
fn group_members(pgid: i32) -> Vec<Member> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // Fields after the command name (which may hold spaces): state, ppid, pgrp, ...
            let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
            if fields.get(2)?.parse::<i32>().ok()? != pgid {
                return None;
            }
            // `num_threads` is the stat file's 20th field, the 18th after the name.
            let threads = fields.get(17)?.parse().ok()?;
            let pss = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find_map(|line| line.strip_prefix("Pss:"))
                        .and_then(|rest| {
                            rest.trim()
                                .trim_end_matches("kB")
                                .trim()
                                .parse::<u64>()
                                .ok()
                        })
                })
                .unwrap_or(0)
                * 1024;
            Some(Member { pid, pss, threads })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_cleaned() {
        assert_eq!(session_dir("a/../b c"), "total/s-a____b_c");
    }

    #[test]
    fn this_process_is_found_in_its_group() {
        let pgid = rustix::process::getpgid(None)
            .unwrap()
            .as_raw_nonzero()
            .get();
        let me = i32::try_from(std::process::id()).unwrap();
        let members = group_members(pgid);
        let own = members.iter().find(|member| member.pid == me).unwrap();
        assert!(own.threads >= 1);
        assert!(own.pss > 0);
    }
}
