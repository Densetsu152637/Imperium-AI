//! Agents manage the app config (roadmap item 22): `set_model_card`, `install_extension` and
//! `uninstall_extension` change the user's config through the daemon from a session in an
//! unrelated folder; every change is journalled with where installs came from, `config.undo`
//! reverses it, and permission rules deny or ask before a change is made. Sources are local
//! folders and a local git repository; nothing touches the network.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// One tool call per request, in order; then a reply.
async fn fake_model(calls: Vec<(&'static str, Value)>) -> String {
    scripted_model(vec![("", calls)]).await
}

/// A model with one script of tool calls per prompt: a conversation whose first user message
/// contains a script's prompt follows that script (one call per request, then a reply).
async fn scripted_model(scripts: Vec<(&'static str, Vec<(&'static str, Value)>)>) -> String {
    let scripts = Arc::new(scripts);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let scripts = Arc::clone(&scripts);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let done = messages.iter().filter(|m| m["role"] == "tool").count();
                let prompt = messages
                    .iter()
                    .find(|m| m["role"] == "user")
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or_default()
                    .to_string();
                let calls = scripts
                    .iter()
                    .find(|(key, _)| prompt.contains(key))
                    .map(|(_, calls)| calls);
                let body = match calls.and_then(|calls| calls.get(done)) {
                    Some((name, arguments)) => sse(json!({"choices":[{"delta":{"tool_calls":[{
                        "index": 0, "id": format!("c{done}"),
                        "function": {"name": name, "arguments": arguments.to_string()}
                    }]}}]})),
                    None => sse(json!({"choices":[{"delta":{"content":"Done."}}]})),
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// An MCP server offering `lookup`; counts how often it is initialised (started).
async fn mcp_server(starts: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/mcp",
        post(move |axum::Json(request): axum::Json<Value>| {
            let starts = Arc::clone(&starts);
            async move {
                let Some(id) = request.get("id").cloned() else {
                    return axum::Json(json!({}));
                };
                let result = match request["method"].as_str() {
                    Some("initialize") => {
                        starts.fetch_add(1, Ordering::SeqCst);
                        json!({"protocolVersion": "2025-06-18", "capabilities": {}})
                    }
                    Some("tools/list") => json!({"tools": [
                        {"name": "lookup", "description": "Look up a word", "inputSchema": {"type": "object"}}]}),
                    _ => json!({"content": [{"type": "text", "text": "lookup ok"}]}),
                };
                axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

fn daemon(home: &Path, base_url: &str, extra: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [[models.catalog]]\nid = \"coder\"\nspec = \"fake:m\"\nlocation = \"cloud\"\n\
         tier = \"standard\"\n[workflow]\nenabled = false\n[memory]\nenabled = false\n{extra}"
    );
    // Written to the user config file, as a real daemon reads it, so saving a card rebuilds the
    // settings from the same layers.
    let paths = imperium_config::Paths::under(home);
    write(&paths.user_config_file(), &config);
    let loaded =
        imperium_config::load(&paths, None, std::iter::empty::<(String, String)>()).unwrap();
    let state = DaemonState::new(paths, loaded.settings().unwrap(), None);
    state.remember_config(&loaded);
    state
}

fn connect(state: &Arc<DaemonState>) -> Connection {
    let mut connection = Connection::new(Arc::clone(state));
    rpc(
        &mut connection,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 10}}),
    )
    .unwrap();
    connection
}

fn rpc(connection: &mut Connection, method: &str, params: Value) -> Result<Value, String> {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let Some(Response { outcome, .. }) = handled.response else {
        panic!("no immediate reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => Ok(value),
        Outcome::Error(error) => Err(error.message),
    }
}

/// What a turn did: each tool's (name, ok, output), and the permission requests it made,
/// answered with `answer`.
#[derive(Debug, Default)]
struct Turn {
    tools: Vec<(String, bool, String)>,
    asked: Vec<(String, String)>,
}

async fn run_turn(
    state: &Arc<DaemonState>,
    project: &Path,
    answer: PermissionDecision,
) -> (String, Turn) {
    run_prompt(state, project, answer, "manage the config").await
}

/// A turn in a new session that starts with `prompt`.
async fn run_prompt(
    state: &Arc<DaemonState>,
    project: &Path,
    answer: PermissionDecision,
    prompt: &str,
) -> (String, Turn) {
    let (session, mut events) = state
        .create_session(&project.display().to_string(), None)
        .unwrap();
    session.send(prompt.into()).unwrap();
    let mut turn = Turn::default();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::ToolFinished {
                    ok, output, tool, ..
                } => turn.tools.push((tool.unwrap_or_default(), ok, output)),
                SessionEvent::PermissionRequested {
                    request_id,
                    tool,
                    summary,
                    ..
                } => {
                    turn.asked.push((tool, summary));
                    session.respond(request_id, answer).unwrap();
                }
                SessionEvent::TurnCompleted { .. } => break,
                SessionEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 60 s");
    (session.id().to_string(), turn)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// A local git repository holding a skill; `None` when git is not installed.
fn skill_repo(dir: &Path) -> Option<(String, String)> {
    write(
        &dir.join("SKILL.md"),
        "---\nname: pdf-helper\ndescription: Reads PDF files.\n---\nUse pdftotext.\n",
    );
    git(dir, &["init", "--quiet"])?;
    git(dir, &["add", "."])?;
    git(dir, &["commit", "--quiet", "-m", "skill"])?;
    let head = git(dir, &["rev-parse", "HEAD"])?;
    let path = dir.display().to_string().replace('\\', "/");
    let url = if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    };
    Some((url, head))
}

fn change_ids(connection: &mut Connection) -> Vec<(u64, String)> {
    let listed = rpc(connection, "config.changes", json!({})).unwrap();
    listed["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| {
            (
                change["id"].as_u64().unwrap(),
                change["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn cards_installs_and_removals_are_recorded_with_their_source_and_undone() {
    let home = tempfile::tempdir().unwrap();
    // The session's folder has nothing to do with the config folder or the sources.
    let project = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let Some((url, head)) = skill_repo(&sources.path().join("skill-repo")) else {
        eprintln!("git is not installed; skipping");
        return;
    };
    let plugin = sources.path().join("hooked-plugin");
    write(
        &plugin.join(".claude-plugin/plugin.json"),
        r#"{"name": "hooked", "description": "Checks commands."}"#,
    );
    write(
        &plugin.join("hooks/hooks.json"),
        r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "check"}]}]}}"#,
    );
    let mod_dir = sources.path().join("bundle").join("clock");
    write(
        &mod_dir.join("mod.json"),
        r#"{"id": "agent-clock", "name": "Clock", "contributes": {"panels": [{"id": "clock", "title": "Clock"}]}}"#,
    );
    write(&mod_dir.join("index.js"), "export default 1;");

    let config_dir = imperium_config::Paths::under(home.path())
        .config_dir()
        .to_path_buf();
    // A skill the user put there by hand: removing it asks.
    write(
        &config_dir.join("skills/manual/SKILL.md"),
        "---\nname: manual\ndescription: Hand-made.\n---\nHi.\n",
    );

    let calls = vec![
        (
            "set_model_card",
            json!({"model": "coder", "card": "Good at: coding and tests."}),
        ),
        (
            "install_extension",
            json!({"kind": "skill", "source": url, "revision": "main"}),
        ),
        (
            "install_extension",
            json!({"kind": "plugin", "source": plugin.display().to_string()}),
        ),
        (
            "install_extension",
            json!({"kind": "mod", "source": sources.path().join("bundle").display().to_string(), "path": "clock"}),
        ),
        (
            "uninstall_extension",
            json!({"kind": "skill", "name": "manual"}),
        ),
    ];
    let url_model = fake_model(calls).await;
    let state = daemon(home.path(), &url_model, "");
    let (session_id, turn) = run_turn(&state, project.path(), PermissionDecision::Allow).await;
    assert_eq!(turn.tools.len(), 5, "{turn:?}");
    for (tool, ok, output) in &turn.tools {
        assert!(*ok, "{tool}: {output}");
    }
    // Only removing the hand-made skill asked.
    assert_eq!(turn.asked.len(), 1, "{:?}", turn.asked);
    assert_eq!(turn.asked[0].0, "uninstall_extension");

    // The card is in the app settings file and in effect.
    let models = std::fs::read_to_string(state.paths().app_models_file()).unwrap();
    assert!(models.contains("Good at: coding and tests."), "{models}");
    let row = state.settings().models.catalog[0].card.clone();
    assert_eq!(row.as_deref(), Some("Good at: coding and tests."));

    // Provenance: the git URL and commit, the folder, and the session.
    let skill_dir = config_dir.join("skills/pdf-helper");
    assert!(skill_dir.join("SKILL.md").is_file());
    assert!(!skill_dir.join(".git").exists());
    let mut connection = connect(&state);
    let listed = rpc(&mut connection, "mcp.list", json!({})).unwrap();
    let skill = listed["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["name"] == "pdf-helper")
        .expect("the installed skill is listed");
    assert_eq!(skill["installed_from"]["kind"], "git");
    assert_eq!(skill["installed_from"]["location"], url);
    assert_eq!(skill["installed_from"]["revision"], head);
    assert_eq!(skill["installed_from"]["session_id"], session_id);
    let hooked = listed["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "hooked")
        .expect("the installed plugin is listed");
    assert_eq!(hooked["installed_from"]["kind"], "path");
    // A plugin with hooks is installed turned off, so its hooks do not run unasked.
    assert_eq!(hooked["enabled"], false);
    assert!(
        state
            .settings()
            .extensions
            .disabled_plugins
            .contains(&"hooked".to_string())
    );
    let mods = rpc(&mut connection, "mods.list", json!({})).unwrap();
    let clock = mods["mods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "agent-clock")
        .expect("the installed mod is listed");
    assert_eq!(clock["installed_from"]["subdir"], "clock");
    assert_eq!(clock["installed_from"]["session_id"], session_id);
    assert!(!config_dir.join("skills/manual").exists());

    // The journal, newest first, and undo of each change.
    let changes = change_ids(&mut connection);
    let kinds: Vec<&str> = changes.iter().map(|(_, kind)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["uninstall", "install", "install", "install", "model_card"]
    );
    let listed = rpc(&mut connection, "config.changes", json!({})).unwrap();
    let card = &listed["changes"][4];
    assert_eq!(card["session_id"], session_id);
    assert_eq!(card["model"], "coder");
    assert_eq!(card["before"], Value::Null);
    assert_eq!(card["after"], "Good at: coding and tests.");
    assert_eq!(listed["changes"][3]["source"]["revision"], head);
    for (id, _) in &changes {
        rpc(&mut connection, "config.undo", json!({"id": id})).unwrap();
    }
    assert!(config_dir.join("skills/manual/SKILL.md").is_file());
    assert!(!skill_dir.exists());
    assert!(!config_dir.join("plugins/hooked").exists());
    assert!(!config_dir.join("mods/agent-clock").exists());
    assert!(
        !state
            .settings()
            .extensions
            .disabled_plugins
            .contains(&"hooked".to_string())
    );
    let models = std::fs::read_to_string(state.paths().app_models_file()).unwrap();
    assert!(!models.contains("Good at"), "{models}");
    assert_eq!(state.settings().models.catalog[0].card, None);
    let listed = rpc(&mut connection, "config.changes", json!({})).unwrap();
    assert!(
        listed["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|change| change["undone_at_unix_ms"].is_u64())
    );
    let again = rpc(&mut connection, "config.undo", json!({"id": changes[0].0}));
    assert!(again.unwrap_err().contains("already undone"));
}

#[tokio::test]
async fn permission_rules_deny_or_ask_before_an_agent_changes_the_config() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let skill = sources.path().join("notes");
    write(
        &skill.join("SKILL.md"),
        "---\nname: notes\ndescription: Takes notes.\n---\n",
    );
    let calls = vec![
        (
            "install_extension",
            json!({"kind": "skill", "source": skill.display().to_string()}),
        ),
        (
            "set_model_card",
            json!({"model": "coder", "card": "Good at: notes."}),
        ),
    ];
    let url = fake_model(calls).await;
    let rules = "[permissions]\nrules = [\n  { tool = \"install_extension\", action = \"deny\" },\n  \
                 { tool = \"set_model_card\", action = \"ask\" },\n]\n";
    let state = daemon(home.path(), &url, rules);
    let (_, turn) = run_turn(&state, project.path(), PermissionDecision::Deny).await;

    assert_eq!(turn.tools.len(), 2, "{turn:?}");
    let (tool, ok, output) = &turn.tools[0];
    assert_eq!(tool, "install_extension");
    assert!(!ok);
    assert!(output.contains("permission rule denies"), "{output}");
    // The ask rule asked, with the card in the request, and the user said no.
    assert_eq!(turn.asked.len(), 1, "{:?}", turn.asked);
    assert_eq!(turn.asked[0].0, "set_model_card");
    assert!(
        turn.asked[0].1.contains("Good at: notes."),
        "{:?}",
        turn.asked
    );
    assert!(!turn.tools[1].1);

    let config_dir: PathBuf = state.paths().config_dir().to_path_buf();
    assert!(!config_dir.join("skills/notes").exists());
    assert!(!state.paths().app_models_file().exists());
    let mut connection = connect(&state);
    assert!(change_ids(&mut connection).is_empty());
}

#[tokio::test]
async fn an_installed_plugin_with_a_server_stays_off_until_the_user_turns_it_on() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let sources = tempfile::tempdir().unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let mcp_url = mcp_server(Arc::clone(&starts)).await;
    // No hooks: only a server, which `activate_plugin` would start without asking anyone.
    let plugin = sources.path().join("words-plugin");
    write(
        &plugin.join(".claude-plugin/plugin.json"),
        r#"{"name": "words", "description": "Dictionary tools."}"#,
    );
    write(
        &plugin.join(".mcp.json"),
        &json!({"mcpServers": {"dict": {"type": "http", "url": mcp_url}}}).to_string(),
    );
    let url = scripted_model(vec![
        (
            "install words",
            vec![(
                "install_extension",
                json!({"kind": "plugin", "source": plugin.display().to_string()}),
            )],
        ),
        (
            "activate words",
            vec![("activate_plugin", json!({"name": "words"}))],
        ),
    ])
    .await;
    let state = daemon(home.path(), &url, "");
    let mut connection = connect(&state);
    let deny = PermissionDecision::Deny;

    // The install runs without asking, and leaves the plugin off.
    let (_, turn) = run_prompt(&state, project.path(), deny, "install words").await;
    assert!(turn.asked.is_empty(), "{:?}", turn.asked);
    let (tool, ok, output) = &turn.tools[0];
    assert_eq!(tool, "install_extension");
    assert!(*ok, "{output}");
    assert!(output.contains("turned off"), "{output}");
    let listed = rpc(&mut connection, "mcp.list", json!({})).unwrap();
    let words = listed["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "words")
        .expect("the installed plugin is listed");
    assert_eq!(words["enabled"], false);
    assert_eq!(words["servers"][0], "words_dict");
    assert!(words["installed_from"]["session_id"].is_string());

    // A later session cannot start its server.
    let (_, turn) = run_prompt(&state, project.path(), deny, "activate words").await;
    let (tool, ok, output) = &turn.tools[0];
    assert_eq!(tool, "activate_plugin");
    assert!(!*ok, "{output}");
    assert_eq!(starts.load(Ordering::SeqCst), 0, "the server was started");

    // Once the user turns it on in Connectors, a new session can activate it.
    rpc(
        &mut connection,
        "config.set",
        json!({"key": "extensions.disabled_plugins", "value": null}),
    )
    .unwrap();
    let (_, turn) = run_prompt(&state, project.path(), deny, "activate words").await;
    let (tool, ok, output) = &turn.tools[0];
    assert_eq!(tool, "activate_plugin");
    assert!(*ok, "{output}");
    assert!(output.contains("words_dict"), "{output}");
    assert_eq!(starts.load(Ordering::SeqCst), 1);
}
