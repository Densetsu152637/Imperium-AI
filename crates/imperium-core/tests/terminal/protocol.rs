//! Stage 2b: `terminal.*` over the protocol, behind the desktop-only gate (decision D2).
//!
//! In-process connections stand in for clients: [`Connection::new`] is what loopback TCP and the
//! local socket give any holder of the daemon's token, a stub [`TerminalGate`] stands in for the
//! desktop app where a test needs one, and one test runs the real gate against a real child
//! process on the real local socket.

use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use imperium_core::{Connection, TerminalCaller, TerminalFrames, TerminalGate};
use imperium_protocol::jsonrpc::{Notification, Outcome};
use serde_json::{Value, json};

use super::*;

/// `permission denied`: what every caller but the desktop app gets.
const FORBIDDEN: i32 = -32005;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
/// A connection this far behind is told it lagged.
const LAG_LIMIT: u64 = 1024 * 1024;

/// A gate that answers `answer` and counts how often it was asked.
#[derive(Debug)]
struct Stub {
    answer: Result<(), String>,
    asked: AtomicUsize,
}

impl TerminalGate for Stub {
    fn admit(&self, _caller: &TerminalCaller<'_>) -> Result<(), String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

fn stub(yes: bool) -> Arc<Stub> {
    Arc::new(Stub {
        answer: if yes {
            Ok(())
        } else {
            Err("the stub says no".into())
        },
        asked: AtomicUsize::new(0),
    })
}

/// A connection that has shaken hands as `name`.
async fn connect(connection: Connection, name: &str) -> Connection {
    let mut connection = connection;
    let outcome = call(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": name, "version": "0"},
               "protocol": imperium_protocol::PROTOCOL_VERSION}),
    )
    .await;
    assert!(matches!(outcome, Outcome::Result(_)), "{outcome:?}");
    connection
}

/// A connection the stub gate lets in.
async fn admitted(state: &Arc<DaemonState>) -> Connection {
    connect(
        Connection::new(Arc::clone(state)).with_terminal_gate(stub(true)),
        "test",
    )
    .await
}

/// The outcome of one call, waiting for a reply that comes later.
async fn call(connection: &mut Connection, method: &str, params: Value) -> Outcome {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let response = match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(later)) => tokio::time::timeout(BOUND, later)
            .await
            .unwrap_or_else(|_| panic!("no reply to {method} in time")),
        (None, None) => panic!("no reply to {method}"),
    };
    response.outcome
}

async fn ok(connection: &mut Connection, method: &str, params: Value) -> Value {
    match call(connection, method, params).await {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method} failed: {error:?}"),
    }
}

fn code(outcome: &Outcome) -> Option<i32> {
    match outcome {
        Outcome::Error(error) => Some(error.code),
        Outcome::Result(_) => None,
    }
}

/// Every method the protocol defines whose name holds `terminal`, from the table the router is
/// checked against (`every_protocol_method_is_routed_to_a_handler`), not from a hand-kept list.
fn terminal_methods() -> Vec<String> {
    include_str!("../../../imperium-protocol/snapshots/methods.txt")
        .lines()
        .filter_map(|line| line.strip_prefix("method "))
        .filter(|name| name.contains("terminal"))
        .map(str::to_owned)
        .collect()
}

fn input(text: &str) -> String {
    B64.encode(format!("{text}{NEWLINE}"))
}

/// Valid params for each terminal method, in an order that works on one live terminal (close
/// last). A method the protocol gains fails here until it is given params and a place.
fn terminal_calls(session: &str, terminal: &str, marker: &str) -> Vec<(String, Value)> {
    let order = [
        "terminal.open",
        "terminal.list",
        "terminal.attach",
        "terminal.input",
        "terminal.resize",
        "terminal.detach",
        "terminal.close",
    ];
    let mut table = terminal_methods();
    table.sort();
    let mut ordered: Vec<String> = order.iter().map(|name| (*name).to_owned()).collect();
    ordered.sort();
    assert_eq!(
        table, ordered,
        "the protocol's terminal methods changed: give each params here"
    );
    order
        .iter()
        .map(|method| {
            let params = match *method {
                "terminal.open" => json!({"session_id": session, "cols": 80, "rows": 24}),
                "terminal.list" => json!({"session_id": session}),
                "terminal.attach" => json!({"terminal_id": terminal, "since": 0}),
                "terminal.input" => json!({"terminal_id": terminal, "data": input(&typed(marker))}),
                "terminal.resize" => json!({"terminal_id": terminal, "cols": 10, "rows": 10}),
                "terminal.detach" | "terminal.close" => json!({"terminal_id": terminal}),
                other => unreachable!("{other}"),
            };
            ((*method).to_owned(), params)
        })
        .collect()
}

/// Output and notifications read from one connection's terminal channel.
struct Watch {
    frames: TerminalFrames,
    bytes: Vec<u8>,
    notes: Vec<Notification>,
}

impl Watch {
    fn of(connection: &mut Connection) -> Self {
        Watch {
            frames: connection.terminal_frames().expect("the terminal channel"),
            bytes: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn drain(&mut self) {
        while let Some(note) = self.frames.try_recv() {
            if let Some(data) = note.params["data"].as_str() {
                self.bytes.extend(B64.decode(data).unwrap());
            }
            self.notes.push(note);
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    async fn wait_for(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + BOUND;
        loop {
            self.drain();
            let text = self.text();
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}: {text:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Opens a terminal over the protocol; its id. ConPTY's cursor queries are answered.
async fn open(connection: &mut Connection, state: &DaemonState, session: &str) -> String {
    let info = ok(
        connection,
        "terminal.open",
        json!({"session_id": session, "cols": 80, "rows": 24}),
    )
    .await;
    let id = info["terminal_id"].as_str().unwrap().to_owned();
    if let Some(terminal) = state.terminals().get(&id) {
        answer_cursor_queries(&terminal);
    }
    id
}

/// 2B-1. Whatever reads the daemon's endpoint file (an agent's command at tier 0, an MCP server,
/// a hook, a plugin, an `imperium` command an agent runs) holds the token and gets this
/// connection, over loopback TCP or the local socket. Every terminal method answers it
/// `-32005`, and nothing is opened, sized or typed. The same calls with the same params succeed
/// from a connection identified as the desktop app, so the refusals are the gate's, not bad
/// params.
pub(super) async fn a_client_with_the_daemon_token_cannot_open_or_type_into_a_terminal() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    let calls = terminal_calls(&session, terminal.id(), "AGENT");
    let mut token_holder = connect(Connection::new(Arc::clone(&fixture.state)), "agent").await;
    for (method, params) in &calls {
        let outcome = call(&mut token_holder, method, params.clone()).await;
        assert_eq!(code(&outcome), Some(FORBIDDEN), "{method}: {outcome:?}");
    }
    // A name the protocol does not define is refused by the gate too, before anything else.
    let outcome = call(&mut token_holder, "terminal.create", json!({})).await;
    assert_eq!(code(&outcome), Some(FORBIDDEN), "{outcome:?}");
    // Only the terminal opened in process exists, at its size, and nothing was typed into it.
    assert_eq!(fixture.state.terminals().ids(), [terminal.id()]);
    assert_eq!((terminal.info().cols, terminal.info().rows), (80, 24));
    terminal
        .pty()
        .write(format!("{}{NEWLINE}", typed("OWNER")).as_bytes())
        .unwrap();
    wait_for(&terminal, &evaluated("OWNER")).await;
    assert!(
        !output(&terminal).contains(&evaluated("AGENT")),
        "a caller typed into the terminal"
    );

    // The positive control: the desktop app, same calls, same params.
    let mut app = connect(
        Connection::new(Arc::clone(&fixture.state)).as_desktop_app_for_tests(),
        "imperium-desktop",
    )
    .await;
    for (method, params) in &calls {
        if method == "terminal.close" {
            // Checked before the close ends it: the input reached the shell.
            wait_for(&terminal, &evaluated("AGENT")).await;
            assert_eq!((terminal.info().cols, terminal.info().rows), (10, 10));
        }
        let outcome = call(&mut app, method, params.clone()).await;
        assert_eq!(code(&outcome), None, "{method}: {outcome:?}");
    }
    assert!(fixture.state.terminals().get(terminal.id()).is_none());
    // The app's own `terminal.open` made one more.
    assert_eq!(fixture.state.terminals().list(&session).len(), 1);
    fixture.state.stop_terminals(BOUND).await;
}

/// 2B-2. A token holder that calls itself the desktop app and takes a desktop lease is still
/// refused every terminal call: a lease is something anyone with the token can claim, and the
/// gate does not look at it.
pub(super) async fn a_token_holder_claiming_a_desktop_lease_is_refused() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    // As if on the local socket: a peer id, but not the app's identity.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut claimant = connect(
        Connection::new(Arc::clone(&fixture.state)).with_peer(Some((std::process::id(), now))),
        "imperium-desktop",
    )
    .await;
    let lease = ok(
        &mut claimant,
        "system.lease",
        json!({"kind": "desktop", "pid": std::process::id(), "label": "Imperium"}),
    )
    .await;
    assert!(lease["id"].is_string(), "{lease}");
    for (method, params) in terminal_calls(&session, terminal.id(), "CLAIMANT") {
        let outcome = call(&mut claimant, &method, params).await;
        assert_eq!(code(&outcome), Some(FORBIDDEN), "{method}: {outcome:?}");
    }
    assert_eq!(fixture.state.terminals().ids(), [terminal.id()]);
    fixture.state.stop_terminals(BOUND).await;
}

/// The gate is asked once per connection, and its answer stands for every later call.
pub(super) async fn the_gate_runs_once_per_connection() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let yes = stub(true);
    let mut admitted = connect(
        Connection::new(Arc::clone(&fixture.state)).with_terminal_gate(yes.clone()),
        "test",
    )
    .await;
    for _ in 0..3 {
        ok(
            &mut admitted,
            "terminal.list",
            json!({"session_id": session}),
        )
        .await;
    }
    open(&mut admitted, &fixture.state, &session).await;
    assert_eq!(yes.asked.load(Ordering::SeqCst), 1);

    let no = stub(false);
    let mut refused = connect(
        Connection::new(Arc::clone(&fixture.state)).with_terminal_gate(no.clone()),
        "test",
    )
    .await;
    for method in ["terminal.list", "terminal.open", "terminal.list"] {
        let outcome = call(
            &mut refused,
            method,
            json!({"session_id": session, "cols": 80, "rows": 24}),
        )
        .await;
        assert_eq!(code(&outcome), Some(FORBIDDEN), "{method}: {outcome:?}");
    }
    assert_eq!(no.asked.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.terminals().list(&session).len(), 1);
    fixture.state.stop_terminals(BOUND).await;
}

/// With the gate passed, `terminal.open` starts a shell in the workspace root and its output
/// reaches the opener; `terminal.resize` reaches the shell.
pub(super) async fn open_prints_the_workspace_root_and_a_resize_reaches_the_shell() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let mut app = admitted(&fixture.state).await;
    let mut watch = Watch::of(&mut app);
    let id = open(&mut app, &fixture.state, &session).await;
    let pwd = if cfg!(windows) { "cd" } else { "pwd" };
    ok(
        &mut app,
        "terminal.input",
        json!({"terminal_id": id, "data": input(pwd)}),
    )
    .await;
    let name = fixture
        .project
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    watch.wait_for(&name).await;
    ok(
        &mut app,
        "terminal.resize",
        json!({"terminal_id": id, "cols": 100, "rows": 30}),
    )
    .await;
    let listed = ok(&mut app, "terminal.list", json!({"session_id": session})).await;
    assert_eq!(listed["terminals"][0]["cols"], 100, "{listed}");
    assert_eq!(listed["terminals"][0]["state"], "running", "{listed}");
    #[cfg(unix)]
    {
        ok(
            &mut app,
            "terminal.input",
            json!({"terminal_id": id, "data": input("stty size")}),
        )
        .await;
        watch.wait_for("30 100").await;
    }
    #[cfg(windows)]
    {
        ok(
            &mut app,
            "terminal.input",
            json!({"terminal_id": id, "data": input("mode con")}),
        )
        .await;
        let text = watch.wait_for("Columns:").await;
        let columns = text
            .lines()
            .find(|line| line.contains("Columns:"))
            .unwrap_or("");
        assert!(columns.contains("100"), "{columns:?}");
    }
    // Closing reports the exit to every other attached connection.
    let mut viewer = admitted(&fixture.state).await;
    let mut viewer_watch = Watch::of(&mut viewer);
    ok(&mut viewer, "terminal.attach", json!({"terminal_id": id})).await;
    ok(&mut app, "terminal.close", json!({"terminal_id": id})).await;
    let deadline = Instant::now() + BOUND;
    loop {
        viewer_watch.drain();
        if let Some(changed) = viewer_watch
            .notes
            .iter()
            .find(|note| note.method == "terminal.changed")
        {
            assert_eq!(changed.params["info"]["state"], "exited", "{changed:?}");
            break;
        }
        assert!(Instant::now() < deadline, "no terminal.changed after close");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(fixture.state.terminals().ids().is_empty());
}

/// 2B-3. A connection that closes is detached from every terminal it was attached to, so the
/// hour-unattached rule ends them.
pub(super) async fn a_closed_connection_is_detached_and_the_hour_rule_applies() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let mut app = admitted(&fixture.state).await;
    let opened = open(&mut app, &fixture.state, &session).await;
    // One opened by another viewer, then attached by this one too.
    let other = fixture.open(&session).await;
    ok(
        &mut app,
        "terminal.attach",
        json!({"terminal_id": other.id()}),
    )
    .await;
    let later = Instant::now() + UNATTACHED_LIMIT + Duration::from_secs(60);
    assert!(fixture.state.end_unattached_terminals(later).is_empty());
    // The other viewer goes; this connection still holds both.
    fixture.state.detach_terminal(other.id()).unwrap();
    assert!(fixture.state.end_unattached_terminals(later).is_empty());
    drop(app);
    let later = Instant::now() + UNATTACHED_LIMIT + Duration::from_secs(60);
    let mut ended = fixture.state.end_unattached_terminals(later);
    ended.sort();
    let mut expected = vec![opened, other.id().to_owned()];
    expected.sort();
    assert_eq!(ended, expected);
    assert!(fixture.state.terminals().ids().is_empty());
}

/// Output reaches the attached connection only; a sentinel typed and echoed is in no journal,
/// no notification to another connection (one that asked for every topic, and one subscribed to
/// the session), no export, no search and no log line, captured at TRACE.
pub(super) async fn the_sentinel_stays_in_the_terminal() {
    const SENTINEL: &str = "SNTL7F3A";
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let mut announcements = fixture.state.announcements();
    let mut app = admitted(&fixture.state).await;
    let mut watch = Watch::of(&mut app);
    let mut other = admitted(&fixture.state).await;
    let mut other_watch = Watch::of(&mut other);
    ok(&mut other, "events.subscribe", json!({"topics": ["*"]})).await;
    let frame = json!({"jsonrpc": "2.0", "id": 2, "method": "session.subscribe",
                       "params": {"session_id": session}});
    let mut events = other
        .handle_text(&frame.to_string())
        .subscription
        .expect("a session subscription")
        .events;
    let id = open(&mut app, &fixture.state, &session).await;
    ok(
        &mut app,
        "terminal.input",
        json!({"terminal_id": id, "data": input(&typed(SENTINEL))}),
    )
    .await;
    watch.wait_for(&evaluated(SENTINEL)).await;
    // Give anything that would leak a moment to do so.
    tokio::time::sleep(Duration::from_millis(300)).await;

    other_watch.drain();
    assert!(other_watch.notes.is_empty(), "{:?}", other_watch.notes);
    while let Ok(note) = announcements.try_recv() {
        assert!(!note.method.starts_with("terminal."), "{}", note.method);
        let text = serde_json::to_string(&note).unwrap();
        assert!(!text.contains(SENTINEL), "{text}");
    }
    while let Ok(event) = events.try_recv() {
        let text = serde_json::to_string(&event).unwrap();
        assert!(!text.contains(SENTINEL), "{text}");
    }
    for format in ["markdown", "json"] {
        let exported = ok(
            &mut app,
            "session.export",
            json!({"session_id": session, "format": format}),
        )
        .await;
        assert!(!exported.to_string().contains(SENTINEL), "{exported}");
    }
    let found = ok(&mut app, "session.search", json!({"query": SENTINEL})).await;
    assert!(!found.to_string().contains(SENTINEL), "{found}");
    fixture.state.stop_terminals(BOUND).await;
    let mut files = Vec::new();
    files_under(fixture.home(), &mut files);
    assert!(!files.is_empty());
    for file in files {
        let bytes = std::fs::read(&file).unwrap_or_default();
        assert!(
            !bytes
                .windows(SENTINEL.len())
                .any(|part| part == SENTINEL.as_bytes()),
            "{} holds the sentinel",
            file.display()
        );
    }
    let log = captured_log();
    assert!(log.contains("opened a terminal"), "the log is captured");
    assert!(!log.contains(SENTINEL), "the log holds the sentinel");
}

fn files_under(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// Detach, then attach with `since`: exactly the missed bytes come back.
pub(super) async fn attach_with_since_replays_exactly_what_was_missed() {
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let terminal = fixture.open(&session).await;
    let mut viewer = admitted(&fixture.state).await;
    terminal
        .pty()
        .write(format!("{}{NEWLINE}", typed("FIRST")).as_bytes())
        .unwrap();
    wait_for(&terminal, &evaluated("FIRST")).await;
    let attached = ok(
        &mut viewer,
        "terminal.attach",
        json!({"terminal_id": terminal.id()}),
    )
    .await;
    assert_eq!(attached["seq"], 0);
    let had = B64.decode(attached["replay"].as_str().unwrap()).unwrap();
    let end = attached["seq"].as_u64().unwrap() + had.len() as u64;
    ok(
        &mut viewer,
        "terminal.detach",
        json!({"terminal_id": terminal.id()}),
    )
    .await;
    // Detaching twice is an error: it was not attached any more.
    let again = call(
        &mut viewer,
        "terminal.detach",
        json!({"terminal_id": terminal.id()}),
    )
    .await;
    assert!(code(&again).is_some());
    terminal
        .pty()
        .write(format!("{}{NEWLINE}", typed("MISSED")).as_bytes())
        .unwrap();
    wait_for(&terminal, &evaluated("MISSED")).await;
    let back = ok(
        &mut viewer,
        "terminal.attach",
        json!({"terminal_id": terminal.id(), "since": end}),
    )
    .await;
    assert_eq!(back["seq"].as_u64(), Some(end));
    assert_eq!(back["truncated"], false);
    let missed = B64.decode(back["replay"].as_str().unwrap()).unwrap();
    let ring = terminal.pty().replay(end).data;
    assert_eq!(missed, ring[..missed.len()]);
    assert!(String::from_utf8_lossy(&missed).contains(&evaluated("MISSED")));
    assert!(!String::from_utf8_lossy(&missed).contains(&evaluated("FIRST")));
    fixture.state.stop_terminals(BOUND).await;
}

/// A connection that stops reading falls behind; past 1 MiB it is told it lagged and gets
/// nothing more, and attaching again with `since` brings it back.
pub(super) async fn a_lagging_connection_is_told_and_reattaches() {
    let fixture = fixture(&known_shell());
    let line = format!("{}\n", "0123456789".repeat(10));
    std::fs::write(fixture.project.path().join("big.txt"), line.repeat(30_000)).unwrap();
    let session = fixture.session();
    let mut app = admitted(&fixture.state).await;
    let mut watch = Watch::of(&mut app);
    let id = open(&mut app, &fixture.state, &session).await;
    let terminal = fixture.state.terminals().get(&id).unwrap();
    let show = if cfg!(windows) {
        "type big.txt"
    } else {
        "cat big.txt"
    };
    ok(
        &mut app,
        "terminal.input",
        json!({"terminal_id": id, "data": input(show)}),
    )
    .await;
    // Nothing is read meanwhile, so the connection falls behind.
    let deadline = Instant::now() + Duration::from_secs(90);
    while terminal.pty().end_seq() < 2 * LAG_LIMIT + LAG_LIMIT / 2 {
        assert!(
            Instant::now() < deadline,
            "only {} bytes of output",
            terminal.pty().end_seq()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    watch.drain();
    let lagged = watch
        .notes
        .iter()
        .position(|note| note.params["lagged"] == true)
        .expect("a lagged notification");
    assert!(
        watch.notes[lagged + 1..]
            .iter()
            .all(|note| note.params["terminal_id"] != id.as_str()),
        "output after lagged"
    );
    let got = watch.bytes.len() as u64;
    assert!(got <= LAG_LIMIT + 64 * 1024, "{got} bytes before lagging");
    // Back from where it stopped (the ring has moved on: truncated).
    let back = ok(
        &mut app,
        "terminal.attach",
        json!({"terminal_id": id, "since": got}),
    )
    .await;
    assert_eq!(back["truncated"], true, "{}", back["seq"]);
    tokio::time::sleep(Duration::from_millis(500)).await;
    ok(
        &mut app,
        "terminal.input",
        json!({"terminal_id": id, "data": input(&typed("RESUMED"))}),
    )
    .await;
    watch.bytes.clear();
    watch.wait_for(&evaluated("RESUMED")).await;
    fixture.state.stop_terminals(BOUND).await;
}

/// M1: a session archived while a terminal opens, and a daemon stopping while one opens, leave
/// no terminal behind, however the two interleave.
pub(super) async fn opening_while_archiving_or_stopping_leaves_nothing() {
    let fixture = fixture(&known_shell());
    let state = &fixture.state;
    for step in 0..8u64 {
        let session = fixture.session();
        let delay = Duration::from_millis(step * 15);
        let (opened, archived) = tokio::join!(state.open_terminal(&session, 80, 24), async {
            tokio::time::sleep(delay).await;
            state.archive(&session)
        });
        archived.unwrap();
        assert!(
            state.terminals().list(&session).is_empty(),
            "step {step}: {opened:?}"
        );
        if let Ok(info) = opened {
            assert!(state.terminals().get(&info.terminal_id).is_none());
        }
    }
    assert!(state.terminals().ids().is_empty());
    let session = fixture.session();
    let (opened, ()) = tokio::join!(state.open_terminal(&session, 80, 24), async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        state.stop_terminals(BOUND).await;
    });
    assert!(state.terminals().ids().is_empty(), "{opened:?}");
    assert!(matches!(
        state.open_terminal(&session, 80, 24).await,
        Err(TerminalError::Stopping)
    ));
}

/// The real gate, the real local socket, a real child: a process carrying the daemon's spawn
/// mark (as an agent's command, a hook or a shell in a terminal does) that connects with the
/// token gets `-32005`.
pub(super) async fn a_real_child_with_the_spawn_mark_is_refused() {
    let data = tempfile::tempdir().unwrap();
    let listener = imperium_ipc::Listener::bind(data.path()).unwrap();
    let address = listener.address();
    let fixture = fixture(&known_shell());
    let session = fixture.session();
    let token = "terminal-gate-token";
    let server = tokio::spawn(imperium_core::serve_ipc(
        listener,
        imperium_core::AccessToken::from(token.to_string()),
        Arc::clone(&fixture.state),
        std::future::pending(),
    ));
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            HELPER,
            &address,
            token,
            &std::process::id().to_string(),
            &session,
        ])
        .env(imperium_tools::SPAWNED_MARK, "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(60), output)
        .await
        .expect("the helper finished")
        .unwrap();
    server.abort();
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{printed}");
    assert!(printed.contains("handshake ok"), "{printed}");
    for method in ["terminal.open", "terminal.list"] {
        assert!(
            printed.contains(&format!("{method} {FORBIDDEN}")),
            "{printed}"
        );
    }
    assert!(fixture.state.terminals().ids().is_empty());
}

/// The helper's argument.
pub(super) const HELPER: &str = "--terminal-gate-helper";

/// The helper process of [`a_real_child_with_the_spawn_mark_is_refused`]: connects to the
/// daemon's local socket with the token and prints each terminal call's error code.
pub(super) fn run_helper(args: &[String]) {
    let [address, token, daemon_pid, session] = args else {
        panic!("helper arguments: {args:?}");
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let stream = imperium_ipc::connect(address, daemon_pid.parse().ok())
            .await
            .unwrap();
        let mut request = "ws://localhost/rpc".into_client_request().unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap();
        let calls = [
            (
                "system.handshake",
                json!({"client": {"name": "imperium-desktop", "version": "0"},
                       "protocol": imperium_protocol::PROTOCOL_VERSION}),
            ),
            (
                "terminal.open",
                json!({"session_id": session, "cols": 80, "rows": 24}),
            ),
            ("terminal.list", json!({"session_id": session})),
        ];
        for (id, (method, params)) in (1u64..).zip(calls) {
            let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .unwrap();
            let reply = loop {
                let message = socket.next().await.unwrap().unwrap();
                let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                if value["id"] == id {
                    break value;
                }
            };
            if method == "system.handshake" {
                assert!(reply["result"].is_object(), "{reply}");
                println!("handshake ok");
            } else {
                println!("{method} {}", reply["error"]["code"]);
            }
        }
    });
}

/// The OS sandbox tier (bubblewrap, Seatbelt) hides the terminal's device from an agent's
/// command: it cannot open the pts the shell runs on, which the user's own process can. Fails if
/// it can (see the plan's residual risks); skips only where the sandbox cannot run and CI does
/// not require it.
pub(super) async fn a_command_at_the_os_sandbox_tier_cannot_open_the_terminal_device() {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let required = std::env::var("IMPERIUM_REQUIRE_OS_SANDBOX")
            .is_ok_and(|value| !matches!(value.as_str(), "" | "0"));
        let fixture = fixture(&known_shell());
        let session = fixture.session();
        let terminal = fixture.open(&session).await;
        terminal.pty().write(b"tty\n").unwrap();
        let mut device = None;
        wait_until("the terminal's device", || {
            device = output(&terminal)
                .split_whitespace()
                .find(|word| word.starts_with("/dev/"))
                .map(str::to_owned);
            device.is_some()
        })
        .await;
        let device = device.unwrap();
        // Reading would take the user's keystrokes; writing would show text or, through
        // TIOCSTI where the kernel allows it, type.
        let probe = format!(
            "( exec 3<{device} ) 2>/dev/null && echo READ-OPENED || echo READ-REFUSED; \
             ( exec 3>{device} ) 2>/dev/null && echo WRITE-OPENED || echo WRITE-REFUSED"
        );
        // The control: the user's own process opens it both ways.
        let own = std::process::Command::new("/bin/sh")
            .args(["-c", &probe])
            .output()
            .unwrap();
        let own = String::from_utf8_lossy(&own.stdout);
        assert!(
            own.contains("READ-OPENED") && own.contains("WRITE-OPENED"),
            "{device} cannot be opened outside the sandbox either: {own:?}"
        );
        let root = fixture.project.path();
        let run = |command: String| async move {
            let mut process = imperium_tools::Sandbox::Os { network: true }
                .command(&[root], &[], &command)
                .ok()?;
            process
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            let output = tokio::time::timeout(Duration::from_secs(60), process.output())
                .await
                .ok()?
                .ok()?;
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        match run("echo SANDBOX-RUNS".to_owned()).await {
            Some(text) if text.contains("SANDBOX-RUNS") => {}
            other => {
                assert!(
                    !required,
                    "the OS sandbox is required here but did not run: {other:?}"
                );
                eprintln!("skipping: the OS sandbox cannot run here ({other:?})");
                fixture.state.stop_terminals(BOUND).await;
                return;
            }
        }
        let inside = run(probe).await.unwrap_or_default();
        assert!(
            inside.contains("READ-REFUSED")
                && inside.contains("WRITE-REFUSED")
                && !inside.contains("OPENED"),
            "a command at the OS sandbox tier opened {device}: {inside:?}"
        );
        fixture.state.stop_terminals(BOUND).await;
    }
}
