//! Background commands: `run_command` with `background: true` starts the command under the
//! session's isolation tier and returns an id at once; `command_output` reads what it wrote
//! since the last read, and `command_stop` ends it with every process it started. A session's
//! processes are killed when the session stops or is deleted (and when its table is dropped),
//! at most [`MAX_RUNNING`] run at once, and each keeps only the last [`MAX_BUFFER`] bytes of
//! output.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{oneshot, watch};

use crate::{Sandbox, ToolError, Workspace};

/// Background commands one session may run at once.
pub const MAX_RUNNING: usize = 8;
/// Processes kept in the table, finished ones included; the oldest finished go first.
const MAX_KEPT: usize = 32;
/// Output kept per process, in bytes; older output is dropped.
pub const MAX_BUFFER: usize = 1024 * 1024;
/// Most output returned by one `command_output` call, in bytes.
const MAX_READ: usize = 30_000;
/// How long `command_stop` waits for the process to end.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// A session's background processes. Clones share the table.
#[derive(Debug, Clone, Default)]
pub struct Processes(Arc<Mutex<Table>>);

impl PartialEq for Processes {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Processes {}

#[derive(Debug, Default)]
struct Table {
    next: u64,
    entries: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    id: String,
    command: String,
    started: Instant,
    output: Arc<Mutex<Output>>,
    /// The exit, once the process has ended.
    status: watch::Receiver<Option<String>>,
    /// Sending (or dropping) it tells the waiter the process was stopped.
    kill: Option<oneshot::Sender<()>>,
    /// Ends the command and everything it started.
    killer: Killer,
    /// Whether it was stopped (so nothing it started is left).
    killed: bool,
    /// Where the previous `command_output` call stopped.
    read_to: u64,
}

impl Entry {
    /// Whether the command itself is still running.
    fn running(&self) -> bool {
        self.status.borrow().is_none()
    }

    /// Whether the command or anything it started may still be running (counted against the
    /// cap, since a command that exits after starting children in the background leaves them).
    fn active(&self) -> bool {
        !self.killed && (self.running() || self.killer.alive())
    }

    /// Kills it now; returns the killer to run (it is synchronous).
    fn stop(&mut self) -> Killer {
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
        self.killed = true;
        self.killer.clone()
    }
}

impl Drop for Table {
    /// The session went away: nothing it started may outlive it.
    fn drop(&mut self) {
        for entry in &mut self.entries {
            if entry.active() {
                entry.stop().kill();
            }
        }
    }
}

/// What a process wrote: the last [`MAX_BUFFER`] bytes, and how many came before them.
#[derive(Debug, Default)]
struct Output {
    bytes: VecDeque<u8>,
    dropped: u64,
}

impl Output {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk);
        let excess = self.bytes.len().saturating_sub(MAX_BUFFER);
        if excess > 0 {
            self.bytes.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    fn end(&self) -> u64 {
        self.dropped + self.bytes.len() as u64
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends a background command and everything it started, also after the command itself has
/// exited and left children behind (`sh -c 'x &'`, `start /b x`): on Unix the command leads its
/// own process group, which is killed as a whole; on Windows it runs in a Job Object that kills
/// every process in it when closed. A container-tier command's container is removed by name.
/// Killing is synchronous and may be repeated.
#[derive(Debug, Clone)]
struct Killer(Arc<KillerInner>);

#[derive(Debug)]
struct KillerInner {
    /// The command's process id (its process group's id on Unix).
    pid: Option<u32>,
    /// The job holding the command and its descendants; closing it kills them all.
    #[cfg(windows)]
    job: Mutex<Option<win32job::Job>>,
    /// A container to remove: its runtime and name.
    container: Option<(String, String)>,
}

/// Runs a short helper program and waits for it, ignoring its output.
fn quietly(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

impl Killer {
    fn new(child: &tokio::process::Child, container: Option<(String, String)>) -> Self {
        #[cfg(windows)]
        let job = child.raw_handle().and_then(|handle| {
            let mut limits = win32job::ExtendedLimitInfo::new();
            limits.limit_kill_on_job_close();
            let job = win32job::Job::create_with_limit_info(&limits).ok()?;
            job.assign_process(handle as isize).ok()?;
            Some(job)
        });
        Killer(Arc::new(KillerInner {
            pid: child.id(),
            #[cfg(windows)]
            job: Mutex::new(job),
            container,
        }))
    }

    /// Kills the command, everything it started, and its container.
    fn kill(&self) {
        #[cfg(windows)]
        {
            let job = lock(&self.0.job).take();
            // Kill the tree first: a child started before the command joined its job is not in
            // the job but is still in the tree. With a job, only while the command is in it
            // (so the id is still the command's, not a reused one).
            let ours = job.as_ref().is_none_or(|job| {
                job.query_process_id_list()
                    .is_ok_and(|pids| self.0.pid.is_some_and(|pid| pids.contains(&(pid as usize))))
            });
            if ours && let Some(pid) = self.0.pid {
                quietly("taskkill", &["/PID", &pid.to_string(), "/T", "/F"]);
            }
            // Then close the job, which kills every process in it.
            drop(job);
        }
        #[cfg(not(windows))]
        if let Some(pid) = self.0.pid {
            quietly("kill", &["-KILL", "--", &format!("-{pid}")]);
        }
        if let Some((runtime, name)) = &self.0.container {
            quietly(runtime, &["rm", "-f", name]);
        }
    }

    /// Whether any process the command started may still be alive.
    fn alive(&self) -> bool {
        #[cfg(windows)]
        {
            lock(&self.0.job).as_ref().is_some_and(|job| {
                job.query_process_id_list()
                    .is_ok_and(|pids| !pids.is_empty())
            })
        }
        #[cfg(not(windows))]
        {
            self.0
                .pid
                .is_some_and(|pid| quietly("kill", &["-0", "--", &format!("-{pid}")]))
        }
    }

    /// The ids of the processes still alive, for tests.
    #[cfg(test)]
    fn members(&self) -> Vec<u32> {
        #[cfg(windows)]
        {
            lock(&self.0.job)
                .as_ref()
                .and_then(|job| job.query_process_id_list().ok())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|pid| u32::try_from(pid).ok())
                .collect()
        }
        #[cfg(not(windows))]
        {
            if self.alive() {
                self.0.pid.into_iter().collect()
            } else {
                Vec::new()
            }
        }
    }
}

/// Runs `killers` off the async runtime's threads when there is one, else here.
fn kill_all(killers: Vec<Killer>) {
    if killers.is_empty() {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn_blocking(move || killers.iter().for_each(Killer::kill));
        }
        Err(_) => killers.iter().for_each(Killer::kill),
    }
}

/// A process name unlikely to be used by anything else, for a container-tier command.
fn container_name(id: &str) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("imperium-bg-{}-{nanos:x}-{serial}-{id}", std::process::id())
}

async fn pump(mut pipe: impl AsyncRead + Unpin, output: Arc<Mutex<Output>>) {
    let mut chunk = vec![0u8; 8192];
    while let Ok(read) = pipe.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        lock(&output).push(&chunk[..read]);
    }
}

impl Processes {
    /// Starts `command` in the background under the workspace's tier and returns its id.
    pub fn start(&self, workspace: &Workspace, command: &str) -> Result<String, ToolError> {
        if workspace.runtime().is_some() {
            return Err(ToolError::Invalid(
                "background commands are not available in remote runtimes yet; run it in the \
                 foreground or start it detached with the runtime's own tools"
                    .into(),
            ));
        }
        let mut table = lock(&self.0);
        let running = table.entries.iter().filter(|entry| entry.active()).count();
        if running >= MAX_RUNNING {
            return Err(ToolError::Invalid(format!(
                "{MAX_RUNNING} background commands are already running; stop one with \
                 command_stop first"
            )));
        }
        table.next += 1;
        let id = format!("p{}", table.next);
        // Unique across sessions and daemons sharing the container runtime.
        let container = container_name(&id);
        let roots = workspace.command_roots();
        let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
        let mut process = workspace.sandbox().named_command(
            &roots,
            workspace.grants(),
            command,
            Some(&container),
        )?;
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process
            .spawn()
            .map_err(|error| ToolError::Io(format!("could not start the command: {error}")))?;
        let killer = Killer::new(
            &child,
            match workspace.sandbox() {
                Sandbox::Container { runtime, .. } => Some((runtime.clone(), container)),
                _ => None,
            },
        );
        let waiter_killer = killer.clone();
        let output = Arc::new(Mutex::new(Output::default()));
        let mut pumps = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            pumps.push(tokio::spawn(pump(stdout, Arc::clone(&output))));
        }
        if let Some(stderr) = child.stderr.take() {
            pumps.push(tokio::spawn(pump(stderr, Arc::clone(&output))));
        }
        let (status_tx, status) = watch::channel(None);
        let (kill, killed) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let ended = tokio::select! {
                status = child.wait() => match status {
                    Ok(status) => match status.code() {
                        Some(code) => format!("exited with code {code}"),
                        None => "terminated by a signal".to_string(),
                    },
                    Err(error) => format!("failed: {error}"),
                },
                // Asked to stop, or the table (and with it the session) went away.
                _ = killed => {
                    let _ = tokio::task::spawn_blocking(move || waiter_killer.kill()).await;
                    let _ = child.kill().await;
                    "stopped".to_string()
                }
            };
            // Let the pipes drain what is left (they close once every process holding them
            // has ended).
            for pump in pumps {
                let _ = tokio::time::timeout(Duration::from_secs(2), pump).await;
            }
            let _ = status_tx.send(Some(ended));
        });
        // Make room: drop the oldest finished processes beyond the limit, only those with
        // nothing left running (dropping one loses its killer).
        while table.entries.len() >= MAX_KEPT {
            match table.entries.iter().position(|entry| !entry.active()) {
                Some(index) => {
                    table.entries.remove(index);
                }
                None => break,
            }
        }
        table.entries.push(Entry {
            id: id.clone(),
            command: command.to_string(),
            started: Instant::now(),
            output,
            status,
            kill: Some(kill),
            killer,
            killed: false,
            read_to: 0,
        });
        Ok(id)
    }

    /// The process's output from byte `since` (by default where the last read stopped), at most
    /// [`MAX_READ`] bytes, under a line saying whether it is still running.
    pub fn output(&self, id: &str, since: Option<u64>) -> Result<String, ToolError> {
        let mut table = lock(&self.0);
        let entry = find(&mut table, id)?;
        let status = entry.status.borrow().clone();
        let output = lock(&entry.output);
        let end = output.end();
        let from = since.unwrap_or(entry.read_to).min(end);
        let mut text = String::new();
        if from < output.dropped {
            text.push_str(&format!(
                "[{} bytes before this were dropped: only the last {MAX_BUFFER} are kept]\n",
                output.dropped - from
            ));
        }
        let start = from.max(output.dropped);
        let offset = (start - output.dropped) as usize;
        let take = output.bytes.len().saturating_sub(offset).min(MAX_READ);
        let bytes: Vec<u8> = output.bytes.range(offset..offset + take).copied().collect();
        let to = start + take as u64;
        drop(output);
        entry.read_to = to;
        let state = match status {
            Some(ended) if !entry.killed && entry.killer.alive() => {
                format!("{ended}; processes it started are still running (command_stop ends them)")
            }
            Some(ended) => ended,
            None => format!("running for {} s", entry.started.elapsed().as_secs()),
        };
        let mut out = format!(
            "process {id} (`{}`): {state}\n--- output (bytes {start}..{to}) ---\n",
            entry.command
        );
        out.push_str(&text);
        out.push_str(&String::from_utf8_lossy(&bytes));
        if to < end {
            out.push_str(&format!(
                "\n[{} more bytes: call command_output again (since={to})]",
                end - to
            ));
        } else if bytes.is_empty() {
            out.push_str("[no new output]");
        }
        Ok(out)
    }

    /// Stops the process and everything it started, and waits briefly for it to end.
    pub async fn stop(&self, id: &str) -> Result<String, ToolError> {
        let (mut status, killer) = {
            let mut table = lock(&self.0);
            let entry = find(&mut table, id)?;
            // Kill whenever anything may be left: the command may have exited and left
            // children running.
            let killer = entry.active().then(|| entry.stop());
            (entry.status.clone(), killer)
        };
        if let Some(killer) = killer {
            let _ = tokio::task::spawn_blocking(move || killer.kill()).await;
        }
        let ended = tokio::time::timeout(STOP_WAIT, status.wait_for(Option::is_some))
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|ended| ended.clone());
        Ok(match ended {
            Some(ended) => format!("process {id}: {ended}"),
            None => format!("process {id}: asked to stop; it has not ended yet"),
        })
    }

    /// The killers of every command that may still have something running, marked stopped.
    fn stop_entries(&self) -> Vec<Killer> {
        let mut table = lock(&self.0);
        table
            .entries
            .iter_mut()
            .filter(|entry| entry.active())
            .map(Entry::stop)
            .collect()
    }

    /// Kills every command and everything it started (the session stopped or was deleted).
    /// Does not wait for the kills to finish.
    pub fn stop_all(&self) {
        kill_all(self.stop_entries());
    }

    /// Like [`Processes::stop_all`], waiting up to `timeout` for the kills to finish (the
    /// daemon is shutting down).
    pub async fn stop_all_and_wait(&self, timeout: Duration) {
        let killers = self.stop_entries();
        if killers.is_empty() {
            return;
        }
        let kills = tokio::task::spawn_blocking(move || killers.iter().for_each(Killer::kill));
        let _ = tokio::time::timeout(timeout, kills).await;
    }

    /// How many commands are still running, counting those that exited but left processes
    /// they started running.
    pub fn running(&self) -> usize {
        lock(&self.0)
            .entries
            .iter()
            .filter(|entry| entry.active())
            .count()
    }
}

fn find<'a>(table: &'a mut Table, id: &str) -> Result<&'a mut Entry, ToolError> {
    let id = id.trim();
    let known: Vec<String> = table.entries.iter().map(|entry| entry.id.clone()).collect();
    table
        .entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or_else(|| {
            ToolError::Invalid(if known.is_empty() {
                format!("no background process {id:?}; none has been started")
            } else {
                format!("no background process {id:?}; known: {}", known.join(", "))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slow_printer() -> &'static str {
        if cfg!(windows) {
            "echo started && ping -n 30 127.0.0.1 > NUL"
        } else {
            "echo started && sleep 30"
        }
    }

    async fn wait_for_output(processes: &Processes, id: &str, text: &str) -> String {
        for _ in 0..100 {
            let output = processes.output(id, Some(0)).unwrap();
            if output.contains(text) {
                return output;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "{text:?} never appeared: {}",
            processes.output(id, Some(0)).unwrap()
        );
    }

    #[tokio::test]
    async fn background_commands_start_report_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let processes = workspace.processes().clone();
        let id = processes.start(&workspace, slow_printer()).unwrap();
        assert_eq!(id, "p1");
        let output = wait_for_output(&processes, &id, "started").await;
        assert!(output.contains("running for"), "{output}");
        // Reading again from where the last read stopped finds nothing new.
        let _ = processes.output(&id, None).unwrap();
        let again = processes.output(&id, None).unwrap();
        assert!(again.contains("[no new output]"), "{again}");

        let stopped = processes.stop(&id).await.unwrap();
        assert!(stopped.contains("stopped"), "{stopped}");
        assert_eq!(processes.running(), 0);
        assert!(processes.output("p9", None).is_err());
    }

    #[tokio::test]
    async fn finished_commands_report_their_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let processes = workspace.processes().clone();
        let id = processes.start(&workspace, "echo done && exit 4").unwrap();
        let output = wait_for_output(&processes, &id, "exited with code 4").await;
        assert!(output.contains("done"), "{output}");
    }

    #[tokio::test]
    async fn the_number_running_is_capped_and_stop_all_ends_them() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let processes = workspace.processes().clone();
        let ids: Vec<String> = (0..MAX_RUNNING)
            .map(|_| processes.start(&workspace, slow_printer()).unwrap())
            .collect();
        assert!(matches!(
            processes.start(&workspace, slow_printer()),
            Err(ToolError::Invalid(message)) if message.contains("already running")
        ));
        processes.stop_all();
        for id in &ids {
            wait_for_output(&processes, id, ": stopped").await;
        }
        assert_eq!(processes.running(), 0);
    }

    /// Whether a process with this id exists.
    fn exists(pid: u32) -> bool {
        if cfg!(windows) {
            let out = std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
        } else {
            quietly("kill", &["-0", "--", &format!("-{pid}")])
        }
    }

    #[tokio::test]
    async fn children_left_by_an_exited_command_count_and_are_killed() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let processes = workspace.processes().clone();
        // The shell exits at once and leaves its child running.
        let orphaning = if cfg!(windows) {
            "start /b ping -n 300 127.0.0.1 > NUL & echo started"
        } else {
            "sleep 300 & echo started"
        };
        let id = processes.start(&workspace, orphaning).unwrap();
        wait_for_output(&processes, &id, "exited with code 0").await;
        let members = {
            let table = lock(&processes.0);
            table.entries[0].killer.members()
        };
        assert!(!members.is_empty(), "the child outlived the shell");
        assert!(members.iter().all(|pid| exists(*pid)));
        // It still counts against the cap, and its output says what is left.
        assert_eq!(processes.running(), 1);
        let output = processes.output(&id, Some(0)).unwrap();
        assert!(output.contains("still running"), "{output}");

        processes.stop_all();
        for _ in 0..100 {
            if members.iter().all(|pid| !exists(*pid)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            members.iter().all(|pid| !exists(*pid)),
            "stop_all killed the group"
        );
        assert_eq!(processes.running(), 0);
    }

    #[tokio::test]
    async fn dropping_the_table_kills_what_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let processes = workspace.processes().clone();
        let id = processes.start(&workspace, slow_printer()).unwrap();
        wait_for_output(&processes, &id, "started").await;
        let members = lock(&processes.0).entries[0].killer.members();
        assert!(!members.is_empty());
        drop(processes);
        drop(workspace);
        for _ in 0..100 {
            if members.iter().all(|pid| !exists(*pid)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(members.iter().all(|pid| !exists(*pid)));
    }

    #[tokio::test]
    async fn pruning_keeps_a_command_whose_children_still_run() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let processes = workspace.processes().clone();
        let orphaning = if cfg!(windows) {
            "start /b ping -n 300 127.0.0.1 > NUL & echo started"
        } else {
            "sleep 300 & echo started"
        };
        let first = processes.start(&workspace, orphaning).unwrap();
        wait_for_output(&processes, &first, "exited with code 0").await;
        // Fill the table with commands that finish, past the number kept.
        for _ in 0..MAX_KEPT {
            let id = processes.start(&workspace, "echo done").unwrap();
            wait_for_output(&processes, &id, "exited with code 0").await;
        }
        assert!(
            processes.output(&first, Some(0)).is_ok(),
            "the command whose children run was not pruned (its killer would be lost)"
        );
        assert_eq!(processes.running(), 1);
        processes.stop_all_and_wait(Duration::from_secs(10)).await;
        assert_eq!(processes.running(), 0);
    }

    #[test]
    fn container_names_differ_for_the_same_id() {
        assert_ne!(container_name("p1"), container_name("p1"));
        assert!(container_name("p1").starts_with("imperium-bg-"));
    }

    #[test]
    fn output_keeps_only_the_tail() {
        let mut output = Output::default();
        let full: Vec<u8> = std::iter::repeat_n(b'a', MAX_BUFFER).collect();
        output.push(&full);
        output.push(b"end");
        assert_eq!(output.bytes.len(), MAX_BUFFER);
        assert_eq!(output.dropped, 3);
        assert_eq!(output.end(), MAX_BUFFER as u64 + 3);
    }
}
