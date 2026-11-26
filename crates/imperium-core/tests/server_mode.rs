//! Server mode (`daemon.server`, docs/02-architecture.md#server-mode-implemented): the desktop-only
//! features are refused with `-32010` and the feature's name, before any peer is looked at and
//! with nothing recorded; the handshake says so; the container tier is refused in the
//! configuration and at session start; approvals carried over in the data folder still count;
//! and SIGTERM runs the daemon's shutdown. Auto-merge's refusal is tested with the fake GitHub in
//! `pr_automerge.rs`, the nested-PID-namespace rule in `peer.rs` and `app_identity`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use imperium_config::{Config, Layer, Paths, Settings, Source};
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response, codes, server_features};
use serde_json::{Value, json};

fn layer(text: &str) -> Layer {
    Layer::new(Source::User("test".into()), text.parse().unwrap())
}

fn settings(extra: &str) -> Settings {
    Config::from_layers([
        Layer::defaults(),
        layer(&format!(
            "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"http://127.0.0.1:9/v1\"\n[workflow]\nenabled = false\n{extra}"
        )),
    ])
    .settings()
    .unwrap()
}

/// A daemon over a fresh home; `server` turns server mode on.
fn daemon(server: bool) -> (Arc<DaemonState>, tempfile::TempDir) {
    daemon_with(|settings| settings.daemon.server = server)
}

fn daemon_with(change: impl FnOnce(&mut Settings)) -> (Arc<DaemonState>, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let mut settings = settings("");
    change(&mut settings);
    let state = DaemonState::new(Paths::under(home.path()), settings, None);
    (state, home)
}

async fn call(connection: &mut Connection, method: &str, params: Value) -> Response {
    let frame = json!({"jsonrpc": "2.0", "id": 5, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    match (handled.response, handled.deferred) {
        (Some(response), _) => response,
        (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(20), deferred)
            .await
            .expect("an answer within 20 s"),
        (None, None) => panic!("no reply"),
    }
}

async fn connect(state: &Arc<DaemonState>) -> (Connection, Value) {
    let mut connection = Connection::new(Arc::clone(state));
    let handshake = json!({"client": {"name": "test", "version": "0"},
                           "protocol": imperium_protocol::PROTOCOL_VERSION});
    let shaken = result(call(&mut connection, "system.handshake", handshake).await);
    (connection, shaken)
}

fn result(response: Response) -> Value {
    match response.outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{error:?}"),
    }
}

/// Asserts `-32010` with `feature` in the error's data, and answers the message.
fn unavailable(response: Response, feature: &str) -> String {
    match response.outcome {
        Outcome::Error(error) => {
            assert_eq!(
                error.code,
                codes::UNAVAILABLE_ON_SERVER,
                "{}",
                error.message
            );
            assert_eq!(error.data, Some(json!({"feature": feature})));
            error.message
        }
        Outcome::Result(value) => panic!("expected -32010, got {value}"),
    }
}

/// A plugin with a command hook under `<config>/plugins/p`, which needs an approval to load.
fn plugin(home: &Path) -> std::path::PathBuf {
    let folder = Paths::under(home).config_dir().join("plugins/p");
    std::fs::create_dir_all(folder.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(folder.join("hooks")).unwrap();
    std::fs::write(folder.join(".claude-plugin/plugin.json"), r#"{"name":"p"}"#).unwrap();
    std::fs::write(
        folder.join("hooks/hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"true"}]}]}}"#,
    )
    .unwrap();
    folder
}

/// The `plugins.list` row of the plugin `p`.
async fn plugin_row(connection: &mut Connection) -> Value {
    let listed = result(call(connection, "plugins.list", json!({})).await);
    listed["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "p")
        .cloned()
        .expect("the plugin is listed")
}

#[test]
fn the_environment_turns_server_mode_on_whatever_config_toml_says() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::under(home.path());
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(paths.user_config_file(), "[daemon]\nserver = false\n").unwrap();
    let server = |env: Vec<(String, String)>| {
        imperium_config::load(&paths, None, env)
            .unwrap()
            .settings()
            .unwrap()
            .daemon
            .server
    };
    assert!(!server(Vec::new()));
    assert!(server(vec![(
        "IMPERIUM_DAEMON__SERVER".into(),
        "true".into()
    )]));
    // Off by default.
    assert!(!settings("").daemon.server);
}

#[test]
fn a_server_refuses_the_container_tier_in_its_configuration() {
    let with = |text: &str| Config::from_layers([Layer::defaults(), layer(text)]).settings();
    let error = with("[daemon]\nserver = true\n[isolation]\ntier = \"container\"")
        .unwrap_err()
        .to_string();
    assert!(error.contains("container"), "{error}");
    assert!(error.contains("server"), "{error}");
    // The other tiers, and the container tier off a server, are unchanged.
    with("[daemon]\nserver = true\n[isolation]\ntier = \"os\"").unwrap();
    with("[isolation]\ntier = \"container\"").unwrap();
}

#[tokio::test]
async fn the_handshake_says_whether_the_daemon_is_a_server() {
    let (state, _home) = daemon(true);
    let (_, shaken) = connect(&state).await;
    assert_eq!(shaken["server"], true, "{shaken}");
    let (state, _home) = daemon(false);
    let (_, shaken) = connect(&state).await;
    assert_eq!(shaken["server"], false, "{shaken}");
}

#[tokio::test]
async fn plugin_approvals_are_refused_on_a_server_and_record_nothing() {
    let (state, home) = daemon(true);
    plugin(home.path());
    let (mut connection, _) = connect(&state).await;
    let preview = result(
        call(
            &mut connection,
            "plugins.inspect_folder",
            json!({"name": "p"}),
        )
        .await,
    );
    let shown = json!({"name": "p", "sha256": preview["sha256"],
                       "runs_digest": preview["runs_digest"]});
    let message = unavailable(
        call(&mut connection, "plugins.approval_challenge", shown.clone()).await,
        server_features::PLUGIN_APPROVAL,
    );
    assert!(message.contains("server"), "{message}");
    let mut approve = shown;
    approve["challenge"] = json!("0".repeat(64));
    unavailable(
        call(&mut connection, "plugins.approve", approve).await,
        server_features::PLUGIN_APPROVAL,
    );
    // Nothing was recorded: the plugin is still withheld, with no approval.
    let row = plugin_row(&mut connection).await;
    assert_eq!(row["withheld"], true, "{row}");
    assert!(row["approval"].is_null(), "{row}");
}

/// An approval recorded on a desktop machine and carried over in the data folder is verified as
/// on any daemon (hash and run summary) and counts on a server: reading a record is not making
/// an approval.
#[tokio::test]
async fn an_approval_carried_over_in_the_data_folder_still_counts_on_a_server() {
    let (state, home) = daemon(true);
    let folder = plugin(home.path());
    let (mut connection, _) = connect(&state).await;
    let preview = result(
        call(
            &mut connection,
            "plugins.inspect_folder",
            json!({"name": "p"}),
        )
        .await,
    );
    let (hash, runs) = (preview["sha256"].clone(), preview["runs_digest"].clone());
    let key = folder
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let store = json!({"plugins": {key: {
        "name": "p", "origin": "preexisting", "hash": hash, "runs": runs,
        "approval": {"hash": hash, "runs": runs, "at_unix_ms": 1, "kind": "carried-over"}
    }}});
    let data = Paths::under(home.path()).data_dir().to_path_buf();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("plugin-installs.json"), store.to_string()).unwrap();
    let row = plugin_row(&mut connection).await;
    assert_eq!(row["withheld"], false, "{row}");
    assert_eq!(row["approval"], "carried-over", "{row}");
}

#[tokio::test]
async fn mod_grants_are_refused_on_a_server_before_any_peer_is_looked_at() {
    let (state, _home) = daemon(true);
    let source = tempfile::tempdir().unwrap();
    std::fs::write(
        source.path().join("mod.json"),
        r#"{"id": "grant-demo", "permissions": ["storage"], "main_context": true}"#,
    )
    .unwrap();
    std::fs::write(source.path().join("index.js"), "export default {};").unwrap();
    let location = source.path().display().to_string();
    // A local-socket peer: on a desktop the walk would be asked about it.
    let mut connection = Connection::new(Arc::clone(&state)).with_peer(Some((
        std::process::id(),
        imperium_api::time::unix_ms() / 1000,
    )));
    let handshake = json!({"client": {"name": "imperium", "version": "0"},
                           "protocol": imperium_protocol::PROTOCOL_VERSION});
    result(call(&mut connection, "system.handshake", handshake).await);
    let preview = result(call(&mut connection, "mods.inspect", json!({"source": location})).await);
    let params = |trusted: bool, interactive: bool| {
        json!({"source": location, "sha256": preview["sha256"], "permissions": ["storage"],
               "trusted": trusted, "interactive": interactive})
    };
    let message = unavailable(
        call(&mut connection, "mods.install", params(true, false)).await,
        server_features::TERMINAL_GRANT,
    );
    // The command-line client installs the mod in its sandbox instead on this prefix.
    assert!(
        message.starts_with(imperium_protocol::errors::TRUST_NOT_GRANTED),
        "{message}"
    );
    let installed = std::fs::read_dir(state.mods_dir()).map_or(0, |dir| dir.count());
    assert_eq!(installed, 0, "nothing is installed when trust is refused");
    // A prompt's approval is dropped with a note, and the mod installs off.
    let done = result(call(&mut connection, "mods.install", params(false, true)).await);
    assert_eq!(done["interactive"], false, "{done}");
    assert_eq!(done["trusted"], false, "{done}");
    let note = done["notes"][0].as_str().unwrap_or_default();
    assert!(note.contains("server"), "{done}");
}

#[tokio::test]
async fn the_container_tier_is_refused_at_session_start_and_in_config_set() {
    // Settings that skipped validation (an environment's own configuration narrowing them, say).
    let (state, _home) = daemon_with(|settings| {
        settings.daemon.server = true;
        settings.isolation.tier = "container".into();
    });
    let folder = tempfile::tempdir().unwrap();
    let (mut connection, _) = connect(&state).await;
    unavailable(
        call(
            &mut connection,
            "session.create",
            json!({"project_dir": folder.path().display().to_string()}),
        )
        .await,
        server_features::CONTAINER_TIER,
    );
    assert!(state.sessions().is_empty());

    let (state, _home) = daemon(true);
    let (mut connection, _) = connect(&state).await;
    unavailable(
        call(
            &mut connection,
            "config.set",
            json!({"key": "isolation.tier", "value": "container"}),
        )
        .await,
        server_features::CONTAINER_TIER,
    );
    // The policy tier starts as anywhere else.
    let created = result(
        call(
            &mut connection,
            "session.create",
            json!({"project_dir": folder.path().display().to_string()}),
        )
        .await,
    );
    let isolation = created["isolation"].as_str().unwrap_or_default();
    assert!(isolation.starts_with("tier 0"), "{created}");
}

/// Off a server nothing above changes: the same calls fail or pass as they did before.
#[tokio::test]
async fn without_server_mode_the_calls_are_judged_as_before() {
    let (state, home) = daemon(false);
    plugin(home.path());
    let (mut connection, _) = connect(&state).await;
    let response = call(
        &mut connection,
        "plugins.approval_challenge",
        json!({"name": "p", "sha256": "0", "runs_digest": "0"}),
    )
    .await;
    let Outcome::Error(error) = response.outcome else {
        panic!("an approval over a connection without a peer is refused");
    };
    assert_ne!(error.code, codes::UNAVAILABLE_ON_SERVER);
    assert!(error.message.contains("local socket"), "{}", error.message);
}

/// `docker stop` sends SIGTERM: the daemon runs the same shutdown as Ctrl+C, so its endpoint file
/// and its local socket are gone after it exits.
#[cfg(unix)]
#[test]
fn sigterm_runs_the_shutdown_and_removes_the_endpoint_and_the_socket() {
    use std::process::{Command, Stdio};
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::under(home.path());
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_imperium-core"))
        .env("IMPERIUM_HOME", home.path())
        .env("IMPERIUM_DAEMON__SERVER", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let file = paths.daemon_endpoint_file();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let endpoint = loop {
        if let Some(endpoint) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        {
            break endpoint;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon did not write its endpoint"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let socket = endpoint["socket"]
        .as_str()
        .map(std::path::PathBuf::from)
        .expect("the daemon serves a local socket");
    // The socket is bound before the endpoint is written.
    assert!(socket.exists(), "{}", socket.display());
    let status = Command::new("kill")
        .args(["-TERM", &daemon.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let exited = loop {
        if let Some(exited) = daemon.try_wait().unwrap() {
            break exited;
        }
        if std::time::Instant::now() > deadline {
            let _ = daemon.kill();
            let _ = daemon.wait();
            panic!("the daemon did not exit after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(exited.success(), "{exited:?}");
    assert!(!file.exists(), "daemon.json is removed");
    assert!(!socket.exists(), "the socket is removed");
}
