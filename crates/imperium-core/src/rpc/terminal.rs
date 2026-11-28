//! `terminal.*` (stage 2b of `docs/plans/terminal-file-panes.md`): the desktop-only gate of
//! decision D2, and each connection's attachments.
//!
//! **The gate.** Terminals are uncapped and run outside every isolation tier, so they are
//! answered only to the installed desktop app: the connection came in on the local socket, its
//! peer is the `imperium-desktop` installed beside `imperium-core` (`app_identity::InstalledApp`,
//! checks 1 to 4 of the plugin approval) **and** the peer may make grants (`peer::may_grant`: it
//! and its ancestors are not the daemon or `sshd` and carry no spawn mark). Anything that reads
//! the daemon's token (an agent's command, an MCP server, a hook, a plugin, an `imperium` command
//! an agent runs) fails it, as do loopback TCP (a mod frame, a page), a paired phone (refused by
//! its scopes before the gate) and a forwarded remote client. Every method whose name holds
//! `terminal` passes through it before its params are read, so a method added later cannot leave
//! it out. It runs once, on the connection's first such call, behind [`TerminalGate`], and the
//! verdict is kept for the connection's life. No lease is involved: a `system.lease` of kind
//! `desktop` is something any token holder can claim and counts for nothing here.
//!
//! **Attachments.** A connection is attached to the terminals it opened or attached; their output
//! and changes go on its own channel ([`TerminalFrames`]), never through the broadcast path.
//! Output is coalesced (at most 64 KiB per notification and one notification per 16 ms per
//! terminal); a connection more than 1 MiB behind is told it `lagged` and gets nothing more from
//! that terminal until it attaches again with `since`. A connection that closes is detached from
//! every terminal it was attached to, so the hour-unattached rule applies.
//!
//! Nothing here logs terminal bytes; only ids and sizes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use imperium_protocol::jsonrpc::Notification;
use imperium_protocol::methods::{
    TERMINAL_CHANGED, TERMINAL_OUTPUT, TerminalAttach, TerminalAttachParams, TerminalAttachResult,
    TerminalChangedParams, TerminalClose, TerminalDetach, TerminalInput, TerminalInputParams,
    TerminalList, TerminalListParams, TerminalListResult, TerminalOpen, TerminalOpenParams,
    TerminalOutputParams, TerminalRef, TerminalResize, TerminalResizeParams, TerminalState,
};
use imperium_pty::{Event, SubscriptionId};
use tokio::sync::{Notify, mpsc};

use super::*;
use crate::terminal::{Terminal, TerminalError};

/// Most output in one `terminal.output`, in bytes.
const CHUNK: usize = 64 * 1024;
/// At most one `terminal.output` per terminal in this long.
const EVERY: Duration = Duration::from_millis(16);
/// How far behind (output not yet written to it, in bytes) a connection may fall.
pub(crate) const LAG_LIMIT: usize = 1024 * 1024;
/// Most input in one `terminal.input`, decoded.
const INPUT_LIMIT: usize = 64 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Who is calling a `terminal.*` method, for a [`TerminalGate`].
#[derive(Debug, Clone, Copy)]
pub struct TerminalCaller<'a> {
    daemon: &'a DaemonState,
    identity: Option<&'a crate::app_identity::Peer>,
    peer: Option<(u32, u64)>,
}

impl TerminalCaller<'_> {
    /// Whether the call came in on the local socket (loopback TCP and a phone never do).
    pub fn on_local_socket(&self) -> bool {
        self.peer.is_some() || self.identity.is_some()
    }

    /// The daemon the call came to.
    pub fn daemon(&self) -> &DaemonState {
        self.daemon
    }
}

/// Decides whether a connection may use terminals. Asked once per connection, on its first
/// `terminal.*` call; the verdict holds for the connection's life. The daemon's own is the
/// desktop-app check of the module documentation; tests pass a stub.
pub trait TerminalGate: Send + Sync + std::fmt::Debug {
    /// `Ok` to let the connection use terminals; otherwise why not.
    fn admit(&self, caller: &TerminalCaller<'_>) -> Result<(), String>;
}

/// The production gate: the installed desktop app, run by a person.
#[derive(Debug)]
pub(crate) struct DesktopGate;

impl TerminalGate for DesktopGate {
    fn admit(&self, caller: &TerminalCaller<'_>) -> Result<(), String> {
        // Both checks are made from the daemon's state, so a server (`daemon.server`) refuses
        // every terminal before any peer is looked at.
        let installed = crate::app_identity::InstalledApp::of(caller.daemon);
        #[cfg(any(test, feature = "testing"))]
        let stand_in = testing::StandIn;
        #[cfg(any(test, feature = "testing"))]
        let identify: &dyn crate::app_identity::Identify = if testing::app_check_skipped() {
            &stand_in
        } else {
            &installed
        };
        #[cfg(not(any(test, feature = "testing")))]
        let identify: &dyn crate::app_identity::Identify = &installed;
        desktop_verdict(identify, caller, |pid, accepted_at| {
            crate::peer::may_grant(caller.daemon, pid, accepted_at).is_ok()
        })
    }
}

/// Test support: lets the production gate's app check pass for any peer, so a test on the real
/// local socket sees the grant check (`peer::may_grant`) judge a real process on its own. Only
/// unit tests and the `testing` feature have it.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::sync::atomic::{AtomicBool, Ordering};

    static SKIP_APP_CHECK: AtomicBool = AtomicBool::new(false);

    /// While `skip`, the desktop gate's app check passes for any peer; the grant check still runs.
    pub fn skip_app_check(skip: bool) {
        SKIP_APP_CHECK.store(skip, Ordering::SeqCst);
    }

    pub(super) fn app_check_skipped() -> bool {
        SKIP_APP_CHECK.load(Ordering::SeqCst)
    }

    /// The app check standing in for `InstalledApp` meanwhile.
    pub(super) struct StandIn;

    impl crate::app_identity::Identify for StandIn {
        fn identify(&self, _peer: Option<&crate::app_identity::Peer>) -> Result<(), String> {
            Ok(())
        }
    }
}

/// Why a peer that passed the app check still gets no terminal.
pub(crate) const NOT_A_PERSON: &str = "the caller was started by the daemon or runs below a process it \
     started (an agent's command, a hook, a terminal), or came through SSH";

/// The desktop-app check (`identify`), then the grant check (`may_grant`) of the peer.
fn desktop_verdict(
    identify: &dyn crate::app_identity::Identify,
    caller: &TerminalCaller<'_>,
    may_grant: impl FnOnce(u32, u64) -> bool,
) -> Result<(), String> {
    identify.identify(caller.identity)?;
    let (pid, accepted_at) = caller
        .peer
        .ok_or_else(|| "the connection did not come in on the local socket".to_string())?;
    if may_grant(pid, accepted_at) {
        Ok(())
    } else {
        Err(NOT_A_PERSON.to_string())
    }
}

/// The refusal every caller but the desktop app gets: `-32005`.
fn refusal(reason: impl std::fmt::Display) -> ErrorObject {
    ErrorObject::new(
        codes::FORBIDDEN,
        format!("permission denied: terminals are only for the desktop app: {reason}"),
    )
}

fn terminal_error(error: TerminalError) -> ErrorObject {
    let code = match &error {
        TerminalError::NoSession(_) | TerminalError::Archived(_) => codes::SESSION_NOT_FOUND,
        TerminalError::Stopping => codes::DAEMON_STOPPING,
        TerminalError::Spawn(_) | TerminalError::Join(_) => codes::INTERNAL_ERROR,
        TerminalError::Disabled
        | TerminalError::Remote
        | TerminalError::SessionLimit
        | TerminalError::DaemonLimit
        | TerminalError::NotFound(_)
        | TerminalError::Shell(_) => codes::INVALID_PARAMS,
    };
    ErrorObject::new(code, error.to_string())
}

/// A terminal as the protocol shows it.
fn wire_info(info: crate::terminal::TerminalInfo) -> imperium_protocol::methods::TerminalInfo {
    let title = info
        .shell
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    imperium_protocol::methods::TerminalInfo {
        terminal_id: info.terminal_id,
        session_id: info.session_id,
        title,
        cwd: info.cwd.display().to_string(),
        shell: info.shell.display().to_string(),
        cols: info.cols,
        rows: info.rows,
        state: if info.exit.is_some() {
            TerminalState::Exited
        } else {
            TerminalState::Running
        },
        exit_code: info.exit.map(|exit| exit.code),
        created_at: info.created_at_ms,
    }
}

/// One connection's `terminal.output` and `terminal.changed` notifications, in order. The
/// transport takes them with [`Connection::terminal_frames`] and writes them to that connection
/// alone.
#[derive(Debug)]
pub struct TerminalFrames {
    frames: mpsc::UnboundedReceiver<(Notification, usize)>,
    backlog: Arc<AtomicUsize>,
}

impl TerminalFrames {
    /// The next notification; what it carried no longer counts as the connection's backlog.
    pub async fn recv(&mut self) -> Option<Notification> {
        let (notification, bytes) = self.frames.recv().await?;
        self.backlog.fetch_sub(bytes, Ordering::SeqCst);
        Some(notification)
    }

    /// The next notification if one is waiting.
    pub fn try_recv(&mut self) -> Option<Notification> {
        let (notification, bytes) = self.frames.try_recv().ok()?;
        self.backlog.fetch_sub(bytes, Ordering::SeqCst);
        Some(notification)
    }
}

/// Output waiting to be sent for one attachment.
#[derive(Debug, Default)]
struct Pending {
    queued: Mutex<Queued>,
    wake: Notify,
}

#[derive(Debug, Default)]
struct Queued {
    /// The `seq` of the first byte of `bytes`.
    seq: u64,
    bytes: Vec<u8>,
    lagged: bool,
    exited: bool,
}

/// What the forwarder takes in one go.
struct Taken {
    data: Option<(u64, Vec<u8>)>,
    more: bool,
    lagged: bool,
    exited: bool,
}

impl Pending {
    /// Called on the terminal's reader thread: never blocks.
    fn push(&self, event: Event<'_>, backlog: &AtomicUsize) {
        {
            let mut queued = lock(&self.queued);
            match event {
                Event::Data { seq, bytes } => {
                    if queued.lagged {
                        return;
                    }
                    if queued.bytes.is_empty() {
                        queued.seq = seq;
                    }
                    let behind = backlog.load(Ordering::SeqCst) + queued.bytes.len() + bytes.len();
                    if behind > LAG_LIMIT {
                        queued.lagged = true;
                        queued.bytes = Vec::new();
                    } else {
                        queued.bytes.extend_from_slice(bytes);
                    }
                }
                Event::Exited(_) => queued.exited = true,
            }
        }
        self.wake.notify_one();
    }

    /// Puts output that came before anything pushed so far in front of it (the opener's replay).
    fn prepend(&self, seq: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        {
            let mut queued = lock(&self.queued);
            let mut all = bytes.to_vec();
            all.append(&mut queued.bytes);
            queued.bytes = all;
            queued.seq = seq;
        }
        self.wake.notify_one();
    }

    fn take(&self, most: usize) -> Taken {
        let mut queued = lock(&self.queued);
        let data = if queued.bytes.is_empty() {
            None
        } else {
            let count = queued.bytes.len().min(most);
            let chunk: Vec<u8> = queued.bytes.drain(..count).collect();
            let seq = queued.seq;
            queued.seq += count as u64;
            Some((seq, chunk))
        };
        Taken {
            data,
            more: !queued.bytes.is_empty(),
            lagged: queued.lagged,
            exited: queued.exited,
        }
    }
}

/// Sends one attachment's output and its terminal's exit to the connection, coalesced.
async fn forward(
    terminal: Arc<Terminal>,
    pending: Arc<Pending>,
    outbox: mpsc::UnboundedSender<(Notification, usize)>,
    backlog: Arc<AtomicUsize>,
) {
    let id = terminal.id().to_owned();
    let output =
        |params: TerminalOutputParams| Notification::new(TERMINAL_OUTPUT, to_value(&params));
    loop {
        pending.wake.notified().await;
        loop {
            let taken = pending.take(CHUNK);
            if let Some((seq, bytes)) = taken.data {
                backlog.fetch_add(bytes.len(), Ordering::SeqCst);
                let params = TerminalOutputParams {
                    terminal_id: id.clone(),
                    seq: Some(seq),
                    data: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
                    lagged: false,
                };
                if outbox.send((output(params), bytes.len())).is_err() {
                    return;
                }
            }
            if taken.lagged {
                tracing::info!(terminal = %id, "a connection fell behind the terminal's output");
                let params = TerminalOutputParams {
                    terminal_id: id.clone(),
                    seq: None,
                    data: None,
                    lagged: true,
                };
                let _ = outbox.send((output(params), 0));
                return;
            }
            if taken.exited && !taken.more {
                let params = TerminalChangedParams {
                    info: wire_info(terminal.info()),
                };
                let _ = outbox.send((Notification::new(TERMINAL_CHANGED, to_value(&params)), 0));
                return;
            }
            if !taken.more {
                break;
            }
            tokio::time::sleep(EVERY).await;
        }
        tokio::time::sleep(EVERY).await;
    }
}

/// One terminal this connection is attached to.
#[derive(Debug)]
struct Attachment {
    terminal: Arc<Terminal>,
    subscription: SubscriptionId,
    forwarder: Option<tokio::task::AbortHandle>,
}

impl Attachment {
    fn stop(&self) {
        self.terminal.pty().unsubscribe(self.subscription);
        if let Some(forwarder) = &self.forwarder {
            forwarder.abort();
        }
    }
}

#[derive(Debug, Default)]
struct Attached {
    /// The connection ended: nothing attaches any more.
    closed: bool,
    by_id: BTreeMap<String, Attachment>,
}

/// What a connection's deferred calls share with it.
#[derive(Debug)]
struct Shared {
    outbox: mpsc::UnboundedSender<(Notification, usize)>,
    /// Bytes of output sent towards the connection and not yet taken by its transport.
    backlog: Arc<AtomicUsize>,
    attached: Mutex<Attached>,
}

impl Shared {
    /// Attaches the connection to `terminal_id` from `since` and returns the terminal and the
    /// kept output from there. A terminal the connection is already attached to is re-attached
    /// (its subscription replaced, still counted once). For the `opener` the daemon has already
    /// counted the attachment, and the kept output is sent as notifications instead.
    fn attach(
        &self,
        state: &DaemonState,
        terminal_id: &str,
        since: u64,
        opener: bool,
    ) -> Result<(Arc<Terminal>, imperium_pty::Replay), TerminalError> {
        let gone = || TerminalError::NotFound(terminal_id.to_owned());
        let mut attached = lock(&self.attached);
        if attached.closed {
            if opener {
                let _ = state.detach_terminal(terminal_id);
            }
            return Err(gone());
        }
        let terminal = match attached.by_id.remove(terminal_id) {
            Some(old) => {
                old.stop();
                state.terminals().get(terminal_id).ok_or_else(gone)?
            }
            None if opener => state.terminals().get(terminal_id).ok_or_else(gone)?,
            None => state.attach_terminal(terminal_id)?,
        };
        let pending = Arc::new(Pending::default());
        let callback = {
            let pending = Arc::clone(&pending);
            let backlog = Arc::clone(&self.backlog);
            move |event: Event<'_>| pending.push(event, &backlog)
        };
        let (subscription, replay, announced) = terminal.pty().subscribe(since, callback);
        if opener {
            pending.prepend(replay.from, &replay.data);
        }
        // An exit announced before this subscription is in the terminal's info already.
        let forwarder = if announced.is_some() && !opener {
            None
        } else {
            if let Some(exit) = &announced {
                pending.push(Event::Exited(exit), &self.backlog);
            }
            Some(
                tokio::spawn(forward(
                    Arc::clone(&terminal),
                    Arc::clone(&pending),
                    self.outbox.clone(),
                    Arc::clone(&self.backlog),
                ))
                .abort_handle(),
            )
        };
        attached.by_id.insert(
            terminal_id.to_owned(),
            Attachment {
                terminal: Arc::clone(&terminal),
                subscription,
                forwarder,
            },
        );
        Ok((terminal, replay))
    }

    /// Detaches the connection from `terminal_id`; `false` when it was not attached.
    fn detach(&self, state: &DaemonState, terminal_id: &str) -> bool {
        let Some(attachment) = lock(&self.attached).by_id.remove(terminal_id) else {
            return false;
        };
        attachment.stop();
        // A terminal closed meanwhile is already gone.
        let _ = state.detach_terminal(terminal_id);
        true
    }

    /// Forgets `terminal_id`, which is being closed.
    fn forget(&self, terminal_id: &str) {
        if let Some(attachment) = lock(&self.attached).by_id.remove(terminal_id) {
            attachment.stop();
        }
    }

    /// The connection ended: detach it from everything, and from anything attached later.
    fn close(&self, state: &DaemonState) {
        let all = {
            let mut attached = lock(&self.attached);
            attached.closed = true;
            std::mem::take(&mut attached.by_id)
        };
        for (id, attachment) in all {
            attachment.stop();
            let _ = state.detach_terminal(&id);
        }
    }
}

/// One connection's terminal state: the gate, its verdict, and the attachments.
#[derive(Debug)]
pub(crate) struct TerminalLink {
    pub(super) gate: Arc<dyn TerminalGate>,
    verdict: Option<Result<(), String>>,
    shared: Arc<Shared>,
    frames: Option<TerminalFrames>,
}

impl TerminalLink {
    pub(super) fn new() -> Self {
        let (outbox, frames) = mpsc::unbounded_channel();
        let backlog = Arc::new(AtomicUsize::new(0));
        TerminalLink {
            gate: Arc::new(DesktopGate),
            verdict: None,
            shared: Arc::new(Shared {
                outbox,
                backlog: Arc::clone(&backlog),
                attached: Mutex::default(),
            }),
            frames: Some(TerminalFrames { frames, backlog }),
        }
    }

    pub(super) fn take_frames(&mut self) -> Option<TerminalFrames> {
        self.frames.take()
    }

    /// The connection ended.
    pub(super) fn close(&self, state: &DaemonState) {
        self.shared.close(state);
    }
}

impl Connection {
    /// Whether this connection may use terminals: never a paired device; otherwise the gate's
    /// verdict, asked on the first call and kept.
    pub(super) fn terminal_admitted(&mut self) -> Result<(), ErrorObject> {
        if self.allowed.is_some() {
            return Err(ErrorObject::new(
                codes::FORBIDDEN,
                "a paired device gets no terminal",
            ));
        }
        #[cfg(feature = "testing")]
        if self.desktop_app_for_tests {
            return Ok(());
        }
        let link = &mut self.terminal;
        if link.verdict.is_none() {
            let caller = TerminalCaller {
                daemon: &self.state,
                identity: self.identity.as_deref(),
                peer: self.peer,
            };
            let verdict = link.gate.admit(&caller);
            if let Err(reason) = &verdict {
                tracing::info!(%reason, "refused terminals to a connection");
            }
            link.verdict = Some(verdict);
        }
        link.verdict.clone().unwrap_or(Ok(())).map_err(refusal)
    }

    /// The `terminal` methods, each behind the gate; `None` for a method of another group.
    pub(super) fn dispatch_terminal(
        &mut self,
        c: &mut Dispatch<'_>,
    ) -> Option<Result<Value, ErrorObject>> {
        if !c.method.contains("terminal") {
            return None;
        }
        if let Err(refused) = self.terminal_admitted() {
            return Some(Err(refused));
        }
        Some(match c.method {
            TerminalOpen::NAME => call_later::<TerminalOpen, _>(
                c.params.take(),
                c.later,
                |params: TerminalOpenParams| {
                    let state = self.state();
                    let shared = Arc::clone(&self.terminal.shared);
                    async move {
                        let info = state
                            .open_terminal(&params.session_id, params.cols, params.rows)
                            .await
                            .map_err(terminal_error)?;
                        shared
                            .attach(&state, &info.terminal_id, 0, true)
                            .map_err(terminal_error)?;
                        Ok(wire_info(info))
                    }
                },
            ),
            TerminalList::NAME => {
                call::<TerminalList>(c.params.take(), |params: TerminalListParams| {
                    Ok(TerminalListResult {
                        terminals: self
                            .state
                            .terminals()
                            .list(&params.session_id)
                            .into_iter()
                            .map(wire_info)
                            .collect(),
                    })
                })
            }
            TerminalAttach::NAME => {
                call::<TerminalAttach>(c.params.take(), |params: TerminalAttachParams| {
                    let (terminal, replay) = self
                        .terminal
                        .shared
                        .attach(
                            &self.state,
                            &params.terminal_id,
                            params.since.unwrap_or(0),
                            false,
                        )
                        .map_err(terminal_error)?;
                    tracing::debug!(terminal = %params.terminal_id, bytes = replay.data.len(), "attached");
                    Ok(TerminalAttachResult {
                        info: wire_info(terminal.info()),
                        replay: base64::engine::general_purpose::STANDARD.encode(&replay.data),
                        seq: replay.from,
                        truncated: replay.dropped > 0,
                    })
                })
            }
            TerminalDetach::NAME => {
                call::<TerminalDetach>(c.params.take(), |params: TerminalRef| {
                    if self
                        .terminal
                        .shared
                        .detach(&self.state, &params.terminal_id)
                    {
                        Ok(Accepted {})
                    } else {
                        Err(ErrorObject::new(
                            codes::INVALID_PARAMS,
                            format!(
                                "this connection is not attached to terminal {}",
                                params.terminal_id
                            ),
                        ))
                    }
                })
            }
            TerminalInput::NAME => {
                call::<TerminalInput>(c.params.take(), |params: TerminalInputParams| {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(params.data.as_bytes())
                        .map_err(|_| {
                            ErrorObject::new(codes::INVALID_PARAMS, "data is not base64")
                        })?;
                    if bytes.len() > INPUT_LIMIT {
                        return Err(ErrorObject::new(
                            codes::INVALID_PARAMS,
                            "at most 64 KiB of input per call",
                        ));
                    }
                    let terminal =
                        self.state
                            .terminals()
                            .get(&params.terminal_id)
                            .ok_or_else(|| {
                                terminal_error(TerminalError::NotFound(params.terminal_id.clone()))
                            })?;
                    terminal.pty().write(&bytes).map_err(|error| {
                        ErrorObject::new(codes::INVALID_PARAMS, error.to_string())
                    })?;
                    Ok(Accepted {})
                })
            }
            TerminalResize::NAME => {
                call::<TerminalResize>(c.params.take(), |params: TerminalResizeParams| {
                    let terminal =
                        self.state
                            .terminals()
                            .get(&params.terminal_id)
                            .ok_or_else(|| {
                                terminal_error(TerminalError::NotFound(params.terminal_id.clone()))
                            })?;
                    terminal.resize(params.cols, params.rows).map_err(|error| {
                        ErrorObject::new(codes::INVALID_PARAMS, error.to_string())
                    })?;
                    Ok(Accepted {})
                })
            }
            TerminalClose::NAME => {
                call_later::<TerminalClose, _>(c.params.take(), c.later, |params: TerminalRef| {
                    self.terminal.shared.forget(&params.terminal_id);
                    let state = self.state();
                    async move {
                        state
                            .close_terminal(&params.terminal_id)
                            .await
                            .map_err(terminal_error)?;
                        Ok(Accepted {})
                    }
                })
            }
            method => Err(ErrorObject::new(
                codes::METHOD_NOT_FOUND,
                format!("unknown method {method:?}"),
            )),
        })
    }
}

/// A gate that lets every connection in, for the routing test.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct AdmitAll;

#[cfg(test)]
impl TerminalGate for AdmitAll {
    fn admit(&self, _caller: &TerminalCaller<'_>) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    /// An app check that says yes or no without looking at the peer.
    struct App(Result<(), &'static str>);

    impl crate::app_identity::Identify for App {
        fn identify(&self, _peer: Option<&crate::app_identity::Peer>) -> Result<(), String> {
            self.0.map_err(str::to_string)
        }
    }

    fn daemon() -> (Arc<DaemonState>, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let settings = imperium_config::Config::from_layers([imperium_config::Layer::defaults()])
            .settings()
            .unwrap();
        let state = DaemonState::new(imperium_config::Paths::under(home.path()), settings, None);
        (state, home)
    }

    /// A process that lives a few seconds, as a child of this one, with or without the mark.
    fn child(marked: bool) -> std::process::Child {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("ping");
            command.args(["-n", "6", "127.0.0.1"]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg("5");
            command
        };
        if marked {
            command.env(imperium_tools::SPAWNED_MARK, "1");
        } else {
            command.env_remove(imperium_tools::SPAWNED_MARK);
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        child
    }

    /// The grant check of a daemon that is some other process, so this test's own children are
    /// judged by their mark alone.
    fn grant(pid: u32, _accepted_at: u64) -> bool {
        crate::peer::may_grant_from(pid, u32::MAX - 1, None)
    }

    /// Both halves of the gate are wired: the app check, then the grant check, which a process
    /// carrying the daemon's spawn mark fails even when the app check passed.
    #[test]
    fn the_gate_needs_the_app_and_a_person_not_a_process_the_daemon_started() {
        let (state, _home) = daemon();
        let mut person = child(false);
        let mut agent = child(true);
        let daemon: &DaemonState = &state;
        let caller = move |pid: Option<u32>| TerminalCaller {
            daemon,
            identity: None,
            peer: pid.map(|pid| (pid, 0)),
        };
        let app = App(Ok(()));
        assert_eq!(
            desktop_verdict(&app, &caller(Some(person.id())), grant),
            Ok(())
        );
        assert_eq!(
            desktop_verdict(&app, &caller(Some(agent.id())), grant),
            Err(NOT_A_PERSON.to_string())
        );
        // No peer: not the local socket.
        assert!(desktop_verdict(&app, &caller(None), grant).is_err());
        // The app check comes first, and its refusal is the answer.
        let asked = std::cell::Cell::new(false);
        let refused = desktop_verdict(
            &App(Err("not the app")),
            &caller(Some(person.id())),
            |_, _| {
                asked.set(true);
                true
            },
        );
        assert_eq!(refused, Err("not the app".to_string()));
        assert!(
            !asked.get(),
            "the grant check ran after the app check failed"
        );
        for process in [&mut person, &mut agent] {
            let _ = process.kill();
            let _ = process.wait();
        }
    }

    /// The production gate's own composition runs the grant check: with the app check passed, a
    /// real process carrying the spawn mark is still refused. Fails if `may_grant` is dropped
    /// from `DesktopGate::admit`.
    #[test]
    fn the_production_gate_runs_the_grant_check() {
        let (state, _home) = daemon();
        let mut agent = child(true);
        let later = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        let caller = TerminalCaller {
            daemon: &state,
            identity: None,
            peer: Some((agent.id(), later)),
        };
        testing::skip_app_check(true);
        let verdict = DesktopGate.admit(&caller);
        testing::skip_app_check(false);
        assert_eq!(verdict, Err(NOT_A_PERSON.to_string()));
        let _ = agent.kill();
        let _ = agent.wait();
    }

    /// The production gate refuses a connection that did not come in on the local socket
    /// (loopback TCP: a mod frame, a page, any token holder) before looking at any process.
    #[test]
    fn the_production_gate_refuses_loopback_tcp() {
        let (state, _home) = daemon();
        let caller = TerminalCaller {
            daemon: &state,
            identity: None,
            peer: None,
        };
        assert!(!caller.on_local_socket());
        let reason = DesktopGate.admit(&caller).unwrap_err();
        assert!(reason.contains("local socket"), "{reason}");
    }

    #[test]
    fn output_is_coalesced_in_chunks_and_a_connection_too_far_behind_lags() {
        let backlog = AtomicUsize::new(0);
        let pending = Pending::default();
        pending.push(
            Event::Data {
                seq: 10,
                bytes: &[b'a'; CHUNK],
            },
            &backlog,
        );
        pending.push(
            Event::Data {
                seq: 10 + CHUNK as u64,
                bytes: b"bc",
            },
            &backlog,
        );
        let first = pending.take(CHUNK);
        assert_eq!(
            first.data.as_ref().map(|(seq, bytes)| (*seq, bytes.len())),
            Some((10, CHUNK))
        );
        assert!(first.more);
        let second = pending.take(CHUNK);
        assert_eq!(second.data, Some((10 + CHUNK as u64, b"bc".to_vec())));
        assert!(!second.more && !second.lagged);
        // The replay an opener gets goes in front of what came after it.
        pending.push(
            Event::Data {
                seq: 7,
                bytes: b"z",
            },
            &backlog,
        );
        pending.prepend(4, b"xyw");
        assert_eq!(pending.take(CHUNK).data, Some((4, b"xywz".to_vec())));
        // A connection already 1 MiB behind: the output is dropped and it is told it lagged.
        backlog.store(LAG_LIMIT, Ordering::SeqCst);
        pending.push(
            Event::Data {
                seq: 8,
                bytes: b"!",
            },
            &backlog,
        );
        let lagged = pending.take(CHUNK);
        assert!(lagged.lagged && lagged.data.is_none());
    }
}
