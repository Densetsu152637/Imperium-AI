//! An environment's first folder is the agent's working directory, and the agent can list and
//! read it however it spells the path: relative, absolute with either separator, as the system
//! prompt shows it, and in the looser spellings models write (quoted, padded, a JSON string with
//! unescaped Windows backslashes, a Git Bash or WSL path on Windows). The folder's path has
//! spaces in it, like `D:\Git Repositories\Imperium-AI`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use imperium_core::{Connection, DaemonState};
use imperium_protocol::jsonrpc::{Outcome, Response};
use imperium_protocol::session::SessionEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

/// One tool call per request, in order, with the arguments exactly as given; then a reply.
async fn fake_model(calls: Vec<(&'static str, String)>) -> String {
    let calls = Arc::new(calls);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let calls = Arc::clone(&calls);
            async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let done = messages.iter().filter(|m| m["role"] == "tool").count();
                let body = match calls.get(done) {
                    Some((name, arguments)) => sse(json!({"choices":[{"delta":{"tool_calls":[{
                        "index": 0, "id": format!("c{done}"),
                        "function": {"name": name, "arguments": arguments}
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

fn daemon(home: &std::path::Path, base_url: &str) -> Arc<DaemonState> {
    let config = format!(
        "[models]\ndefault = \"fake:m\"\n[endpoints.fake]\nbase_url = \"{base_url}\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false"
    );
    let settings = imperium_config::Config::from_layers([
        imperium_config::Layer::defaults(),
        imperium_config::Layer::new(
            imperium_config::Source::User("test".into()),
            config.parse().unwrap(),
        ),
    ])
    .settings()
    .unwrap();
    DaemonState::new(imperium_config::Paths::under(home), settings, None)
}

fn call(connection: &mut Connection, id: i64, method: &str, params: Value) -> Value {
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let handled = connection.handle_text(&frame.to_string());
    let Some(Response { outcome, .. }) = handled.response else {
        panic!("no immediate reply to {method}");
    };
    match outcome {
        Outcome::Result(value) => value,
        Outcome::Error(error) => panic!("{method}: {error:?}"),
    }
}

fn path_args(path: &str) -> String {
    json!({ "path": path }).to_string()
}

#[tokio::test]
async fn the_first_folder_of_an_environment_is_listable_and_readable_however_it_is_spelled() {
    let home = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let project = base.path().join("Git Repositories").join("Imperium AI");
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(
        project.join("docs").join("notes.md"),
        "inside the project\n",
    )
    .unwrap();
    std::fs::create_dir_all(project.join("notes")).unwrap();
    std::fs::write(
        project.join("notes").join("todo.md"),
        "inside the project\n",
    )
    .unwrap();
    let second = tempfile::tempdir().unwrap();

    let written = project.display().to_string();
    let forward = written.replace('\\', "/");
    let mut calls: Vec<(&'static str, String)> = vec![
        ("list_dir", path_args(".")),
        ("list_dir", path_args(&written)),
        ("list_dir", path_args(&forward)),
        (
            "list_dir",
            path_args(&format!("{written}{}", std::path::MAIN_SEPARATOR)),
        ),
        ("list_dir", path_args(&format!("\"{written}\""))),
        ("list_dir", path_args(&format!(" {written} "))),
        ("read_file", path_args("docs/notes.md")),
        ("read_file", path_args(&format!("{forward}/docs/notes.md"))),
    ];
    if cfg!(windows) {
        // A JSON string with the backslashes left unescaped, as small models write it.
        calls.push(("list_dir", format!("{{\"path\": \"{written}\"}}")));
        // Unescaped, `\notes\todo.md` reads as a newline and a tab: valid JSON, wrong path.
        calls.push((
            "read_file",
            format!("{{\"path\": \"{written}\\notes\\todo.md\"}}"),
        ));
        let drive = written[..1].to_lowercase();
        let rest = forward[2..].to_string();
        // Git Bash and WSL spellings of the same folder.
        calls.push(("list_dir", path_args(&format!("/{drive}{rest}"))));
        calls.push(("list_dir", path_args(&format!("/mnt/{drive}{rest}"))));
        calls.push(("list_dir", path_args(&format!("/{}:{rest}", &written[..1]))));
    }
    let count = calls.len();
    let url = fake_model(calls).await;

    let state = daemon(home.path(), &url);
    let mut connection = Connection::new(Arc::clone(&state));
    call(
        &mut connection,
        1,
        "system.handshake",
        json!({"client": {"name": "test", "version": "0"}, "protocol": {"major": 0, "minor": 2}}),
    );
    let env = call(
        &mut connection,
        2,
        "environments.create",
        json!({"name": "Spaced", "folders": [written, second.path()]}),
    );
    let created = call(
        &mut connection,
        3,
        "session.create",
        json!({"environment_id": env["id"]}),
    );
    let session = state
        .session(created["session_id"].as_str().unwrap())
        .unwrap();
    let mut events = session.subscribe();
    session.send("what can you see?".into()).unwrap();
    let mut results = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await.unwrap().event {
                SessionEvent::ToolFinished { ok, output, .. } => results.push((ok, output)),
                SessionEvent::TurnCompleted { .. } => break,
                SessionEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends within 30 s");

    assert_eq!(results.len(), count, "{results:?}");
    for (index, (ok, output)) in results.iter().enumerate() {
        assert!(*ok, "call {index} failed: {output}");
        assert!(
            output.contains("docs/") || output.contains("inside the project"),
            "call {index}: {output}"
        );
    }
}
