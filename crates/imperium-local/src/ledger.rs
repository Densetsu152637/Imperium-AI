//! A ledger of the server processes (`llama-server`, `vllm`) a daemon started.
//!
//! Servers end with the daemon when it stops gracefully (they are killed on drop, and the
//! daemon stops them at shutdown). A daemon that is killed outright, or crashes, cannot do that,
//! and on Unix a child does not follow its parent down: its servers would run on, holding the
//! GPU, until the next daemon starts. So every server's process id, start time and name is
//! written here when it starts, and the next daemon sweeps what is still running under the
//! same id, start time and name before it loads anything of its own.

use std::path::{Path, PathBuf};

const FILE: &str = "children.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    pid: u32,
    started: u64,
    name: String,
}

fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            Some(Entry {
                pid: parts.next()?.parse().ok()?,
                started: parts.next()?.parse().ok()?,
                name: parts.next()?.to_string(),
            })
        })
        .collect()
}

fn read(dir: &Path) -> Vec<Entry> {
    std::fs::read_to_string(dir.join(FILE)).map_or_else(|_| Vec::new(), |text| parse(&text))
}

fn write(dir: &Path, entries: &[Entry]) {
    let text: String = entries
        .iter()
        .map(|entry| format!("{} {} {}\n", entry.pid, entry.started, entry.name))
        .collect();
    let path: PathBuf = dir.join(FILE);
    let _ = if text.is_empty() {
        std::fs::remove_file(&path)
    } else {
        std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, text))
    };
}

/// What the process table says about `pid`: its start time and name, or `None` when it is gone.
fn identity(system: &mut sysinfo::System, pid: u32) -> Option<(u64, String)> {
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    let process = system.process(pid)?;
    if matches!(
        process.status(),
        sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
    ) {
        return None;
    }
    Some((
        process.start_time(),
        process.name().to_string_lossy().replace(' ', "_"),
    ))
}

fn is_running(system: &mut sysinfo::System, entry: &Entry) -> bool {
    identity(system, entry.pid)
        .is_some_and(|(started, name)| started == entry.started && name == entry.name)
}

/// Writes down a server the daemon just started, and forgets the ones that have ended.
pub fn record(dir: &Path, pid: u32) {
    let mut system = sysinfo::System::new();
    let Some((started, name)) = identity(&mut system, pid) else {
        return;
    };
    let mut entries: Vec<Entry> = read(dir)
        .into_iter()
        .filter(|entry| is_running(&mut system, entry))
        .collect();
    entries.push(Entry { pid, started, name });
    write(dir, &entries);
}

/// Ends the servers an earlier daemon left running, and empties the ledger. Returns how many
/// were ended. Only a process with the recorded id, start time and name is touched.
pub fn sweep(dir: &Path) -> usize {
    let mut system = sysinfo::System::new();
    let mut ended = 0;
    for entry in read(dir) {
        if !is_running(&mut system, &entry) {
            continue;
        }
        let pid = sysinfo::Pid::from_u32(entry.pid);
        if let Some(process) = system.process(pid) {
            tracing::warn!(pid = entry.pid, name = %entry.name, "ending a server an earlier daemon left running");
            process.kill();
            ended += 1;
        }
    }
    write(dir, &[]);
    ended
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    fn child() -> std::process::Child {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("ping");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("a child process")
    }

    #[test]
    fn a_server_left_running_is_ended_by_the_next_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let mut left = child();
        std::thread::sleep(std::time::Duration::from_millis(300));
        record(dir.path(), left.id());
        assert_eq!(read(dir.path()).len(), 1);

        assert_eq!(sweep(dir.path()), 1);
        assert!(left.wait().is_ok(), "the server was ended");
        assert!(!dir.path().join(FILE).exists(), "the ledger is empty");
    }

    #[test]
    fn a_reused_process_id_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut other = child();
        std::thread::sleep(std::time::Duration::from_millis(300));
        // The ledger names this id with another start time: the process is not that server.
        write(
            dir.path(),
            &[Entry {
                pid: other.id(),
                started: 1,
                name: "ping".into(),
            }],
        );
        assert_eq!(sweep(dir.path()), 0);
        assert!(other.try_wait().unwrap().is_none(), "still running");
        let _ = other.kill();
        let _ = other.wait();
    }
}
