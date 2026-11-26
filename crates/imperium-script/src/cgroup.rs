//! A cgroup v2 memory limit for workers, when the daemon runs in a delegated cgroup (plan C1).
//!
//! It is an extra behind the worker's own `RLIMIT_DATA` and Monty's memory limit, so anything
//! that does not fit is skipped rather than failed: no cgroup v2, no memory controller, a cgroup
//! the daemon's user does not own (the mark systemd's `Delegate=` leaves), a daemon running as
//! root (which could write cgroups nobody delegated to it), or a cgroup whose controllers cannot
//! be enabled. When it does fit, the first worker moves the daemon into a leaf
//! (`<cgroup>/imperium-daemon`, since a cgroup v2 that hands controllers to its children cannot
//! hold processes itself), enables the memory controller for the children, and every worker then
//! runs in its own child (`<cgroup>/imperium-script-<pid>`) with `memory.max` at twice
//! `script_memory_mb` and no swap. The worker is moved there before its program is sent, so no
//! program runs outside it.

use std::{
    fs,
    io::{self, Write as _},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// The leaf the daemon moves itself into.
const DAEMON_LEAF: &str = "imperium-daemon";

/// The prefix of each worker's cgroup.
const WORKER_PREFIX: &str = "imperium-script-";

/// The daemon's delegated cgroup, set up once.
#[derive(Debug)]
pub(crate) struct Cgroups {
    base: PathBuf,
}

/// The daemon's delegated cgroup, if it has one, set up on first use.
pub(crate) fn delegated() -> Option<&'static Cgroups> {
    static SETUP: OnceLock<Option<Cgroups>> = OnceLock::new();
    SETUP
        .get_or_init(|| {
            let own = fs::read_to_string("/proc/self/cgroup").ok()?;
            let uid = effective_uid()?;
            Cgroups::set_up(Path::new("/sys/fs/cgroup"), &own, std::process::id(), uid).ok()
        })
        .as_ref()
}

/// The effective user id, from `/proc/self/status` (the second number of `Uid:`).
fn effective_uid() -> Option<u32> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

impl Cgroups {
    /// Sets up the daemon's cgroup under `mount` (the cgroup2 mount), given the text of
    /// `/proc/self/cgroup`, the daemon's process id and its effective user id.
    pub(crate) fn set_up(mount: &Path, own: &str, pid: u32, uid: u32) -> Result<Self, String> {
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
        let owner = fs::metadata(&base)
            .map_err(|err| format!("{}: {err}", base.display()))?
            .uid();
        if owner != uid {
            return Err(format!(
                "{} is not delegated to the daemon's user",
                base.display()
            ));
        }
        let controllers = read(&base.join("cgroup.controllers"))?;
        if !controllers.split_whitespace().any(|name| name == "memory") {
            return Err("the memory controller is not available".to_owned());
        }
        let leaf = base.join(DAEMON_LEAF);
        create_dir(&leaf)?;
        write(&leaf.join("cgroup.procs"), &pid.to_string())?;
        let enabled = read(&base.join("cgroup.subtree_control")).unwrap_or_default();
        if !enabled.split_whitespace().any(|name| name == "memory") {
            write(&base.join("cgroup.subtree_control"), "+memory")?;
        }
        Ok(Self { base })
    }

    /// Puts the worker `pid` in a cgroup of its own with `memory.max` at `bytes` and no swap.
    /// The cgroup is removed when the returned lease is dropped after the worker is gone.
    pub(crate) fn place(&self, pid: u32, bytes: u64) -> Result<Lease, String> {
        self.sweep();
        let dir = self.base.join(format!("{WORKER_PREFIX}{pid}"));
        create_dir(&dir)?;
        let lease = Lease(dir.clone());
        write(&dir.join("memory.max"), &bytes.to_string())?;
        // Without swap accounting the file is absent; the memory limit still holds.
        let _ = write(&dir.join("memory.swap.max"), "0");
        write(&dir.join("cgroup.procs"), &pid.to_string())?;
        Ok(lease)
    }

    /// Removes worker cgroups left behind (a worker killed without waiting); a cgroup that still
    /// has a process refuses removal and stays.
    fn sweep(&self) {
        let Ok(entries) = fs::read_dir(&self.base) else {
            return;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(WORKER_PREFIX)
            {
                let _ = fs::remove_dir(entry.path());
            }
        }
    }
}

/// A worker's cgroup, removed (best effort) when dropped.
#[derive(Debug)]
pub(crate) struct Lease(PathBuf);

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))
}

fn write(path: &Path, value: &str) -> Result<(), String> {
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .and_then(|mut file| file.write_all(value.as_bytes()))
        .map_err(|err| format!("{}: {err}", path.display()))
}

fn create_dir(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
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
}
