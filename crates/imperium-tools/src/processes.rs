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
    /// Sending (or dropping) it kills the process.
    kill: Option<oneshot::Sender<()>>,
    /// Where the previous `command_output` call stopped.
    read_to: u64,
}

impl Entry {
    fn running(&self) -> bool {
        self.status.borrow().is_none()
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

/// How to end a process and everything it started.
#[derive(Debug, Clone)]
enum Killer {
    /// The process (group on Unix, tree on Windows).
    Tree(Option<u32>),
    /// A container, removed by name with its runtime.
    Container { runtime: String, name: String },
}

impl Killer {
    async fn kill(&self) {
        match self {
            Killer::Tree(Some(pid)) => kill_tree(*pid).await,
            Killer::Tree(None) => {}
            Killer::Container { runtime, name } => {
                let _ = tokio::process::Command::new(runtime)
                    .args(["rm", "-f", name])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
    }
}

#[cfg(windows)]
async fn kill_tree(pid: u32) {
    let _ = tokio::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

#[cfg(not(windows))]
async fn kill_tree(pid: u32) {
    // The command leads its own process group, so the group ends with everything it started.
    let _ = tokio::process::Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
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
        let running = table.entries.iter().filter(|entry| entry.running()).count();
        if running >= MAX_RUNNING {
            return Err(ToolError::Invalid(format!(
                "{MAX_RUNNING} background commands are already running; stop one with \
                 command_stop first"
            )));
        }
        table.next += 1;
        let id = format!("p{}", table.next);
        let container = format!("imperium-bg-{}-{id}", std::process::id());
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
        let killer = match workspace.sandbox() {
            Sandbox::Container { runtime, .. } => Killer::Container {
                runtime: runtime.clone(),
                name: container,
            },
            _ => Killer::Tree(child.id()),
        };
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
                    killer.kill().await;
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
        // Make room: drop the oldest finished processes beyond the limit.
        while table.entries.len() >= MAX_KEPT {
            match table.entries.iter().position(|entry| !entry.running()) {
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
        let mut status = {
            let mut table = lock(&self.0);
            let entry = find(&mut table, id)?;
            if let Some(kill) = entry.kill.take() {
                let _ = kill.send(());
            }
            entry.status.clone()
        };
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

    /// Kills every process (the session stopped or was deleted). Does not wait.
    pub fn stop_all(&self) {
        let mut table = lock(&self.0);
        for entry in &mut table.entries {
            if let Some(kill) = entry.kill.take() {
                let _ = kill.send(());
            }
        }
    }

    /// How many processes are still running.
    pub fn running(&self) -> usize {
        lock(&self.0)
            .entries
            .iter()
            .filter(|entry| entry.running())
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
