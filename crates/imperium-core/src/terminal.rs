//! The daemon's terminals (stage 2a of `docs/plans/terminal-file-panes.md`): shells on pseudo
//! terminals, started for the desktop app in a session's working folder.
//!
//! Each terminal belongs to a running session (four at most) and the daemon holds sixteen at
//! most. A terminal ends when it is closed, when its session is archived, deleted or stopped,
//! after an hour with nothing attached, and when the daemon stops; it is **not** daemon work, so
//! it keeps no idle daemon alive. The shell runs outside every isolation tier and permission
//! rule (it is the user's), so no agent, tool, hook or mod reaches this module: only the
//! protocol's desktop-only `terminal.*` methods will (stage 2b).
//!
//! Terminal bytes go nowhere but the terminal's ring and its subscribers: nothing here logs
//! them, only ids, sizes and exit codes.

pub mod shell;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use imperium_pty::{Exit, Pty, SpawnError, Spec};

use crate::state::DaemonState;
use shell::{Shell, ShellError, ShellSources, resolve_shell, shell_env};

/// Terminals one session may hold.
pub const PER_SESSION: usize = 4;
/// Terminals the daemon may hold.
pub const PER_DAEMON: usize = 16;
/// How long a terminal with nothing attached is kept.
pub const UNATTACHED_LIMIT: Duration = Duration::from_secs(60 * 60);
/// How often terminals with nothing attached are looked for.
const UNATTACHED_CHECK_EVERY: Duration = Duration::from_secs(60);
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 500;

/// Why a terminal could not be opened or used.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("terminals are turned off (terminal.enabled)")]
    Disabled,
    #[error("no running session {0}")]
    NoSession(String),
    #[error("session {0} is archived")]
    Archived(String),
    #[error(
        "this session runs in a remote runtime; terminals are only for sessions on this computer"
    )]
    Remote,
    #[error("a session holds at most 4 terminals")]
    SessionLimit,
    #[error("the daemon holds at most 16 terminals")]
    DaemonLimit,
    #[error("no terminal {0}")]
    NotFound(String),
    #[error(transparent)]
    Shell(#[from] ShellError),
    #[error(transparent)]
    Spawn(#[from] SpawnError),
    #[error("the terminal could not start: {0}")]
    Join(String),
}

/// What a terminal is, for lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalInfo {
    pub terminal_id: String,
    pub session_id: String,
    pub shell: PathBuf,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    /// How the shell ended; `None` while it runs.
    pub exit: Option<Exit>,
    pub created_at_ms: u64,
}

#[derive(Debug)]
struct Attachments {
    count: usize,
    /// When the last attachment went (`None` while one is attached).
    since: Option<Instant>,
}

/// One terminal: its shell on a pseudo terminal.
#[derive(Debug)]
pub struct Terminal {
    id: String,
    session_id: String,
    shell: PathBuf,
    cwd: PathBuf,
    created_at_ms: u64,
    size: Mutex<(u16, u16)>,
    attachments: Mutex<Attachments>,
    pty: Pty,
}

impl Terminal {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The pseudo terminal, for its output ring, subscriptions and input.
    pub fn pty(&self) -> &Pty {
        &self.pty
    }

    pub fn info(&self) -> TerminalInfo {
        let (cols, rows) = *self.size.lock().unwrap_or_else(PoisonError::into_inner);
        TerminalInfo {
            terminal_id: self.id.clone(),
            session_id: self.session_id.clone(),
            shell: self.shell.clone(),
            cwd: self.cwd.clone(),
            cols,
            rows,
            exit: self.pty.exit(),
            created_at_ms: self.created_at_ms,
        }
    }

    /// Tells the shell its new size, clamped to 1 to [`MAX_COLS`] by 1 to [`MAX_ROWS`].
    pub fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        let size = clamp(cols, rows);
        self.pty.resize(size.0, size.1)?;
        *self.size.lock().unwrap_or_else(PoisonError::into_inner) = size;
        Ok(())
    }

    fn attach(&self) {
        let mut attachments = self
            .attachments
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        attachments.count += 1;
        attachments.since = None;
    }

    fn detach(&self, now: Instant) {
        let mut attachments = self
            .attachments
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        attachments.count = attachments.count.saturating_sub(1);
        if attachments.count == 0 {
            attachments.since = Some(now);
        }
    }

    /// Whether nothing has been attached since before `limit` ago.
    fn unattached_at(&self, now: Instant, limit: Duration) -> bool {
        let attachments = self
            .attachments
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        attachments.count == 0
            && attachments
                .since
                .is_some_and(|since| now.saturating_duration_since(since) >= limit)
    }
}

fn clamp(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The daemon's terminals, by id.
#[derive(Debug, Default)]
pub struct TerminalService {
    terminals: Mutex<BTreeMap<String, Arc<Terminal>>>,
    /// Opens one at a time, so two at once cannot both pass the limits.
    opening: tokio::sync::Mutex<()>,
    next: std::sync::atomic::AtomicU64,
}

impl TerminalService {
    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Terminal>>> {
        self.terminals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Terminal>> {
        self.map().get(id).cloned()
    }

    /// A session's terminals, oldest first.
    pub fn list(&self, session_id: &str) -> Vec<TerminalInfo> {
        let mut infos: Vec<TerminalInfo> = self
            .map()
            .values()
            .filter(|terminal| terminal.session_id == session_id)
            .map(|terminal| terminal.info())
            .collect();
        infos.sort_by_key(|info| info.created_at_ms);
        infos
    }

    /// Every terminal's id.
    pub fn ids(&self) -> Vec<String> {
        self.map().keys().cloned().collect()
    }

    fn take(&self, keep: impl Fn(&Terminal) -> bool) -> Vec<Arc<Terminal>> {
        let mut map = self.map();
        let ids: Vec<String> = map
            .values()
            .filter(|terminal| !keep(terminal))
            .map(|terminal| terminal.id.clone())
            .collect();
        ids.iter().filter_map(|id| map.remove(id)).collect()
    }
}

/// Ends terminals on threads of their own: `end` blocks for seconds, and a caller that is
/// archiving or deleting a session must not wait for it.
fn end_in_background(terminals: Vec<Arc<Terminal>>) {
    for terminal in terminals {
        tracing::info!(terminal = %terminal.id, session = %terminal.session_id, "ending the terminal");
        let _ = std::thread::Builder::new()
            .name("imperium-terminal-end".into())
            .spawn(move || {
                terminal.pty.end();
            });
    }
}

impl DaemonState {
    /// Opens a terminal in the session's working folder (the first folder, its worktree when it
    /// works in one), attached once for the caller. The shell and its environment are the
    /// daemon's choice (decision D6): nothing from the caller but the size reaches the spawn.
    pub async fn open_terminal(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalInfo, TerminalError> {
        let settings = self.terminal_settings();
        if !settings.enabled {
            return Err(TerminalError::Disabled);
        }
        let terminals = self.terminals();
        let _opening = terminals.opening.lock().await;
        let session = match self.session(session_id) {
            Some(session) => session,
            None if self.is_archived(session_id) => {
                return Err(TerminalError::Archived(session_id.to_owned()));
            }
            None => return Err(TerminalError::NoSession(session_id.to_owned())),
        };
        let workspace = session.workspace();
        if workspace.runtime().is_some() {
            return Err(TerminalError::Remote);
        }
        {
            let map = terminals.map();
            if map.len() >= PER_DAEMON {
                return Err(TerminalError::DaemonLimit);
            }
            if map
                .values()
                .filter(|terminal| terminal.session_id == session_id)
                .count()
                >= PER_SESSION
            {
                return Err(TerminalError::SessionLimit);
            }
        }
        let cwd = workspace.root().to_path_buf();
        let folders: Vec<PathBuf> = workspace
            .roots()
            .into_iter()
            .map(std::path::Path::to_path_buf)
            .chain(workspace.scratch().map(std::path::Path::to_path_buf))
            .collect();
        drop(session);
        let Shell { program, args } =
            resolve_shell(&ShellSources::from_system(settings.shell, folders))?;
        let (cols, rows) = clamp(cols, rows);
        let mut spec = Spec::new(program.clone(), cwd.clone());
        spec.args = args;
        spec.env = shell_env(std::env::vars_os(), session_id, &cwd);
        spec.cols = cols;
        spec.rows = rows;
        // `spec.supervisor` and `spec.daemon` stay `None`: the Linux supervisor is this very
        // executable (`/proc/self/exe pty-supervisor`) watching this very process. They are
        // never taken from a caller.
        #[cfg(feature = "testing")]
        if let Some(supervisor) = testing::supervisor() {
            spec.supervisor = Some(supervisor);
        }
        let pty = tokio::task::spawn_blocking(move || Pty::spawn(spec))
            .await
            .map_err(|error| TerminalError::Join(error.to_string()))??;
        let number = terminals
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let terminal = Arc::new(Terminal {
            id: format!("term-{}-{number}", now_ms()),
            session_id: session_id.to_owned(),
            shell: program,
            cwd,
            created_at_ms: now_ms(),
            size: Mutex::new((cols, rows)),
            attachments: Mutex::new(Attachments {
                count: 1,
                since: None,
            }),
            pty,
        });
        // The session may have been archived or deleted while the shell started.
        if self.session(session_id).is_none() {
            end_in_background(vec![terminal]);
            return Err(TerminalError::NoSession(session_id.to_owned()));
        }
        let info = terminal.info();
        tracing::info!(terminal = %info.terminal_id, session = %session_id, cols, rows, "opened a terminal");
        terminals.map().insert(info.terminal_id.clone(), terminal);
        Ok(info)
    }

    /// The terminal settings from the user's own file as it is now (they apply without a
    /// restart), else as the daemon started with them. An environment's files never set them.
    fn terminal_settings(&self) -> imperium_config::TerminalSettings {
        crate::connectors::saved_settings(self.paths())
            .map_or_else(|| self.settings().terminal.clone(), |saved| saved.terminal)
    }

    /// Attaches one more viewer to a terminal.
    pub fn attach_terminal(&self, terminal_id: &str) -> Result<Arc<Terminal>, TerminalError> {
        let terminal = self
            .terminals()
            .get(terminal_id)
            .ok_or_else(|| TerminalError::NotFound(terminal_id.to_owned()))?;
        terminal.attach();
        Ok(terminal)
    }

    /// Detaches a viewer; the terminal keeps running.
    pub fn detach_terminal(&self, terminal_id: &str) -> Result<(), TerminalError> {
        let terminal = self
            .terminals()
            .get(terminal_id)
            .ok_or_else(|| TerminalError::NotFound(terminal_id.to_owned()))?;
        terminal.detach(Instant::now());
        Ok(())
    }

    /// Ends a terminal's processes and removes it; how its shell ended.
    pub async fn close_terminal(&self, terminal_id: &str) -> Result<Option<Exit>, TerminalError> {
        let terminal = self
            .terminals()
            .map()
            .remove(terminal_id)
            .ok_or_else(|| TerminalError::NotFound(terminal_id.to_owned()))?;
        tracing::info!(terminal = %terminal_id, "closing the terminal");
        tokio::task::spawn_blocking(move || terminal.pty.end())
            .await
            .map_err(|error| TerminalError::Join(error.to_string()))
    }

    /// Ends every terminal of a session (archived, deleted or stopped), without waiting.
    pub(crate) fn end_session_terminals(&self, session_id: &str) {
        end_in_background(
            self.terminals()
                .take(|terminal| terminal.session_id != session_id),
        );
    }

    /// Ends the terminals that have had nothing attached for [`UNATTACHED_LIMIT`] at `now`;
    /// their ids.
    pub fn end_unattached_terminals(&self, now: Instant) -> Vec<String> {
        let ended = self
            .terminals()
            .take(|terminal| !terminal.unattached_at(now, UNATTACHED_LIMIT));
        let ids = ended.iter().map(|terminal| terminal.id.clone()).collect();
        end_in_background(ended);
        ids
    }

    /// Looks for unattached terminals every minute while the daemon runs.
    pub fn start_terminal_sweeper(self: &Arc<Self>) {
        let daemon = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(UNATTACHED_CHECK_EVERY).await;
                let Some(state) = daemon.upgrade() else {
                    return;
                };
                state.end_unattached_terminals(Instant::now());
            }
        });
    }

    /// Ends every terminal and waits up to `timeout` for their processes to end (shutdown).
    pub async fn stop_terminals(&self, timeout: Duration) {
        let all = self.terminals().take(|_| false);
        if all.is_empty() {
            return;
        }
        tracing::info!(terminals = all.len(), "ending the terminals");
        let mut ends = tokio::task::JoinSet::new();
        for terminal in all {
            ends.spawn_blocking(move || {
                terminal.pty.end();
            });
        }
        if tokio::time::timeout(timeout, ends.join_all())
            .await
            .is_err()
        {
            tracing::warn!("terminals were still ending when the daemon stopped waiting");
        }
    }
}

/// Test support: start the Linux supervisor from another executable than this process's own.
/// Only the `testing` feature has it, and only a test sets it.
#[cfg(feature = "testing")]
pub mod testing {
    use std::sync::Mutex;

    use imperium_pty::Supervisor;

    static SUPERVISOR: Mutex<Option<Supervisor>> = Mutex::new(None);

    /// Makes later terminals start `<exe> pty-supervisor ...` instead of `/proc/self/exe`.
    pub fn use_supervisor_exe(exe: Option<std::path::PathBuf>) {
        *SUPERVISOR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = exe.map(|exe| Supervisor {
            exe,
            args: vec!["pty-supervisor".into()],
            flags: Vec::new(),
        });
    }

    pub(super) fn supervisor() -> Option<Supervisor> {
        SUPERVISOR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
