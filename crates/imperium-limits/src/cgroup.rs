//! Linux cgroup v2: the daemon's delegated subtree and the units in it (plan L3).
//!
//! Every operation works on an opened directory, with every later file opened relative to it,
//! and reads a written value back (M5): a path swapped between a check and a write cannot
//! redirect it. Nothing here needs `unsafe`: `rustix` wraps the calls.
//!
//! Two users:
//!
//! - [`Tree`]: the units of a [`crate::Limits`] under a given root, laid out as
//!   `<root>/total/{s-<session>/,}<unit>` (terminal and model units directly under `<root>`).
//! - [`Cgroups`]: the code-mode worker's cgroup (#328, moved here unchanged in behaviour): the
//!   daemon moves itself into a leaf (`imperium-daemon`) and puts each worker in its own child
//!   with `memory.max` and no swap.

use std::{
    io::{self, Read as _, Write as _},
    os::fd::{AsFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use rustix::fs::{AtFlags, Mode, OFlags};

use crate::Caps;

/// `CGROUP2_SUPER_MAGIC`.
const CGROUP2_MAGIC: i64 = 0x6367_7270;

/// The leaf the daemon moves itself into.
const DAEMON_LEAF: &str = "imperium-daemon";

/// The prefix of each code-mode worker's cgroup.
const WORKER_PREFIX: &str = "imperium-script-";

/// The prefix of each unit's cgroup.
const UNIT_PREFIX: &str = "u-";

/// `swap.max` and the other files that only exist with their accounting turned on.
fn absent(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::NotFound
}

fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    Ok(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

fn open_dir_at(dir: impl AsFd, name: &str) -> io::Result<OwnedFd> {
    Ok(rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?)
}

/// Reads `name` in `dir`.
pub(crate) fn read_at(dir: impl AsFd, name: &str) -> io::Result<String> {
    let fd = rustix::fs::openat(dir, name, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())?;
    let mut text = String::new();
    std::fs::File::from(fd).read_to_string(&mut text)?;
    Ok(text)
}

/// Writes `value` to `name` in `dir` in one `write`.
pub(crate) fn write_at(dir: impl AsFd, name: &str, value: &str) -> io::Result<()> {
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o644),
    )?;
    std::fs::File::from(fd).write_all(value.as_bytes())
}

/// Writes `value` and reads it back; a cap the kernel did not take is an error, not a no-op.
fn set_at(dir: impl AsFd, name: &str, value: &str) -> io::Result<()> {
    write_at(&dir, name, value)?;
    let back = read_at(&dir, name)?;
    if back.trim() == value {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{name} reads {:?} after writing {value:?}",
            back.trim()
        )))
    }
}

fn mkdir_at(dir: impl AsFd, name: &str) -> io::Result<()> {
    match rustix::fs::mkdirat(dir, name, Mode::from_raw_mode(0o755)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn rmdir_at(dir: impl AsFd, name: &str) -> io::Result<()> {
    Ok(rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR)?)
}

fn is_cgroup2(fs: &rustix::fs::StatFs) -> bool {
    // The field's type differs between architectures.
    #[allow(clippy::unnecessary_cast, clippy::cast_lossless)]
    let kind = fs.f_type as i64;
    kind == CGROUP2_MAGIC
}

fn has(list: &str, name: &str) -> bool {
    list.split_whitespace().any(|item| item == name)
}

/// A value of `field` in a flat-keyed file (`memory.events`, `pids.events`, `cpu.stat`).
pub(crate) fn field(text: &str, field: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(' ')?;
        (key == field).then(|| value.trim().parse().ok()).flatten()
    })
}

/// Creates `<parent>/<name>` for a [`Tree`] root, after turning on the `memory`, `pids` and
/// (where delegated) `cpu` controllers for `parent`'s children. For a delegated `parent` that
/// holds no process itself (CI's subtree, the daemon's scope above its leaf).
///
/// # Errors
/// When `parent` cannot be opened or the directory cannot be created.
pub fn prepare_child(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = open_dir(parent)?;
    let available = read_at(&dir, "cgroup.controllers")?;
    for controller in ["memory", "pids", "cpu"] {
        if has(&available, controller) {
            // Already on, or refused (no-internal-process rule): the child's own check reports it.
            let _ = write_at(&dir, "cgroup.subtree_control", &format!("+{controller}"));
        }
    }
    mkdir_at(&dir, name)?;
    Ok(parent.join(name))
}

/// The units of one [`crate::Limits`] under a delegated root.
#[derive(Debug)]
pub(crate) struct Tree {
    root: PathBuf,
    dir: OwnedFd,
    /// Whether the `cpu` controller reaches the units (CPU caps are enforced).
    pub(crate) cpu: bool,
}

/// A unique suffix for unit names.
static NEXT_UNIT: AtomicU64 = AtomicU64::new(1);

impl Tree {
    /// Opens `root` after the checks of M5: a cgroup2 filesystem, owned by the daemon's user,
    /// a daemon not running as root, and `memory` and `pids` available. Turns on the controllers
    /// for its children and creates `total` with the total caps.
    pub(crate) fn open(root: &Path, total: &Caps) -> Result<Self, String> {
        let fail = |what: &str, err: io::Error| format!("{}: {what}: {err}", root.display());
        let dir = open_dir(root).map_err(|err| fail("open", err))?;
        let fs = rustix::fs::fstatfs(&dir).map_err(|err| fail("statfs", err.into()))?;
        if !is_cgroup2(&fs) {
            return Err(format!("{} is not on a cgroup2 filesystem", root.display()));
        }
        let uid = rustix::process::geteuid();
        if uid.is_root() {
            return Err("a daemon running as root does not use a cgroup subtree".to_owned());
        }
        let owner = rustix::fs::fstat(&dir)
            .map_err(|err| fail("stat", err.into()))?
            .st_uid;
        if owner != uid.as_raw() {
            return Err(format!(
                "{} is not delegated to the daemon's user",
                root.display()
            ));
        }
        let controllers = read_at(&dir, "cgroup.controllers").map_err(|err| fail("read", err))?;
        for needed in ["memory", "pids"] {
            if !has(&controllers, needed) {
                return Err(format!("the {needed} controller is not delegated"));
            }
        }
        let tree = Self {
            root: root.to_owned(),
            dir,
            cpu: has(&controllers, "cpu"),
        };
        tree.enable(".")?;
        tree.group("total", total)?;
        Ok(tree)
    }

    /// Turns on the controllers for the children of `rel` (relative to the root).
    fn enable(&self, rel: &str) -> Result<(), String> {
        let dir = if rel == "." {
            rustix::io::dup(&self.dir).map_err(|err| err.to_string())?
        } else {
            open_dir_at(&self.dir, rel).map_err(|err| format!("{rel}: {err}"))?
        };
        let mut wanted = vec!["memory", "pids"];
        if self.cpu {
            wanted.push("cpu");
        }
        for controller in wanted {
            write_at(&dir, "cgroup.subtree_control", &format!("+{controller}"))
                .map_err(|err| format!("{rel}: +{controller}: {err}"))?;
        }
        let enabled = read_at(&dir, "cgroup.subtree_control").map_err(|err| err.to_string())?;
        if !has(&enabled, "memory") || !has(&enabled, "pids") {
            return Err(format!("{rel}: the controllers did not turn on"));
        }
        Ok(())
    }

    /// Creates (or updates) a group, `total` or `total/s-<session>`, with its caps.
    pub(crate) fn group(&self, rel: &str, caps: &Caps) -> Result<(), String> {
        let (parent, name) = rel.rsplit_once('/').unwrap_or((".", rel));
        let parent_dir = if parent == "." {
            rustix::io::dup(&self.dir).map_err(|err| err.to_string())?
        } else {
            open_dir_at(&self.dir, parent).map_err(|err| format!("{parent}: {err}"))?
        };
        mkdir_at(&parent_dir, name).map_err(|err| format!("{rel}: {err}"))?;
        let dir = open_dir_at(&parent_dir, name).map_err(|err| format!("{rel}: {err}"))?;
        write_caps(&dir, caps, false, self.cpu).map_err(|err| format!("{rel}: {err}"))?;
        self.enable(rel)
    }

    /// Creates a unit's cgroup under `parent` (relative to the root; `.` for the root itself).
    pub(crate) fn unit(
        &self,
        parent: &str,
        class: &str,
        caps: &Caps,
    ) -> Result<UnitCgroup, String> {
        let parent_dir = if parent == "." {
            rustix::io::dup(&self.dir).map_err(|err| err.to_string())?
        } else {
            open_dir_at(&self.dir, parent).map_err(|err| format!("{parent}: {err}"))?
        };
        sweep(&parent_dir, UNIT_PREFIX);
        let name = format!(
            "{UNIT_PREFIX}{class}-{}-{}",
            std::process::id(),
            NEXT_UNIT.fetch_add(1, Ordering::Relaxed)
        );
        mkdir_at(&parent_dir, &name).map_err(|err| format!("{name}: {err}"))?;
        let path = if parent == "." {
            self.root.join(&name)
        } else {
            self.root.join(parent).join(&name)
        };
        let unit = UnitCgroup {
            path,
            dir: open_dir_at(&parent_dir, &name).map_err(|err| format!("{name}: {err}"))?,
            parent: parent_dir,
            name,
        };
        write_caps(&unit.dir, caps, true, self.cpu)
            .map_err(|err| format!("{}: {err}", unit.name))?;
        Ok(unit)
    }
}

/// Writes a group's or unit's caps. A unit also gets no swap and `memory.oom.group`, so the
/// kernel stops it whole rather than one process of it.
fn write_caps(dir: &OwnedFd, caps: &Caps, unit: bool, cpu: bool) -> io::Result<()> {
    if let Some(bytes) = caps.memory_bytes {
        // The kernel keeps whole pages; caps are whole MiB, so the value reads back unchanged.
        set_at(dir, "memory.max", &bytes.to_string())?;
        if unit {
            match write_at(dir, "memory.swap.max", "0") {
                Err(err) if !absent(&err) => return Err(err),
                _ => {}
            }
        }
    }
    if unit {
        set_at(dir, "memory.oom.group", "1")?;
    }
    if let Some(tasks) = caps.tasks {
        set_at(dir, "pids.max", &tasks.to_string())?;
    }
    if let (Some(percent), true) = (caps.cpu_percent, cpu) {
        set_at(dir, "cpu.max", &cpu_max(percent))?;
    }
    Ok(())
}

/// `cpu.max` for `percent` of the whole machine: a quota of that share of every CPU per period.
pub(crate) fn cpu_max(percent: u16) -> String {
    const PERIOD: u64 = 100_000;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
    let quota = PERIOD * cpus * u64::from(percent.clamp(1, 100)) / 100;
    format!("{quota} {PERIOD}")
}

/// Removes the empty cgroups named `prefix*` under `dir` (units whose processes are gone); one
/// that still has a process refuses removal and stays.
fn sweep(dir: &OwnedFd, prefix: &str) {
    let Ok(mut entries) = rustix::fs::Dir::read_from(dir) else {
        return;
    };
    let mut names = Vec::new();
    while let Some(Ok(entry)) = entries.next() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) {
            names.push(name);
        }
    }
    for name in names {
        let _ = rmdir_at(dir, &name);
    }
}

/// One unit's cgroup. Removed (best effort) when dropped; a busy one is swept later.
#[derive(Debug)]
pub(crate) struct UnitCgroup {
    /// The absolute path, which the launcher joins.
    pub(crate) path: PathBuf,
    dir: OwnedFd,
    parent: OwnedFd,
    name: String,
}

impl UnitCgroup {
    pub(crate) fn read(&self, name: &str) -> io::Result<String> {
        read_at(&self.dir, name)
    }

    /// The process ids in the unit.
    pub(crate) fn procs(&self) -> Vec<i32> {
        self.read("cgroup.procs")
            .map(|text| {
                text.lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Kills every process in the unit: `cgroup.kill` (kernel 5.14 or later), else freeze, kill
    /// each listed process and thaw. Frozen processes cannot exit, so their ids are not reused
    /// between the listing and the signal.
    pub(crate) fn kill(&self) {
        match write_at(&self.dir, "cgroup.kill", "1") {
            Err(err)
                if absent(&err)
                    || err.raw_os_error() == Some(rustix::io::Errno::INVAL.raw_os_error()) =>
            {
                let _ = write_at(&self.dir, "cgroup.freeze", "1");
                for pid in self.procs() {
                    if let Some(pid) = rustix::process::Pid::from_raw(pid) {
                        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                    }
                }
                let _ = write_at(&self.dir, "cgroup.freeze", "0");
            }
            _ => {}
        }
    }

    /// An inotify watch on the unit's `memory.events` and `pids.events`, non-blocking.
    pub(crate) fn watch(&self) -> io::Result<OwnedFd> {
        use rustix::fs::inotify;
        let fd = inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC)?;
        for file in ["memory.events", "pids.events"] {
            inotify::add_watch(&fd, self.path.join(file), inotify::WatchFlags::MODIFY)?;
        }
        Ok(fd)
    }
}

impl Drop for UnitCgroup {
    fn drop(&mut self) {
        let _ = rmdir_at(&self.parent, &self.name);
    }
}

/// The daemon's delegated cgroup for code-mode workers (#328), set up once.
#[derive(Debug)]
pub struct Cgroups {
    base: PathBuf,
    dir: OwnedFd,
}

/// The daemon's delegated cgroup, if it has one, set up on first use.
pub fn delegated() -> Option<&'static Cgroups> {
    static SETUP: OnceLock<Option<Cgroups>> = OnceLock::new();
    SETUP
        .get_or_init(|| {
            let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
            let uid = rustix::process::geteuid().as_raw();
            let mount = Path::new("/sys/fs/cgroup");
            let fs = rustix::fs::statfs(mount).ok()?;
            if !is_cgroup2(&fs) {
                return None;
            }
            Cgroups::set_up(mount, &own, std::process::id(), uid).ok()
        })
        .as_ref()
}

impl Cgroups {
    /// Sets up the daemon's cgroup under `mount` (the cgroup2 mount), given the text of
    /// `/proc/self/cgroup`, the daemon's process id and its effective user id.
    ///
    /// # Errors
    /// Not cgroup v2, a daemon running as root, a cgroup not owned by its user (not delegated),
    /// no memory controller, or a write that did not take.
    pub fn set_up(mount: &Path, own: &str, pid: u32, uid: u32) -> Result<Self, String> {
        let path = own
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or("the daemon is not in a cgroup v2 hierarchy")?
            .trim()
            .trim_start_matches('/');
        let mut base = mount.join(path);
        // A daemon already in its leaf (moved there by an earlier worker of this process).
        if base.file_name().is_some_and(|name| name == DAEMON_LEAF) {
            base.pop();
        }
        if uid == 0 {
            return Err("a daemon running as root does not use cgroups for workers".to_owned());
        }
        let fail = |err: io::Error| format!("{}: {err}", base.display());
        let dir = open_dir(&base).map_err(fail)?;
        let owner = rustix::fs::fstat(&dir)
            .map_err(|err| fail(err.into()))?
            .st_uid;
        if owner != uid {
            return Err(format!(
                "{} is not delegated to the daemon's user",
                base.display()
            ));
        }
        let controllers = read_at(&dir, "cgroup.controllers").map_err(fail)?;
        if !has(&controllers, "memory") {
            return Err("the memory controller is not available".to_owned());
        }
        mkdir_at(&dir, DAEMON_LEAF).map_err(fail)?;
        let leaf = open_dir_at(&dir, DAEMON_LEAF).map_err(fail)?;
        write_at(&leaf, "cgroup.procs", &pid.to_string()).map_err(fail)?;
        let enabled = read_at(&dir, "cgroup.subtree_control").unwrap_or_default();
        if !has(&enabled, "memory") {
            write_at(&dir, "cgroup.subtree_control", "+memory").map_err(fail)?;
        }
        Ok(Self { base, dir })
    }

    /// Puts the worker `pid` in a cgroup of its own with `memory.max` at `bytes` and no swap.
    /// The cgroup is removed when the returned lease is dropped after the worker is gone.
    ///
    /// # Errors
    /// When the cgroup cannot be created or a value does not take.
    pub fn place(&self, pid: u32, bytes: u64) -> Result<Lease, String> {
        sweep(&self.dir, WORKER_PREFIX);
        let name = format!("{WORKER_PREFIX}{pid}");
        let fail = |err: io::Error| format!("{}: {err}", self.base.join(&name).display());
        mkdir_at(&self.dir, &name).map_err(fail)?;
        let lease = Lease {
            parent: rustix::io::dup(&self.dir).map_err(|err| fail(err.into()))?,
            name: name.clone(),
        };
        let dir = open_dir_at(&self.dir, &name).map_err(fail)?;
        write_at(&dir, "memory.max", &bytes.to_string()).map_err(fail)?;
        // Without swap accounting the file is absent; the memory limit still holds.
        let _ = write_at(&dir, "memory.swap.max", "0");
        write_at(&dir, "cgroup.procs", &pid.to_string()).map_err(fail)?;
        Ok(lease)
    }

    /// The cgroup the daemon's leaf and the workers sit in.
    #[must_use]
    pub fn base(&self) -> &Path {
        &self.base
    }
}

/// A worker's cgroup, removed (best effort) when dropped.
#[derive(Debug)]
pub struct Lease {
    parent: OwnedFd,
    name: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = rmdir_at(&self.parent, &self.name);
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::MetadataExt as _};

    use super::*;

    /// A cgroup2 mount stand-in: `<mount>/user/app` with the memory controller available.
    fn fake_mount() -> (tempfile::TempDir, PathBuf) {
        let mount = tempfile::tempdir().unwrap();
        let base = mount.path().join("user/app");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("cgroup.controllers"), "cpu io memory pids\n").unwrap();
        fs::write(base.join("cgroup.subtree_control"), "").unwrap();
        (mount, base)
    }

    fn own_uid(path: &Path) -> u32 {
        fs::metadata(path).unwrap().uid()
    }

    #[test]
    fn a_delegated_cgroup_gets_a_daemon_leaf_and_the_memory_controller() {
        let (mount, base) = fake_mount();
        let uid = own_uid(&base);
        let cgroups = Cgroups::set_up(mount.path(), "0::/user/app\n", 4242, uid).unwrap();
        assert_eq!(
            fs::read_to_string(base.join("imperium-daemon/cgroup.procs")).unwrap(),
            "4242"
        );
        assert_eq!(
            fs::read_to_string(base.join("cgroup.subtree_control")).unwrap(),
            "+memory"
        );

        let lease = cgroups.place(777, 512 * 1024 * 1024).unwrap();
        let dir = base.join("imperium-script-777");
        assert_eq!(
            fs::read_to_string(dir.join("memory.max")).unwrap(),
            (512_u64 * 1024 * 1024).to_string()
        );
        assert_eq!(
            fs::read_to_string(dir.join("memory.swap.max")).unwrap(),
            "0"
        );
        assert_eq!(fs::read_to_string(dir.join("cgroup.procs")).unwrap(), "777");
        // A real cgroup directory empties itself; the stand-in needs its files removed first.
        for file in ["memory.max", "memory.swap.max", "cgroup.procs"] {
            fs::remove_file(dir.join(file)).unwrap();
        }
        drop(lease);
        assert!(!dir.exists(), "the worker's cgroup outlived it");
    }

    #[test]
    fn cgroups_that_are_not_delegated_or_lack_memory_are_skipped() {
        let (mount, base) = fake_mount();
        let uid = own_uid(&base);
        // Not cgroup v2.
        assert!(Cgroups::set_up(mount.path(), "1:memory:/user/app\n", 1, uid).is_err());
        // Owned by someone else.
        assert!(Cgroups::set_up(mount.path(), "0::/user/app\n", 1, uid + 1).is_err());
        // Root never uses them.
        assert!(Cgroups::set_up(mount.path(), "0::/user/app\n", 1, 0).is_err());
        // No memory controller.
        fs::write(base.join("cgroup.controllers"), "cpu pids\n").unwrap();
        assert!(Cgroups::set_up(mount.path(), "0::/user/app\n", 1, uid).is_err());
        assert!(!base.join("imperium-daemon").exists());
    }

    #[test]
    fn a_daemon_already_in_its_leaf_uses_the_cgroup_above() {
        let (mount, base) = fake_mount();
        let uid = own_uid(&base);
        fs::write(base.join("cgroup.subtree_control"), "memory\n").unwrap();
        let cgroups =
            Cgroups::set_up(mount.path(), "0::/user/app/imperium-daemon\n", 9, uid).unwrap();
        assert_eq!(cgroups.base, base);
        // The controller was already on; nothing was rewritten.
        assert_eq!(
            fs::read_to_string(base.join("cgroup.subtree_control")).unwrap(),
            "memory\n"
        );
    }

    #[test]
    fn a_tree_refuses_a_root_that_is_not_cgroup2() {
        let dir = tempfile::tempdir().unwrap();
        let err = Tree::open(dir.path(), &Caps::default()).unwrap_err();
        assert!(err.contains("not on a cgroup2 filesystem"), "{err}");
    }

    #[test]
    fn flat_keyed_fields_and_cpu_max() {
        let events = "low 0\nhigh 0\nmax 3\noom 1\noom_kill 1\n";
        assert_eq!(field(events, "max"), Some(3));
        assert_eq!(field(events, "oom_kill"), Some(1));
        assert_eq!(field(events, "missing"), None);
        let max = cpu_max(25);
        let (quota, period) = max.split_once(' ').unwrap();
        assert_eq!(period, "100000");
        let cpus = std::thread::available_parallelism().unwrap().get() as u64;
        assert_eq!(quota.parse::<u64>().unwrap(), 25_000 * cpus);
    }
}
